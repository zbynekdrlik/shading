//! The venue's panel layout, stored by the service (shading#16): the order of the camera blocks and
//! which of them the panel hides. One JSON document per venue, edited in the panel's ARRANGE mode
//! (`PUT /api/layout`) and pushed to every panel over `/ws` after each save.
//!
//! It lives in its own file next to the config (`--layout-file`, by default
//! `bkshading-layout.json` beside `--config`), never in the TOML: camera-box seeds that file, and an
//! edit there would need a restart. Writes are atomic (a temporary file, then a rename). Without a
//! config file the document lives in memory only (logged loudly at start). The rules (revision
//! check, validation, number order) are `bkshading_proto::layout`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bkshading_proto::layout::{apply_put, LayoutDoc, LayoutPut, LayoutRefusal};
use tokio::sync::watch;

/// The default layout file name, next to the config file.
pub const DEFAULT_LAYOUT_FILE: &str = "bkshading-layout.json";

/// Where the layout file goes: `explicit` when given, else [`DEFAULT_LAYOUT_FILE`] in the config
/// file's directory, else `None` (no config file: memory only).
pub fn layout_path(explicit: Option<&Path>, config: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    let config = config?;
    let dir = config.parent().unwrap_or_else(|| Path::new(""));
    Some(dir.join(DEFAULT_LAYOUT_FILE))
}

/// Why a save did not land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// The panel's save was refused (`stale` / `invalid`).
    Refused(LayoutRefusal),
    /// The file write failed; the stored document is unchanged.
    Write(String),
}

/// Writes `data` to `path` atomically: a temporary file in the same directory, then a rename, so a
/// crash never leaves half a document.
pub fn atomic_write(path: &Path, data: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

/// Reads the document at `path`. No file = the default (rev 0: number order, nothing hidden). A
/// file that cannot be read or parsed is moved aside to `<file>.bad` (logged as an error) and the
/// default is used, so a broken layout never keeps the shading panel from starting; the next save
/// writes a fresh file.
pub fn load(path: &Path) -> LayoutDoc {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(file = %path.display(), "layout: no file yet, number order (rev 0)");
            return LayoutDoc::default();
        }
        Err(e) => {
            tracing::error!(file = %path.display(), error = %e, "layout: file not readable, using number order");
            return LayoutDoc::default();
        }
    };
    match serde_json::from_str::<LayoutDoc>(&text) {
        Ok(doc) => {
            tracing::info!(
                file = %path.display(),
                rev = doc.rev,
                order = ?doc.order,
                hidden = ?doc.hidden,
                "layout: loaded"
            );
            doc
        }
        Err(e) => {
            let mut bad = path.as_os_str().to_owned();
            bad.push(".bad");
            let moved = std::fs::rename(path, &bad);
            tracing::error!(
                file = %path.display(),
                error = %e,
                moved_to = %PathBuf::from(&bad).display(),
                moved_ok = moved.is_ok(),
                "layout: file is not a layout document, moved aside; using number order"
            );
            LayoutDoc::default()
        }
    }
}

/// The venue's stored layout: the current document, its file (if any) and the push channel every
/// `/ws` socket listens on. Cheap to clone.
#[derive(Clone)]
pub struct LayoutStore {
    inner: Arc<Inner>,
}

struct Inner {
    path: Option<PathBuf>,
    /// Held across the check, the file write and the publish, so two saves never interleave.
    doc: Mutex<LayoutDoc>,
    tx: watch::Sender<Arc<LayoutDoc>>,
    /// Kept so the channel always has a receiver (a `send` with none would be dropped).
    _rx: watch::Receiver<Arc<LayoutDoc>>,
}

impl LayoutStore {
    /// A store over `path` (loaded now), or in memory only when `path` is `None`.
    pub fn open(path: Option<PathBuf>) -> Self {
        let doc = match &path {
            Some(p) => load(p),
            None => {
                tracing::warn!(
                    "layout: no --config and no --layout-file, the panel layout lives in memory \
                     only and is lost at restart"
                );
                LayoutDoc::default()
            }
        };
        Self::with_doc(path, doc)
    }

    /// A store holding `doc` (tests; `open` after loading).
    pub fn with_doc(path: Option<PathBuf>, doc: LayoutDoc) -> Self {
        let (tx, rx) = watch::channel(Arc::new(doc.clone()));
        LayoutStore {
            inner: Arc::new(Inner {
                path,
                doc: Mutex::new(doc),
                tx,
                _rx: rx,
            }),
        }
    }

    /// The current document.
    pub fn current(&self) -> LayoutDoc {
        self.lock().clone()
    }

    /// A receiver of every saved document (the current one first).
    pub fn subscribe(&self) -> watch::Receiver<Arc<LayoutDoc>> {
        self.inner.tx.subscribe()
    }

    /// The file the document is written to, if any.
    pub fn path(&self) -> Option<&Path> {
        self.inner.path.as_deref()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LayoutDoc> {
        // A poisoned lock only means a panic elsewhere while holding it; the document itself is
        // always whole (it is replaced, never edited in place).
        self.inner.doc.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Saves `put` from `peer`: the new document (written, then published to every panel), or why
    /// not. Every outcome is logged with the peer, the revisions and the lists.
    pub fn save(&self, put: &LayoutPut, peer: &str) -> Result<LayoutDoc, SaveError> {
        let mut doc = self.lock();
        let next = match apply_put(&doc, put) {
            Ok(next) => next,
            Err(refusal) => {
                tracing::warn!(
                    peer,
                    refusal = refusal.text(),
                    current_rev = doc.rev,
                    put_rev = put.rev,
                    order = ?put.order,
                    hidden = ?put.hidden,
                    "layout: save refused"
                );
                return Err(SaveError::Refused(refusal));
            }
        };
        if let Some(path) = &self.inner.path {
            let text = serde_json::to_string_pretty(&next).map_err(|e| {
                tracing::error!(peer, error = %e, "layout: serialize failed");
                SaveError::Write(e.to_string())
            })?;
            if let Err(e) = atomic_write(path, &text) {
                tracing::error!(
                    peer,
                    file = %path.display(),
                    error = %e,
                    rev = next.rev,
                    "layout: write failed, the stored layout is unchanged"
                );
                return Err(SaveError::Write(e.to_string()));
            }
        }
        tracing::info!(
            peer,
            from_rev = doc.rev,
            to_rev = next.rev,
            order = ?next.order,
            hidden = ?next.hidden,
            file = %self.inner.path.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| "(memory)".into()),
            "layout: saved"
        );
        *doc = next.clone();
        let _ = self.inner.tx.send(Arc::new(next.clone()));
        Ok(next)
    }
}
