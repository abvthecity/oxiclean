//! Colors, glyphs and the small drawing pieces every view shares.
//!
//! Colors are foregrounds only, picked to read on dark and light
//! terminals alike; selection is a marker and weight, never a background.
//! With `NO_COLOR` set, everything is drawn in the terminal's own color.

use oxiclean_typecheck::program_map::Cause;
use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

/// An RGB color, so bars can blend between two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// The color `t` of the way from `self` to `other`.
    pub(crate) fn mix(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        // Each channel stays within 0..=255, so the cast can't truncate.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ch = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        Self(ch(self.0, other.0), ch(self.1, other.1), ch(self.2, other.2))
    }
}

pub(crate) const ACCENT: Rgb = Rgb(86, 204, 152);
pub(crate) const MUTED: Rgb = Rgb(128, 138, 134);
pub(crate) const TRACK: Rgb = Rgb(78, 86, 83);
pub(crate) const WARN: Rgb = Rgb(232, 180, 84);
pub(crate) const ERROR: Rgb = Rgb(236, 112, 100);
const GLINT: Rgb = Rgb(236, 248, 242);

/// The color of a cause.
pub(crate) fn cause_color(cause: Cause) -> Rgb {
    match cause {
        Cause::SourceImport => WARN,
        Cause::CatchAll => ERROR,
        Cause::Tests => Rgb(110, 166, 232),
        Cause::BuildOutput => Rgb(178, 148, 236),
        Cause::Include => Rgb(92, 190, 204),
        _ => ACCENT,
    }
}

/// Styles, with or without color.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Theme {
    pub color: bool,
}

impl Theme {
    /// Color unless `NO_COLOR` is set to anything non-empty.
    pub(crate) fn from_env() -> Self {
        Self { color: std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()) }
    }

    pub(crate) fn fg(self, c: Rgb) -> Style {
        if self.color { Style::new().fg(Color::Rgb(c.0, c.1, c.2)) } else { Style::new() }
    }

    /// Primary text: the terminal's own color.
    pub(crate) fn text(self) -> Style {
        Style::new()
    }

    pub(crate) fn strong(self) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    /// Secondary text.
    pub(crate) fn muted(self) -> Style {
        if self.color { self.fg(MUTED) } else { Style::new().add_modifier(Modifier::DIM) }
    }

    /// Section labels: muted, bold, upper case.
    pub(crate) fn label(self) -> Style {
        self.muted().add_modifier(Modifier::BOLD)
    }

    pub(crate) fn accent(self) -> Style {
        self.fg(ACCENT)
    }
}

/// A horizontal bar `width` cells wide, filled `frac` of the way with a
/// blend from `from` to `to`. `glint`, from 0 to 1, is where a bright band
/// passes over the filled part.
pub(crate) fn bar(
    theme: Theme,
    width: usize,
    frac: f32,
    from: Rgb,
    to: Rgb,
    glint: Option<f32>,
) -> Vec<Span<'static>> {
    let frac = frac.clamp(0.0, 1.0);
    // Half cells, so short bars still show a difference.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let halves = (frac * (width * 2) as f32).round() as usize;
    let mut spans = Vec::with_capacity(width);
    for i in 0..width {
        #[allow(clippy::cast_precision_loss)]
        let t = if width > 1 { i as f32 / (width - 1) as f32 } else { 0.0 };
        let mut c = from.mix(to, t);
        if let Some(g) = glint {
            let d = (t - g).abs();
            if d < 0.12 {
                c = c.mix(GLINT, (1.0 - d / 0.12) * 0.7);
            }
        }
        let (glyph, style) = if 2 * i + 2 <= halves {
            ("━", theme.fg(c))
        } else if 2 * i < halves {
            ("╸", theme.fg(c))
        } else {
            ("─", theme.fg(TRACK))
        };
        spans.push(Span::styled(glyph, style));
    }
    spans
}

/// `n` in at most five characters: `912`, `12.4k`, `1.38M`.
pub(crate) fn compact(n: usize) -> String {
    #[allow(clippy::cast_precision_loss)]
    let f = n as f64;
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{:.1}k", f / 1e3),
        10_000..1_000_000 => format!("{:.0}k", f / 1e3),
        1_000_000..10_000_000 => format!("{:.2}M", f / 1e6),
        _ => format!("{:.1}M", f / 1e6),
    }
}

/// `n` with thousands separators: `1,384,220`.
pub(crate) fn grouped(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// The frames of a spinner.
pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_stay_short() {
        assert_eq!(compact(912), "912");
        assert_eq!(compact(1_240), "1.2k");
        assert_eq!(compact(29_040), "29k");
        assert_eq!(compact(1_384_220), "1.38M");
        assert_eq!(grouped(1_384_220), "1,384,220");
        assert_eq!(grouped(12), "12");
    }

    #[test]
    fn bars_fill_in_half_cells() {
        let glyphs = |frac| -> String {
            bar(Theme { color: false }, 4, frac, ACCENT, ACCENT, None)
                .iter()
                .map(|s| s.content.as_ref())
                .collect()
        };
        assert_eq!(glyphs(0.0), "────");
        assert_eq!(glyphs(0.5), "━━──");
        assert_eq!(glyphs(0.625), "━━╸─");
        assert_eq!(glyphs(1.0), "━━━━");
    }
}
