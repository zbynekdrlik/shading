//! shading#34: NDI UYVY is video range (Y 16-235, Cb/Cr 16-240) and the SDK converts the
//! 640x360 proxy with BT.709 (measured live on strih-pp against the SDK's own BGRX frame).
//! The preview's `uyvy_to_rgb` must give the same RGB, or the operator judges shading on lifted
//! blacks and shifted colours.

use bkshading::preview::convert::uyvy_to_rgb;

/// One macropixel `U Y0 V Y1` with the same luma for both pixels.
fn px(y: u8, cb: u8, cr: u8) -> Vec<u8> {
    uyvy_to_rgb(&[cb, y, cr, y], 2, 1)
}

fn assert_near(got: &[u8], want: [u8; 3], what: &str) {
    for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
        let d = (*g as i16 - *w as i16).abs();
        assert!(d <= 1, "{what}: channel {i} got {got:?}, want {want:?}");
    }
}

#[test]
fn video_black_16_is_rgb_0_34() {
    let rgb = px(16, 128, 128);
    assert_eq!(rgb, vec![0, 0, 0, 0, 0, 0]);
}

#[test]
fn video_white_235_is_rgb_255_34() {
    let rgb = px(235, 128, 128);
    assert_eq!(rgb, vec![255, 255, 255, 255, 255, 255]);
}

#[test]
fn below_black_and_above_white_clamp_34() {
    assert_eq!(px(0, 128, 128), vec![0; 6]);
    assert_eq!(px(255, 128, 128), vec![255; 6]);
}

#[test]
fn mid_grey_stays_neutral_on_the_video_scale_34() {
    // Y 126 = (126-16)*255/219 = 128.1.
    let rgb = px(126, 128, 128);
    assert_eq!(rgb, vec![128; 6]);
}

#[test]
fn bt709_75_percent_colour_bars_34() {
    // ITU-R BT.709 75 % bars, 8-bit video range (Y, Cb, Cr) -> RGB 191 / 0.
    let bars: [(&str, (u8, u8, u8), [u8; 3]); 7] = [
        ("white", (180, 128, 128), [191, 191, 191]),
        ("yellow", (168, 44, 136), [191, 191, 0]),
        ("cyan", (145, 147, 44), [0, 191, 191]),
        ("green", (133, 63, 52), [0, 191, 0]),
        ("magenta", (63, 193, 204), [191, 0, 191]),
        ("red", (51, 109, 212), [191, 0, 0]),
        ("blue", (28, 212, 120), [0, 0, 191]),
    ];
    for (name, (y, cb, cr), want) in bars {
        let rgb = px(y, cb, cr);
        assert_near(&rgb[..3], want, name);
        assert_near(&rgb[3..], want, name);
    }
}

#[test]
fn both_pixels_of_a_macropixel_share_its_chroma_in_order_34() {
    // U Y0 V Y1: black then white, neutral chroma.
    let rgb = uyvy_to_rgb(&[128, 16, 128, 235], 2, 1);
    assert_eq!(rgb, vec![0, 0, 0, 255, 255, 255]);
}
