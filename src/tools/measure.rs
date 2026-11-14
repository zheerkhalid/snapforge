//! Ruler math (pure): units, calibration, tick marks, protractor angles.

use crate::settings::tools::RulerUnit;

pub const MM_PER_INCH: f64 = 25.4;
pub const CM_PER_INCH: f64 = 2.54;
/// Windows' nominal pixel density when nothing better is known.
pub const DEFAULT_PPI: f64 = 96.0;

/// Diagonal of a `w` x `h` pixel panel, in pixels.
pub fn diagonal_px(w: u32, h: u32) -> f64 {
    ((w as f64).powi(2) + (h as f64).powi(2)).sqrt()
}

/// Physical pixel density from the monitor resolution and its diagonal in inches.
/// Returns [`DEFAULT_PPI`] for nonsensical input.
pub fn ppi_from_diagonal(w: u32, h: u32, diagonal_in: f64) -> f64 {
    if !(diagonal_in.is_finite() && diagonal_in > 0.5) || w == 0 || h == 0 {
        return DEFAULT_PPI;
    }
    diagonal_px(w, h) / diagonal_in
}

/// The diagonal (inches) a panel would have at `ppi`.
pub fn diagonal_from_ppi(w: u32, h: u32, ppi: f64) -> f64 {
    if ppi <= 0.0 {
        return 0.0;
    }
    diagonal_px(w, h) / ppi
}

/// Diagonal in inches from a physical size in millimetres (GetDeviceCaps HORZSIZE /
/// VERTSIZE, which Windows fills from the EDID). `None` when the size is missing or
/// implausible: smaller than 4", larger than 120", or with an aspect ratio that does not
/// match the pixel grid (projectors and some KVMs report junk such as 160x90 mm or 0).
pub fn diagonal_from_mm(w_mm: i32, h_mm: i32, w_px: u32, h_px: u32) -> Option<f64> {
    if w_mm <= 0 || h_mm <= 0 || w_px == 0 || h_px == 0 {
        return None;
    }
    let d = ((w_mm as f64).powi(2) + (h_mm as f64).powi(2)).sqrt() / MM_PER_INCH;
    if !(4.0..=120.0).contains(&d) {
        return None;
    }
    // Orientation-independent: some drivers keep the landscape size for rotated panels.
    let aspect = |a: f64, b: f64| a.max(b) / a.min(b);
    let aspect_mm = aspect(w_mm as f64, h_mm as f64);
    let aspect_px = aspect(w_px as f64, h_px as f64);
    if (aspect_mm / aspect_px - 1.0).abs() > 0.15 {
        return None;
    }
    Some(d)
}

/// Default diagonal for a monitor: from its reported size, else as if it were 96 PPI.
pub fn default_diagonal(w_mm: i32, h_mm: i32, w_px: u32, h_px: u32) -> f64 {
    diagonal_from_mm(w_mm, h_mm, w_px, h_px).unwrap_or_else(|| diagonal_from_ppi(w_px, h_px, DEFAULT_PPI))
}

/// Pixels per one `unit`.
pub fn px_per_unit(unit: RulerUnit, ppi: f64) -> f64 {
    match unit {
        RulerUnit::Px => 1.0,
        RulerUnit::Cm => ppi / CM_PER_INCH,
        RulerUnit::Inch => ppi,
    }
}

pub fn px_to_unit(px: f64, unit: RulerUnit, ppi: f64) -> f64 {
    px / px_per_unit(unit, ppi)
}

pub fn unit_to_px(v: f64, unit: RulerUnit, ppi: f64) -> f64 {
    v * px_per_unit(unit, ppi)
}

/// Human text for a length: `"123 px"`, `"3.25 cm"`, `"1.28 in"`.
pub fn format_length(px: f64, unit: RulerUnit, ppi: f64) -> String {
    let v = px_to_unit(px, unit, ppi);
    match unit {
        RulerUnit::Px => format!("{} px", v.round() as i64),
        RulerUnit::Cm => format!("{v:.2} cm"),
        RulerUnit::Inch => format!("{v:.2} in"),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    /// Distance from the ruler's zero, in pixels.
    pub pos: f64,
    /// 0 = minor, 1 = middle, 2 = major.
    pub level: u8,
    pub label: Option<String>,
}

/// Tick marks for a ruler `length_px` long. `min_gap_px` is the smallest allowed
/// distance between two minor ticks (scaled for DPI by the caller), so dense units
/// coarsen instead of turning into a grey smear.
pub fn ticks(length_px: f64, unit: RulerUnit, ppi: f64, min_gap_px: f64) -> Vec<Tick> {
    // (minor step, ticks per middle, ticks per major, majors per label) in the unit.
    let schemes: &[(f64, u32, u32, u32)] = match unit {
        RulerUnit::Px => &[(2.0, 5, 25, 2), (5.0, 2, 10, 2), (10.0, 5, 10, 1), (25.0, 2, 4, 1), (50.0, 2, 4, 2)],
        RulerUnit::Cm => &[(0.1, 5, 10, 1), (0.25, 2, 4, 1), (0.5, 2, 2, 1), (1.0, 5, 5, 2)],
        RulerUnit::Inch => &[(1.0 / 16.0, 4, 16, 1), (0.125, 4, 8, 1), (0.25, 2, 4, 1), (0.5, 2, 2, 1)],
    };
    let ppu = px_per_unit(unit, ppi);
    let &(step, mid_every, major_every, label_every) =
        schemes.iter().find(|s| s.0 * ppu >= min_gap_px).or(schemes.last()).unwrap_or(&(1.0, 5, 10, 1));
    let step_px = step * ppu;
    if !(step_px.is_finite() && step_px > 0.0) {
        return Vec::new();
    }
    let n = (length_px / step_px).floor() as u32;
    let mut out = Vec::with_capacity(n as usize + 1);
    for i in 0..=n {
        let level = if i % major_every == 0 {
            2
        } else if i % mid_every == 0 {
            1
        } else {
            0
        };
        let label = (level == 2 && (i / major_every) % label_every == 0).then(|| {
            let v = i as f64 * step;
            if (v - v.round()).abs() < 1e-6 {
                format!("{}", v.round() as i64)
            } else {
                format!("{v:.1}")
            }
        });
        out.push(Tick { pos: i as f64 * step_px, level, label });
    }
    out
}

/// Direction of `p` seen from `c`, in degrees counter-clockwise from the +x axis with
/// the y axis pointing up (screen y grows downwards), in [0, 360).
pub fn direction_deg(c: (f64, f64), p: (f64, f64)) -> f64 {
    let a = (-(p.1 - c.1)).atan2(p.0 - c.0).to_degrees();
    a.rem_euclid(360.0)
}

/// Angle between the rays c→a and c→b, in [0, 180].
pub fn angle_between(c: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let d = (direction_deg(c, a) - direction_deg(c, b)).abs();
    if d > 180.0 {
        360.0 - d
    } else {
        d
    }
}

/// Counter-clockwise sweep from ray c→a to ray c→b, in [0, 360).
pub fn sweep_ccw(c: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    (direction_deg(c, b) - direction_deg(c, a)).rem_euclid(360.0)
}

pub fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn conversions() {
        assert!(close(px_to_unit(96.0, RulerUnit::Inch, 96.0), 1.0, 1e-12));
        assert!(close(px_to_unit(96.0, RulerUnit::Cm, 96.0), 2.54, 1e-12));
        assert!(close(px_to_unit(37.0, RulerUnit::Px, 300.0), 37.0, 1e-12));
        assert!(close(unit_to_px(1.0, RulerUnit::Cm, 254.0), 100.0, 1e-9));
        for u in RulerUnit::ALL {
            assert!(close(unit_to_px(px_to_unit(123.4, u, 140.0), u, 140.0), 123.4, 1e-9));
        }
        assert_eq!(format_length(150.4, RulerUnit::Px, 96.0), "150 px");
        assert_eq!(format_length(96.0, RulerUnit::Inch, 96.0), "1.00 in");
        assert_eq!(format_length(96.0, RulerUnit::Cm, 96.0), "2.54 cm");
    }

    #[test]
    fn calibration_math() {
        // A 27" 2560x1440 panel is ~108.8 PPI.
        let ppi = ppi_from_diagonal(2560, 1440, 27.0);
        assert!(close(ppi, 108.79, 0.01), "{ppi}");
        // A 15.6" 1920x1080 laptop is ~141.2 PPI.
        assert!(close(ppi_from_diagonal(1920, 1080, 15.6), 141.21, 0.01));
        // Round trip.
        assert!(close(diagonal_from_ppi(2560, 1440, ppi), 27.0, 1e-9));
        // Bad input falls back to 96.
        assert_eq!(ppi_from_diagonal(1920, 1080, 0.0), DEFAULT_PPI);
        assert_eq!(ppi_from_diagonal(1920, 1080, f64::NAN), DEFAULT_PPI);
        assert_eq!(ppi_from_diagonal(0, 1080, 24.0), DEFAULT_PPI);
        // EDID: 597x336 mm is a 27" 16:9 panel.
        let d = diagonal_from_mm(597, 336, 2560, 1440).expect("plausible");
        assert!(close(d, 26.97, 0.01), "{d}");
        // Junk sizes are rejected.
        assert_eq!(diagonal_from_mm(0, 0, 1920, 1080), None);
        assert!(diagonal_from_mm(336, 597, 2560, 1440).is_some(), "rotated panel");
        assert_eq!(diagonal_from_mm(16, 9, 1920, 1080), None, "too small");
        assert_eq!(diagonal_from_mm(500, 500, 1920, 1080), None, "wrong aspect");
        // Without EDID the default is 96 PPI: 1920x1080 -> 22.95".
        assert!(close(default_diagonal(0, 0, 1920, 1080), 22.95, 0.01));
        assert!(close(default_diagonal(597, 336, 2560, 1440), 26.97, 0.01));
        // 1 cm on the 27" panel.
        assert!(close(unit_to_px(1.0, RulerUnit::Cm, ppi), 42.83, 0.01));
    }

    #[test]
    fn pixel_ticks() {
        let t = ticks(200.0, RulerUnit::Px, 96.0, 2.0);
        assert_eq!(t.len(), 101);
        assert_eq!(t[0], Tick { pos: 0.0, level: 2, label: Some("0".into()) });
        assert_eq!(t[5].level, 1);
        assert_eq!(t[5].pos, 10.0);
        assert_eq!(t[1].level, 0);
        assert_eq!(t[25].level, 2);
        assert_eq!(t[25].label, None, "50 is major but unlabelled");
        assert_eq!(t[50].label.as_deref(), Some("100"));
        // High DPI: coarser minor ticks.
        let t = ticks(200.0, RulerUnit::Px, 192.0, 4.0);
        assert_eq!(t[1].pos, 5.0);
        assert_eq!(t[20].label.as_deref(), Some("100"));
    }

    #[test]
    fn metric_and_imperial_ticks() {
        // At 96 PPI 1 mm is 3.78 px.
        let t = ticks(200.0, RulerUnit::Cm, 96.0, 3.0);
        assert!(close(t[1].pos, 3.7795, 1e-3));
        assert_eq!(t[10].label.as_deref(), Some("1"));
        assert!(close(t[10].pos, 37.795, 1e-3));
        assert_eq!(t[5].level, 1);
        // Too dense for millimetres: coarsen to 2.5 mm.
        let t = ticks(200.0, RulerUnit::Cm, 96.0, 6.0);
        assert!(close(t[1].pos, 0.25 * 96.0 / 2.54, 1e-9));
        // Inches: 1/16" is 6 px at 96 PPI.
        let t = ticks(200.0, RulerUnit::Inch, 96.0, 3.0);
        assert_eq!(t[1].pos, 6.0);
        assert_eq!(t[8].level, 1, "half inch");
        assert_eq!(t[16].label.as_deref(), Some("1"));
        assert_eq!(t[16].pos, 96.0);
        assert_eq!(t.last().map(|t| t.pos), Some(198.0));
    }

    #[test]
    fn angles() {
        let c = (100.0, 100.0);
        assert!(close(direction_deg(c, (200.0, 100.0)), 0.0, 1e-9));
        assert!(close(direction_deg(c, (100.0, 0.0)), 90.0, 1e-9), "up is 90 degrees");
        assert!(close(direction_deg(c, (0.0, 100.0)), 180.0, 1e-9));
        assert!(close(direction_deg(c, (100.0, 200.0)), 270.0, 1e-9));
        assert!(close(angle_between(c, (200.0, 100.0), (100.0, 0.0)), 90.0, 1e-9));
        assert!(close(angle_between(c, (200.0, 100.0), (200.0, 0.0)), 45.0, 1e-9));
        // The smaller angle wraps through 0.
        assert!(close(angle_between(c, (200.0, 90.0), (200.0, 110.0)), 11.42, 0.01));
        assert!(close(angle_between(c, (200.0, 100.0), (0.0, 100.0)), 180.0, 1e-9));
        assert!(close(sweep_ccw(c, (200.0, 100.0), (100.0, 200.0)), 270.0, 1e-9));
        assert!(close(sweep_ccw(c, (100.0, 200.0), (200.0, 100.0)), 90.0, 1e-9));
        assert!(close(distance((0.0, 0.0), (3.0, 4.0)), 5.0, 1e-12));
    }
}
