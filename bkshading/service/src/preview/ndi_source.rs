//! Real NDI preview receiver (feature `ndi`, OFF by default).
//!
//! A MINIMAL `libloading` receiver that mirrors the appliance's verified `src/ndi.rs` recv
//! path — dynamic load of `libndi.so`, `find` the source by name, `recv_create_v3` at
//! **bandwidth LOWEST** (the NDI low-quality preview stream the owner specified), then
//! `recv_capture_v3` per frame. The FFI declarations are copied from `src/ndi.rs` (the
//! recv subset) so they match a live-verified layout rather than being reinvented; the pixel
//! math is delegated to the pure, unit-tested [`crate::preview::convert`] functions.
//!
//! This path is UNVERIFIED against a live cambox NDI source in this lane (there is no way to
//! exercise it on CI). It is gated OFF so the default build/CI uses the stub; end-to-end
//! verification + full FourCC coverage of the real low-bandwidth stream are the remaining
//! live rig-verify half (issue 1157).
//!
//! LIFECYCLE (issue 808, reconnect-safe): the loaded runtime is PROCESS-SHARED and load-once
//! ([`SharedRuntime`] keep-alive static below). The SDK's initialize/destroy pair is
//! application-lifetime and the destroy is process-GLOBAL — with a per-source runtime, one
//! camera's routine reconnect (the worker drops its source before every backoff) would tear
//! the SDK down under every other live receiver. Only the RECEIVER handle is per-source.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use libloading::Library;

use crate::preview::convert::{bgra_to_rgb, rgba_to_rgb, uyvy_to_rgb};
use crate::preview::frame::RawFrame;
use crate::preview::ndi_paths::{current_ndi_os, ndi_search_candidates};
use crate::preview::shared_runtime::SharedRuntime;
use crate::preview::source::PreviewSource;

// --- FFI layout (recv subset, copied verbatim from the appliance src/ndi.rs) --------------

#[allow(dead_code)] // FFI layout: some fields exist only for correct C struct size / are read by the SDK
#[repr(C)]
struct NDIlibFindCreateT {
    show_local_sources: bool,
    p_groups: *const c_char,
    p_extra_ips: *const c_char,
}

#[allow(dead_code)] // FFI layout: some fields exist only for correct C struct size / are read by the SDK
#[repr(C)]
#[derive(Clone, Copy)]
struct NDIlibSourceT {
    p_ndi_name: *const c_char,
    p_url_address: *const c_char,
}

#[allow(dead_code)] // FFI layout: some fields exist only for correct C struct size / are read by the SDK
#[repr(C)]
struct NDIlibRecvCreateV3T {
    source_to_connect_to: NDIlibSourceT,
    color_format: c_int,
    bandwidth: c_int,
    allow_video_fields: bool,
    p_ndi_recv_name: *const c_char,
}

#[allow(dead_code)] // FFI layout: some fields exist only for correct C struct size / are read by the SDK
#[repr(C)]
struct NDIlibVideoFrameV2RecvT {
    xres: c_int,
    yres: c_int,
    fourcc: u32,
    frame_rate_n: c_int,
    frame_rate_d: c_int,
    picture_aspect_ratio: f32,
    frame_format_type: c_int,
    timecode: i64,
    p_data: *mut u8,
    line_stride_in_bytes: c_int,
    p_metadata: *const c_char,
    timestamp: i64,
}

type FnInitialize = unsafe extern "C" fn() -> bool;
type FnDestroy = unsafe extern "C" fn();
type FnFindCreateV2 = unsafe extern "C" fn(*const NDIlibFindCreateT) -> *mut c_void;
type FnFindDestroy = unsafe extern "C" fn(*mut c_void);
type FnFindWaitForSources = unsafe extern "C" fn(*mut c_void, u32) -> bool;
type FnFindGetCurrentSources = unsafe extern "C" fn(*mut c_void, *mut u32) -> *const NDIlibSourceT;
type FnRecvCreateV3 = unsafe extern "C" fn(*const NDIlibRecvCreateV3T) -> *mut c_void;
type FnRecvDestroy = unsafe extern "C" fn(*mut c_void);
type FnRecvCaptureV3 = unsafe extern "C" fn(
    *mut c_void,
    *mut NDIlibVideoFrameV2RecvT,
    *mut c_void,
    *mut c_void,
    u32,
) -> c_int;
type FnRecvFreeVideoV2 = unsafe extern "C" fn(*mut c_void, *const NDIlibVideoFrameV2RecvT);

const FRAME_TYPE_VIDEO: c_int = 1;
// Request BGRX/BGRA = value 0 per vendor/distroav/lib/ndi/Processing.NDI.Recv.h
// (NDIlib_recv_color_format_BGRX_BGRA = 0; UYVY_BGRA would be 1). This is the correct, live-verified
// request (the BGRA/BGRX 4bpp path serves the JPEG preview); the actual FourCC is dispatched on
// capture, where UYVY is still handled defensively (see the FOURCC_UYVY arm below).
const COLOR_FORMAT_BGRX_BGRA: c_int = 0;
// The NDI low-quality preview stream (`NDIlib_recv_bandwidth_lowest`). src/ndi.rs uses 100
// (HIGHEST) for the full display; a shading preview only needs LOWEST.
const BANDWIDTH_LOWEST: c_int = 0;

const FOURCC_UYVY: u32 = u32::from_le_bytes(*b"UYVY");
const FOURCC_BGRA: u32 = u32::from_le_bytes(*b"BGRA");
const FOURCC_BGRX: u32 = u32::from_le_bytes(*b"BGRX");
const FOURCC_RGBA: u32 = u32::from_le_bytes(*b"RGBA");
const FOURCC_RGBX: u32 = u32::from_le_bytes(*b"RGBX");

// --- Loaded library -----------------------------------------------------------------------

struct NdiLib {
    _library: Library,
    destroy: FnDestroy,
    find_create_v2: FnFindCreateV2,
    find_destroy: FnFindDestroy,
    find_wait_for_sources: FnFindWaitForSources,
    find_get_current_sources: FnFindGetCurrentSources,
    recv_create_v3: FnRecvCreateV3,
    recv_destroy: FnRecvDestroy,
    recv_capture_v3: FnRecvCaptureV3,
    recv_free_video_v2: FnRecvFreeVideoV2,
}

// SAFETY: the NDI SDK is documented as safe for concurrent use across threads.
unsafe impl Send for NdiLib {}
unsafe impl Sync for NdiLib {}

/// The ONE process-wide NDI runtime slot (issue 808 reconnect-safe lifecycle): loaded +
/// initialized on the first connect, shared by every preview source, kept alive for the
/// process lifetime. The SDK destroy is process-GLOBAL, so a mid-flight release (e.g. one
/// camera's reconnect while others stream) must be structurally impossible.
static SHARED_NDI: SharedRuntime<NdiLib> = SharedRuntime::new();

impl NdiLib {
    /// The process-shared runtime: the first call loads (dlopen + SDK initialize), later
    /// calls return the SAME handle; a failed load is not cached, so the worker's backoff
    /// retry loads again.
    fn shared() -> Result<Arc<Self>> {
        SHARED_NDI.acquire(|| Self::load_uncached().map(Arc::new))
    }

    /// Load + initialize a FRESH runtime. Callers go through [`NdiLib::shared`] — a fresh
    /// per-source runtime is exactly the reconnect-destroy hazard the shared slot exists
    /// to prevent.
    fn load_uncached() -> Result<Self> {
        // Cross-platform ordered candidate library paths (env dirs, then per-OS well-known dirs,
        // then bare names for the dynamic-linker fallback). The DECISION lives in the pure,
        // CI-unit-tested `ndi_paths` module (issue 1157): the bkshading service ships to the
        // strih PC (Windows), where the runtime is Processing.NDI.Lib.x64.dll — never a `.so`.
        let candidates = ndi_search_candidates(current_ndi_os(), |k| std::env::var(k).ok());
        for path in &candidates {
            // A dir-joined candidate is only worth a load attempt if the file exists; a bare-name
            // candidate (empty parent) is handed straight to the loader (LD_LIBRARY_PATH /
            // Windows PATH / dyld search).
            let is_bare = path.parent().is_none_or(|p| p.as_os_str().is_empty());
            if is_bare || path.exists() {
                if let Ok(lib) = unsafe { Library::new(path) } {
                    return Self::init(lib);
                }
            }
        }
        anyhow::bail!(
            "NDI runtime not found (install the NDI SDK / NDI Tools, or set NDI_RUNTIME_DIR_V6); \
             tried {} candidate paths",
            candidates.len()
        )
    }

    fn init(lib: Library) -> Result<Self> {
        unsafe {
            let initialize = *lib
                .get::<FnInitialize>(b"NDIlib_initialize")
                .context("symbol NDIlib_initialize")?;
            let destroy = *lib
                .get::<FnDestroy>(b"NDIlib_destroy")
                .context("NDIlib_destroy")?;
            let find_create_v2 = *lib
                .get::<FnFindCreateV2>(b"NDIlib_find_create_v2")
                .context("NDIlib_find_create_v2")?;
            let find_destroy = *lib
                .get::<FnFindDestroy>(b"NDIlib_find_destroy")
                .context("NDIlib_find_destroy")?;
            let find_wait_for_sources = *lib
                .get::<FnFindWaitForSources>(b"NDIlib_find_wait_for_sources")
                .context("NDIlib_find_wait_for_sources")?;
            let find_get_current_sources = *lib
                .get::<FnFindGetCurrentSources>(b"NDIlib_find_get_current_sources")
                .context("NDIlib_find_get_current_sources")?;
            let recv_create_v3 = *lib
                .get::<FnRecvCreateV3>(b"NDIlib_recv_create_v3")
                .context("NDIlib_recv_create_v3")?;
            let recv_destroy = *lib
                .get::<FnRecvDestroy>(b"NDIlib_recv_destroy")
                .context("NDIlib_recv_destroy")?;
            let recv_capture_v3 = *lib
                .get::<FnRecvCaptureV3>(b"NDIlib_recv_capture_v3")
                .context("NDIlib_recv_capture_v3")?;
            let recv_free_video_v2 = *lib
                .get::<FnRecvFreeVideoV2>(b"NDIlib_recv_free_video_v2")
                .context("NDIlib_recv_free_video_v2")?;

            // fn pointers are Copy and now live in their own bindings (each `?` temporary
            // borrow of `lib` ended at its statement), so `lib` moves into `_library` with no
            // outstanding borrow (E0505).
            if !initialize() {
                anyhow::bail!("NDIlib_initialize returned false");
            }
            Ok(Self {
                _library: lib,
                destroy,
                find_create_v2,
                find_destroy,
                find_wait_for_sources,
                find_get_current_sources,
                recv_create_v3,
                recv_destroy,
                recv_capture_v3,
                recv_free_video_v2,
            })
        }
    }
}

impl Drop for NdiLib {
    // With the keep-alive SHARED_NDI slot holding an Arc for the process lifetime, this
    // never fires mid-flight (the SDK destroy is application-exit territory); it stays
    // correct for any future non-cached use.
    fn drop(&mut self) {
        unsafe { (self.destroy)() }
    }
}

// --- The preview source -------------------------------------------------------------------

/// A low-bandwidth NDI receiver presented as a [`PreviewSource`].
pub struct NdiPreviewSource {
    lib: Arc<NdiLib>,
    receiver: *mut c_void,
    name: String,
}

// SAFETY: the receiver handle is only touched from the single worker thread that owns it, and
// the NDI SDK is thread-safe; `NdiLib` is `Send`/`Sync`.
unsafe impl Send for NdiPreviewSource {}

impl NdiPreviewSource {
    /// Find `source_name` (substring match, e.g. `"CAM1 (usb)"`) and connect a LOWEST-bandwidth
    /// receiver. Bounded find (~5 s) so a missing source becomes an error the worker retries.
    pub fn connect(source_name: &str) -> Result<Self> {
        // Process-shared runtime (never a fresh per-connect load — see the module doc).
        let lib = NdiLib::shared()?;

        let find_create = NDIlibFindCreateT {
            show_local_sources: true,
            p_groups: ptr::null(),
            p_extra_ips: ptr::null(),
        };
        let finder = unsafe { (lib.find_create_v2)(&find_create) };
        if finder.is_null() {
            anyhow::bail!("NDIlib_find_create_v2 returned null");
        }

        let start = std::time::Instant::now();
        let mut found: Option<NDIlibSourceT> = None;
        while start.elapsed() < Duration::from_secs(5) {
            unsafe { (lib.find_wait_for_sources)(finder, 1000) };
            let mut n: u32 = 0;
            let sources = unsafe { (lib.find_get_current_sources)(finder, &mut n) };
            if !sources.is_null() {
                for i in 0..n {
                    let src = unsafe { *sources.add(i as usize) };
                    if src.p_ndi_name.is_null() {
                        continue;
                    }
                    let name = unsafe { CStr::from_ptr(src.p_ndi_name) }.to_string_lossy();
                    if name.contains(source_name) {
                        found = Some(src);
                        break;
                    }
                }
            }
            if found.is_some() {
                break;
            }
        }

        let Some(source) = found else {
            unsafe { (lib.find_destroy)(finder) };
            anyhow::bail!("NDI source '{source_name}' not found within 5s");
        };

        let recv_name = CString::new("bkshading-preview").context("recv name")?;
        let create = NDIlibRecvCreateV3T {
            source_to_connect_to: source,
            color_format: COLOR_FORMAT_BGRX_BGRA,
            bandwidth: BANDWIDTH_LOWEST,
            allow_video_fields: false,
            p_ndi_recv_name: recv_name.as_ptr(),
        };
        // Create the receiver BEFORE destroying the finder (the source pointers it holds are
        // owned by the finder until the receiver copies them).
        let receiver = unsafe { (lib.recv_create_v3)(&create) };
        unsafe { (lib.find_destroy)(finder) };
        if receiver.is_null() {
            anyhow::bail!("NDIlib_recv_create_v3 returned null");
        }

        Ok(Self {
            lib,
            receiver,
            name: source_name.to_string(),
        })
    }
}

impl PreviewSource for NdiPreviewSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<RawFrame>> {
        let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        let mut vf: NDIlibVideoFrameV2RecvT = unsafe { std::mem::zeroed() };
        let frame_type = unsafe {
            (self.lib.recv_capture_v3)(
                self.receiver,
                &mut vf,
                ptr::null_mut(),
                ptr::null_mut(),
                timeout_ms,
            )
        };
        if frame_type != FRAME_TYPE_VIDEO {
            return Ok(None); // timeout / audio / metadata / error — not our frame
        }

        let width = vf.xres.max(0) as usize;
        let height = vf.yres.max(0) as usize;
        let stride = vf.line_stride_in_bytes.max(0) as usize;
        let fourcc = vf.fourcc;

        // Copy the pixels out (the SDK reuses/free the buffer) BEFORE freeing the frame.
        let raw = if !vf.p_data.is_null() && stride > 0 && height > 0 && width > 0 {
            let len = stride * height;
            Some(unsafe { std::slice::from_raw_parts(vf.p_data, len).to_vec() })
        } else {
            None
        };
        unsafe { (self.lib.recv_free_video_v2)(self.receiver, &vf) };

        let Some(raw) = raw else {
            return Ok(None);
        };

        let bpp = match fourcc {
            FOURCC_UYVY => 2,
            FOURCC_BGRA | FOURCC_BGRX | FOURCC_RGBA | FOURCC_RGBX => 4,
            other => {
                tracing::warn!(
                    fourcc = other,
                    "unsupported NDI preview FourCC; skipping frame"
                );
                return Ok(None);
            }
        };

        // Strip any row padding so the pure converters see tightly-packed rows.
        let tight = tight_rows(&raw, stride, width * bpp, height);
        let rgb = match fourcc {
            FOURCC_UYVY => uyvy_to_rgb(&tight, width, height),
            FOURCC_BGRA | FOURCC_BGRX => bgra_to_rgb(&tight, width, height),
            FOURCC_RGBA | FOURCC_RGBX => rgba_to_rgb(&tight, width, height),
            _ => unreachable!("fourcc filtered above"),
        };

        Ok(Some(RawFrame::new(width as u32, height as u32, rgb)))
    }
}

impl Drop for NdiPreviewSource {
    fn drop(&mut self) {
        if !self.receiver.is_null() {
            unsafe { (self.lib.recv_destroy)(self.receiver) }
        }
    }
}

/// Copy `height` rows of `row_bytes` each out of a `stride`-padded buffer into a tight buffer.
fn tight_rows(data: &[u8], stride: usize, row_bytes: usize, height: usize) -> Vec<u8> {
    if stride == row_bytes {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(row_bytes * height);
    for row in 0..height {
        let start = row * stride;
        let end = start + row_bytes;
        if end <= data.len() {
            out.extend_from_slice(&data[start..end]);
        }
    }
    out
}
