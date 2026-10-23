//! Software "frosted glass": shrink, box-blur a few times, lift the colours a little. Works on plain RGBA pixels,
//! so it needs nothing from the window system.

const FACTOR: usize = 4;

/// Blurs `rgba` (w x h) with `passes` box blurs of the given radius (edges are clamped). Three passes approximate a Gaussian.
pub fn box_blur(src: &[u8], w: usize, h: usize, radius: usize, passes: usize) -> Vec<u8> {
    assert_eq!(src.len(), w * h * 4);
    let mut cur = src.to_vec();
    let mut tmp = vec![0u8; cur.len()];
    let span = (2 * radius + 1) as u32;
    for _ in 0..passes {
        for y in 0..h {                                   // horizontal
            for x in 0..w {
                let mut acc = [0u32; 4];
                for k in 0..=2 * radius {
                    let sx = (x + k).saturating_sub(radius).min(w - 1);
                    let i = (y * w + sx) * 4;
                    for c in 0..4 { acc[c] += cur[i + c] as u32; }
                }
                for c in 0..4 { tmp[(y * w + x) * 4 + c] = (acc[c] / span) as u8; }
            }
        }
        for y in 0..h {                                   // vertical
            for x in 0..w {
                let mut acc = [0u32; 4];
                for k in 0..=2 * radius {
                    let sy = (y + k).saturating_sub(radius).min(h - 1);
                    let i = (sy * w + x) * 4;
                    for c in 0..4 { acc[c] += tmp[i + c] as u32; }
                }
                for c in 0..4 { cur[(y * w + x) * 4 + c] = (acc[c] / span) as u8; }
            }
        }
    }
    cur
}

/// A frosted, quarter-size copy of the picture. Draw it stretched back up with smooth filtering.
pub fn frosted(rgba: &[u8], w: usize, h: usize) -> (usize, usize, Vec<u8>) {
    assert_eq!(rgba.len(), w * h * 4);
    let (sw, sh) = ((w / FACTOR).max(1), (h / FACTOR).max(1));
    let mut small = vec![0u8; sw * sh * 4];
    for y in 0..sh {
        for x in 0..sw {
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for dy in 0..FACTOR {
                for dx in 0..FACTOR {
                    let i = ((y * FACTOR + dy).min(h - 1) * w + (x * FACTOR + dx).min(w - 1)) * 4;
                    for c in 0..4 { acc[c] += rgba[i + c] as u32; }
                    n += 1;
                }
            }
            for c in 0..4 { small[(y * sw + x) * 4 + c] = (acc[c] / n) as u8; }
        }
    }
    let mut out = box_blur(&small, sw, sh, 4, 3);
    for p in out.chunks_exact_mut(4) {                     // slightly richer colours, like macOS vibrancy
        let l = 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
        for c in 0..3 { p[c] = (l + (p[c] as f32 - l) * 1.18).clamp(0.0, 255.0) as u8; }
    }
    (sw, sh, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, px: [u8; 4]) -> Vec<u8> { (0..w * h).flat_map(|_| px).collect() }

    #[test]
    fn a_flat_picture_stays_flat() {
        let img = solid(20, 12, [90, 140, 200, 255]);
        assert_eq!(box_blur(&img, 20, 12, 3, 3), img);
    }

    #[test]
    fn a_bright_dot_spreads_out_but_does_not_vanish_or_grow() {
        let (w, h) = (31, 31);
        let mut img = solid(w, h, [0, 0, 0, 255]);
        let centre = (15 * w + 15) * 4;
        img[centre..centre + 3].copy_from_slice(&[255, 255, 255]);
        let out = box_blur(&img, w, h, 2, 1);
        assert!(out[centre] < 255 && out[centre] > 0);             // softened
        assert!(out[(15 * w + 17) * 4] > 0);                        // neighbours picked it up
        assert_eq!(out[(15 * w + 25) * 4], 0);                      // far away stays black
        let total: u32 = out.chunks_exact(4).map(|p| p[0] as u32).sum();
        assert!((total as i32 - 255).abs() < 40, "energy roughly conserved, got {total}");
    }

    #[test]
    fn frosting_makes_a_quarter_size_smooth_copy_and_keeps_grays_gray() {
        let img = solid(100, 60, [128, 128, 128, 255]);
        let (sw, sh, out) = frosted(&img, 100, 60);
        assert_eq!((sw, sh), (25, 15));
        assert!(out.chunks_exact(4).all(|p| p[0] == p[1] && p[1] == p[2] && (p[0] as i32 - 128).abs() <= 1));
        assert!(out.chunks_exact(4).all(|p| p[3] == 255));
    }

    #[test]
    fn tiny_pictures_do_not_panic() {
        let img = solid(2, 2, [10, 20, 30, 255]);
        let (sw, sh, _) = frosted(&img, 2, 2);
        assert_eq!((sw, sh), (1, 1));
    }
}
