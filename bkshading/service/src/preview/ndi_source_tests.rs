//! The NDI capture glue driven by a fake SDK receiver (shading#3): the order of the lost-link
//! window, the usable check, keep, the copy and the free, with no libndi.

use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;

use super::*;

/// One scripted SDK return: a frame type and, for video, (fourcc, width, height, stride, data).
type Scripted = (c_int, Option<(u32, i32, i32, i32, Vec<u8>)>);

#[derive(Default)]
struct FakeRecv {
    script: VecDeque<Scripted>,
    /// Buffers handed out, kept alive until the fake drops (the SDK owns its buffers too).
    held: Vec<Vec<u8>>,
    log: Vec<&'static str>,
    waits: u32,
}

impl NdiRecv for FakeRecv {
    fn capture(&mut self, vf: &mut NDIlibVideoFrameV2RecvT, _timeout_ms: u32) -> c_int {
        self.log.push("capture");
        let (kind, frame) = self.script.pop_front().unwrap_or((FRAME_TYPE_NONE, None));
        if let Some((fourcc, w, h, stride, mut data)) = frame {
            vf.fourcc = fourcc;
            vf.xres = w;
            vf.yres = h;
            vf.line_stride_in_bytes = stride;
            vf.p_data = if data.is_empty() {
                ptr::null_mut()
            } else {
                data.as_mut_ptr()
            };
            self.held.push(data);
        }
        kind
    }

    fn free(&mut self, _vf: &NDIlibVideoFrameV2RecvT) {
        self.log.push("free");
    }

    fn wait(&mut self, _d: Duration) {
        self.waits += 1;
    }
}

fn state(clock: Rc<Cell<u64>>) -> RecvState {
    RecvState::new("CAM1 (usb)", Box::new(move || clock.get()))
}

fn bgra(w: i32, h: i32, stride: i32) -> Option<(u32, i32, i32, i32, Vec<u8>)> {
    let data = (0..(stride * h) as usize).map(|i| i as u8).collect();
    Some((FOURCC_BGRA, w, h, stride, data))
}

fn frees(recv: &FakeRecv) -> usize {
    recv.log.iter().filter(|e| **e == "free").count()
}

#[test]
fn an_unwanted_video_frame_is_freed_once_and_never_built() {
    let mut recv = FakeRecv::default();
    recv.script.push_back((FRAME_TYPE_VIDEO, bgra(4, 2, 16)));
    let mut st = state(Rc::new(Cell::new(0)));
    let mut asked = 0;
    let got = capture_frame(&mut recv, &mut st, 100, &mut || {
        asked += 1;
        false
    })
    .expect("no error");
    assert!(got.is_none());
    assert_eq!(asked, 1);
    assert_eq!(recv.log, ["capture", "free"]);
}

#[test]
fn a_kept_padded_frame_is_packed_converted_then_freed() {
    let mut recv = FakeRecv::default();
    // 2x1 BGRA with 4 bytes of row padding: stride 12 for 8 bytes of pixels.
    recv.script.push_back((FRAME_TYPE_VIDEO, bgra(2, 1, 12)));
    let mut st = state(Rc::new(Cell::new(0)));
    let frame = capture_frame(&mut recv, &mut st, 100, &mut || true)
        .expect("no error")
        .expect("a frame");
    assert!(frame.is_valid());
    // BGRA bytes 0..8 = B0 G1 R2 A3 | B4 G5 R6 A7 -> RGB 2,1,0 | 6,5,4.
    assert_eq!(frame.rgb, [2, 1, 0, 6, 5, 4]);
    assert_eq!(
        recv.log,
        ["capture", "free"],
        "copy before the free, freed once"
    );
}

#[test]
fn an_unusable_frame_is_freed_without_asking_keep() {
    let mut recv = FakeRecv::default();
    let nv12 = u32::from_le_bytes(*b"NV12");
    recv.script
        .push_back((FRAME_TYPE_VIDEO, Some((nv12, 4, 2, 4, vec![0; 12]))));
    recv.script
        .push_back((FRAME_TYPE_VIDEO, Some((FOURCC_BGRA, 4, 2, 16, Vec::new()))));
    let mut st = state(Rc::new(Cell::new(0)));
    for _ in 0..2 {
        let got =
            capture_frame(&mut recv, &mut st, 100, &mut || panic!("keep asked")).expect("no error");
        assert!(got.is_none());
    }
    assert_eq!(frees(&recv), 2);
}

#[test]
fn a_timeout_or_an_event_frees_nothing() {
    let mut recv = FakeRecv::default();
    recv.script.push_back((FRAME_TYPE_NONE, None));
    recv.script.push_back((100, None));
    let mut st = state(Rc::new(Cell::new(0)));
    for _ in 0..2 {
        assert!(
            capture_frame(&mut recv, &mut st, 100, &mut || panic!("keep asked"))
                .expect("no error")
                .is_none()
        );
    }
    assert_eq!(frees(&recv), 0);
}

#[test]
fn a_lost_link_waits_then_timeouts_end_it_in_a_rebuild() {
    let mut recv = FakeRecv::default();
    let clock = Rc::new(Cell::new(0));
    let mut st = state(clock.clone());
    recv.script.push_back((FRAME_TYPE_ERROR, None));
    assert!(capture_frame(&mut recv, &mut st, 100, &mut || true)
        .expect("first error waits")
        .is_none());
    assert_eq!(recv.waits, 1, "a lost link never spins");
    clock.set(LOST_REBUILD_AFTER_MS - 1);
    recv.script.push_back((FRAME_TYPE_NONE, None));
    assert!(capture_frame(&mut recv, &mut st, 100, &mut || true).is_ok());
    clock.set(LOST_REBUILD_AFTER_MS);
    recv.script.push_back((FRAME_TYPE_NONE, None));
    assert!(
        capture_frame(&mut recv, &mut st, 100, &mut || true).is_err(),
        "rebuild"
    );
    assert_eq!(frees(&recv), 0);
}

#[test]
fn a_video_frame_closes_the_lost_window() {
    let mut recv = FakeRecv::default();
    let clock = Rc::new(Cell::new(0));
    let mut st = state(clock.clone());
    recv.script.push_back((FRAME_TYPE_ERROR, None));
    capture_frame(&mut recv, &mut st, 100, &mut || false).expect("waits");
    clock.set(100);
    recv.script.push_back((FRAME_TYPE_VIDEO, bgra(4, 2, 16)));
    capture_frame(&mut recv, &mut st, 100, &mut || false).expect("video");
    clock.set(10 * LOST_REBUILD_AFTER_MS);
    recv.script.push_back((FRAME_TYPE_NONE, None));
    assert!(capture_frame(&mut recv, &mut st, 100, &mut || false).is_ok());
}
