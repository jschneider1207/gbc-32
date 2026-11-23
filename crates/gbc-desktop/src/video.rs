//! Scaling and letterboxing the 160x144 frame into a window buffer.

use gbc_core::{Frame, SCREEN_HEIGHT, SCREEN_WIDTH, rgb555_to_888};

/// Where the scaled frame sits in a window.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct Layout {
    pub scale: usize,
    pub x: usize,
    pub y: usize,
}

/// The largest integer scale that fits `w` x `h` (never 0: a window
/// smaller than the frame still gets scale 1, clipped at the right and
/// bottom), centered.
pub fn layout(w: usize, h: usize) -> Layout {
    let scale = (w / SCREEN_WIDTH).min(h / SCREEN_HEIGHT).max(1);
    Layout {
        scale,
        x: w.saturating_sub(SCREEN_WIDTH * scale) / 2,
        y: h.saturating_sub(SCREEN_HEIGHT * scale) / 2,
    }
}

/// RGB555 to softbuffer's 0RGB.
pub fn pixel_0rgb(p: u16) -> u32 {
    let [r, g, b] = rgb555_to_888(p);
    u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
}

/// Draws `frame` into `buf` (`w` x `h`, row-major): black letterbox,
/// nearest-neighbor integer scale, clipped to the buffer.
pub fn blit(frame: &Frame, buf: &mut [u32], w: usize, h: usize) {
    buf.fill(0);
    if w == 0 || h == 0 || buf.len() < w * h {
        return;
    }
    let l = layout(w, h);
    let out_w = (SCREEN_WIDTH * l.scale).min(w);
    let mut row = vec![0u32; SCREEN_WIDTH * l.scale];
    for sy in 0..SCREEN_HEIGHT {
        for sx in 0..SCREEN_WIDTH {
            let c = pixel_0rgb(frame[sy * SCREEN_WIDTH + sx]);
            row[sx * l.scale..(sx + 1) * l.scale].fill(c);
        }
        for k in 0..l.scale {
            let y = l.y + sy * l.scale + k;
            if y >= h {
                return;
            }
            buf[y * w + l.x..y * w + l.x + out_w].copy_from_slice(&row[..out_w]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_fit_has_no_border() {
        assert_eq!(
            layout(640, 576),
            Layout {
                scale: 4,
                x: 0,
                y: 0
            }
        );
    }

    #[test]
    fn the_smaller_axis_limits_the_scale_and_the_rest_is_centered() {
        // 1000x600: height allows 4 (576), width 6.
        assert_eq!(
            layout(1000, 600),
            Layout {
                scale: 4,
                x: 180,
                y: 12
            }
        );
        // 700x1000: width allows 4.
        assert_eq!(
            layout(700, 1000),
            Layout {
                scale: 4,
                x: 30,
                y: 212
            }
        );
    }

    #[test]
    fn tiny_or_empty_windows_never_give_scale_zero() {
        for (w, h) in [(0, 0), (1, 1), (159, 143), (160, 0), (0, 144)] {
            let l = layout(w, h);
            assert!(l.scale >= 1, "{w}x{h}");
            assert_eq!((l.x, l.y), (0, 0), "{w}x{h}");
        }
        assert_eq!(
            layout(319, 287),
            Layout {
                scale: 1,
                x: 79,
                y: 71
            }
        );
    }

    #[test]
    fn pixel_conversion_matches_the_core() {
        assert_eq!(pixel_0rgb(0), 0x000000);
        assert_eq!(pixel_0rgb(0x7FFF), 0xFFFFFF);
        // RGB555 red is the low 5 bits.
        assert_eq!(pixel_0rgb(0x001F), 0xFF0000);
        assert_eq!(pixel_0rgb(0x7C00), 0x0000FF);
        assert_eq!(pixel_0rgb(0x03E0), 0x00FF00);
    }

    #[test]
    fn blit_scales_centers_and_letterboxes() {
        let mut f: Box<Frame> = Box::new([0; SCREEN_WIDTH * SCREEN_HEIGHT]);
        f[0] = 0x001F; // top-left red
        f[SCREEN_WIDTH * SCREEN_HEIGHT - 1] = 0x7C00; // bottom-right blue
        let (w, h) = (330, 300); // scale 2, x=5, y=6
        let mut buf = vec![0xABCDEFu32; w * h];
        blit(&f, &mut buf, w, h);
        assert_eq!(buf[0], 0, "letterbox is black");
        assert_eq!(buf[6 * w + 4], 0);
        for (x, y) in [(5, 6), (6, 6), (5, 7), (6, 7)] {
            assert_eq!(buf[y * w + x], 0xFF0000, "{x},{y}");
        }
        assert_eq!(buf[6 * w + 7], 0, "next source pixel is black");
        assert_eq!(buf[(6 + 287) * w + 5 + 319], 0x0000FF);
        assert_eq!(buf[(6 + 288) * w + 5], 0, "bottom letterbox");
    }

    #[test]
    fn blit_clips_a_tiny_window_without_panicking() {
        let f: Box<Frame> = Box::new([0x7FFF; SCREEN_WIDTH * SCREEN_HEIGHT]);
        let mut buf = vec![0u32; 10 * 10];
        blit(&f, &mut buf, 10, 10);
        assert!(buf.iter().all(|&p| p == 0xFFFFFF));
    }
}
