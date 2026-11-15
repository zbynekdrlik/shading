//! The panel's live state (fohmixer-ui's `store`): the cameras and the venue layout as the service
//! pushes them, the connection, ARRANGE and its note. One [`Store`] per page, provided as context.
//! `conn` holds the connection's pure rules, `net` the browser IO that feeds the store.

pub mod conn;
pub mod net;

use std::time::Duration;

use bkshading_proto::layout::{hidden_count, LayoutDoc};
use bkshading_proto::wire::{Aggregate, CameraView, ServerMsg};
use leptos::prelude::*;

use crate::behave::arrange_drag::{Arrange, Note, NoteTimer, NOTE_MS};

/// The page's state. Cheap to copy (signals).
#[derive(Clone, Copy)]
pub struct Store {
    /// The cameras of the last push, in the service's (config) order.
    pub cameras: RwSignal<Vec<CameraView>>,
    /// Whether any aggregate arrived yet (the empty-config note waits for one).
    pub heard: RwSignal<bool>,
    /// The venue's stored layout (rev 0 = number order until the service says otherwise).
    pub layout: RwSignal<LayoutDoc>,
    pub ws_open: RwSignal<bool>,
    /// The last contact with the service (a push or a successful poll), `Date.now()` ms.
    pub last_contact: RwSignal<f64>,
    /// The clock the banner and the holds follow, ticked by `net::start`.
    pub now: RwSignal<f64>,
    pub arrange: RwSignal<Arrange>,
    /// The note under the header (a result or a refusal), while it shows.
    pub note: RwSignal<Option<Note>>,
    note_timer: StoredValue<NoteTimer>,
    /// The camera whose #14 popup is open.
    pub info_for: RwSignal<Option<String>>,
}

impl Store {
    /// A fresh store at `now` (a fresh page gets the stale window as grace before the banner).
    pub fn new(now: f64) -> Self {
        Store {
            cameras: RwSignal::new(Vec::new()),
            heard: RwSignal::new(false),
            layout: RwSignal::new(LayoutDoc::default()),
            ws_open: RwSignal::new(false),
            last_contact: RwSignal::new(now),
            now: RwSignal::new(now),
            arrange: RwSignal::new(Arrange::Off),
            note: RwSignal::new(None),
            note_timer: StoredValue::new(NoteTimer::default()),
            info_for: RwSignal::new(None),
        }
    }

    /// Whether the panel has contact now (tracked).
    pub fn connected(&self) -> bool {
        conn::is_connected(self.ws_open.get(), self.last_contact.get(), self.now.get())
    }

    /// Whether the panel has contact now, untracked (the preview loader's tick).
    pub fn connected_untracked(&self) -> bool {
        conn::is_connected(
            self.ws_open.get_untracked(),
            self.last_contact.get_untracked(),
            net::now_ms(),
        )
    }

    /// The listed camera ids (tracked).
    pub fn camera_ids(&self) -> Vec<String> {
        self.cameras
            .with(|c| c.iter().map(|c| c.id.clone()).collect())
    }

    /// The camera `id` of the last push (tracked).
    pub fn camera(&self, id: &str) -> Option<CameraView> {
        self.cameras
            .with(|c| c.iter().find(|c| c.id == id).cloned())
    }

    /// The blocks to draw, each with whether it is a hidden camera's place in ARRANGE (tracked).
    pub fn blocks(&self) -> Vec<(String, bool)> {
        let cams = self.camera_ids();
        self.layout
            .with(|doc| self.arrange.with(|a| a.blocks(doc, &cams)))
    }

    /// How many listed cameras the stored layout hides (tracked).
    pub fn hidden_count(&self) -> usize {
        let cams = self.camera_ids();
        self.layout.with(|doc| hidden_count(doc, &cams))
    }

    /// Contact with the service at `now`.
    pub fn contact(&self, now: f64) {
        self.last_contact.set(now);
        self.now.set(now);
    }

    /// A message from the service (a push, a poll's answer, or the test hook), at `now`.
    pub fn apply(&self, msg: ServerMsg, now: f64) {
        match msg {
            ServerMsg::State(agg) => self.apply_state(agg),
            ServerMsg::Layout(doc) => self.apply_layout(doc),
        }
        self.contact(now);
    }

    fn apply_state(&self, agg: Aggregate) {
        if self.cameras.with_untracked(|c| *c != agg.cameras) {
            self.cameras.set(agg.cameras);
        }
        if !self.heard.get_untracked() {
            self.heard.set(true);
        }
    }

    /// The service's layout: shown at once; a save from another device ends ARRANGE unsaved.
    pub fn apply_layout(&self, doc: LayoutDoc) {
        let rev = doc.rev;
        if self.layout.with_untracked(|d| *d != doc) {
            leptos::logging::log!(
                "[layout] rev {rev}: order {:?} hidden {:?}",
                doc.order,
                doc.hidden
            );
            self.layout.set(doc);
        }
        if let Some(note) = self.arrange.try_update(|a| a.doc_arrived(rev)).flatten() {
            self.show_note(note);
        }
    }

    /// Shows `note` for [`NOTE_MS`] (a later note keeps its own full time).
    pub fn show_note(&self, note: Note) {
        self.note.set(Some(note));
        let mine = self
            .note_timer
            .try_update_value(|t| t.show())
            .unwrap_or_default();
        let store = *self;
        set_timeout(
            move || {
                let expired = store
                    .note_timer
                    .try_with_value(|t| t.expires(mine))
                    .unwrap_or(false);
                if expired {
                    store.note.set(None);
                }
            },
            Duration::from_millis(u64::from(NOTE_MS)),
        );
    }

    /// Shows `note` until the next note replaces it (the save's "Ukladám…").
    pub fn show_sticky_note(&self, note: Note) {
        self.note_timer.update_value(|t| {
            t.show();
        });
        self.note.set(Some(note));
    }
}
