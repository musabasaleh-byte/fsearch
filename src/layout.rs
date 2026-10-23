//! Window scaling: a window designed at `design_width` points should cover about `fraction` of the screen width.

/// UI zoom factor so that a `design_width`-point window is `fraction` of the screen, within [min, max].
pub fn zoom_for(screen_width: f32, fraction: f32, design_width: f32, min: f32, max: f32) -> f32 {
    if screen_width <= 0.0 || design_width <= 0.0 { return 1.0; }
    (screen_width * fraction / design_width).clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::zoom_for;
    #[test]
    fn scales_with_the_screen_within_limits() {
        assert!((zoom_for(1920.0, 0.36, 780.0, 0.7, 1.4) - 0.886).abs() < 0.01);   // full-HD at 100%
        assert_eq!(zoom_for(1280.0, 0.36, 780.0, 0.7, 1.4), 0.7);                   // small laptop: lower limit
        assert_eq!(zoom_for(3840.0, 0.36, 780.0, 0.7, 1.4), 1.4);                   // 4K at 100%: upper limit
        assert_eq!(zoom_for(0.0, 0.36, 780.0, 0.7, 1.4), 1.0);                      // unknown screen
    }
}
