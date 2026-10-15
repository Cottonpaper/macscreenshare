//! Rect fitting and coordinate mapping shared by the renderer and input.

/// A rectangle in surface pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn full(width: f32, height: f32) -> Self {
        Rect { x: 0.0, y: 0.0, w: width, h: height }
    }
}

/// Fit `content` inside `surface`, preserving aspect ratio and centring it.
/// Black bars fill the remainder (letterbox/pillarbox).
pub fn fit_rect(surface_w: f32, surface_h: f32, content_w: f32, content_h: f32) -> Rect {
    if surface_w <= 0.0 || surface_h <= 0.0 || content_w <= 0.0 || content_h <= 0.0 {
        return Rect::full(surface_w.max(0.0), surface_h.max(0.0));
    }
    let scale = (surface_w / content_w).min(surface_h / content_h);
    let w = content_w * scale;
    let h = content_h * scale;
    Rect {
        x: (surface_w - w) / 2.0,
        y: (surface_h - h) / 2.0,
        w,
        h,
    }
}

/// Map a point in surface pixels to normalized content coordinates `[0,1]`.
/// Returns `None` when the point falls outside the fitted content rect.
pub fn to_normalized(rect: Rect, px: f32, py: f32) -> Option<(f32, f32)> {
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return None;
    }
    let nx = (px - rect.x) / rect.w;
    let ny = (py - rect.y) / rect.h;
    if !(0.0..=1.0).contains(&nx) || !(0.0..=1.0).contains(&ny) {
        return None;
    }
    Some((nx, ny))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wider_content_is_letterboxed() {
        // 16:9 content in a square window → full width, bars top and bottom.
        let r = fit_rect(1000.0, 1000.0, 1920.0, 1080.0);
        assert!((r.w - 1000.0).abs() < 0.01);
        assert!((r.h - 1000.0 * 9.0 / 16.0).abs() < 0.01);
        assert!((r.x - 0.0).abs() < 0.01);
        assert!(r.y > 0.0);
    }

    #[test]
    fn taller_content_is_pillarboxed() {
        let r = fit_rect(1000.0, 1000.0, 1080.0, 1920.0);
        assert!((r.h - 1000.0).abs() < 0.01);
        assert!((r.w - 1000.0 * 9.0 / 16.0).abs() < 0.01);
        assert!((r.y - 0.0).abs() < 0.01);
        assert!(r.x > 0.0);
    }

    #[test]
    fn centred_mapping() {
        let r = Rect { x: 100.0, y: 50.0, w: 800.0, h: 400.0 };
        assert_eq!(to_normalized(r, 500.0, 250.0), Some((0.5, 0.5)));
        assert_eq!(to_normalized(r, 100.0, 50.0), Some((0.0, 0.0)));
        assert_eq!(to_normalized(r, 900.0, 450.0), Some((1.0, 1.0)));
    }

    #[test]
    fn rejects_points_in_the_bars() {
        let r = Rect { x: 100.0, y: 50.0, w: 800.0, h: 400.0 };
        assert_eq!(to_normalized(r, 50.0, 250.0), None);
        assert_eq!(to_normalized(r, 500.0, 10.0), None);
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        let r = fit_rect(0.0, 100.0, 1920.0, 1080.0);
        assert_eq!(r.w, 0.0);
        assert_eq!(to_normalized(Rect::full(0.0, 0.0), 1.0, 1.0), None);
    }
}
