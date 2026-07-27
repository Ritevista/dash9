//! Semantic terminal theme: named roles and a stable series palette.
//!
//! Widgets reference roles from here, never raw `Color` literals
//! (Mechanism 4). This is the only module allowed to turn a semantic
//! role or a [`crate::chart::Severity`] into a concrete `ratatui`
//! color; `crate::chart` itself stays Ratatui-free.

use ratatui::style::Color;

use crate::chart::Severity;

pub const PRIMARY: Color = Color::Cyan;
pub const SECONDARY: Color = Color::Magenta;
pub const SUCCESS: Color = Color::Green;
pub const WARNING: Color = Color::Yellow;
pub const DANGER: Color = Color::Red;
pub const MUTED: Color = Color::DarkGray;
pub const TEXT: Color = Color::Gray;
pub const FOCUS: Color = Color::LightCyan;

/// Stable chart-series palette. Order is part of the contract: series
/// `i` always gets `series_color(i)`, so a legend and its line keep
/// the same color across redraws as long as series order is stable.
/// Sized to match `chart::MAX_DISPLAYED_SERIES` exactly — a chart
/// legitimately showing its full cap of series must never wrap back
/// onto a color already in use on screen (confirmed live: an
/// 8-per-cpu-core panel hit both the old 6-color palette's wrap point
/// at once, indistinguishable by color alone). The last two
/// (`LightRed`, `LightBlue`) are the standard ANSI "bright" variants
/// of colors already in the first six — every other base hue
/// (cyan/magenta/green/yellow/blue/red) is already spoken for, so
/// distinguishing the last two slots by intensity rather than
/// introducing an RGB color (which not every terminal renders
/// consistently) keeps the palette portable.
const SERIES: [Color; 8] = [
    PRIMARY,
    SECONDARY,
    SUCCESS,
    WARNING,
    Color::Blue,
    DANGER,
    Color::LightRed,
    Color::LightBlue,
];

pub fn series_color(index: usize) -> Color {
    SERIES[index % SERIES.len()]
}

/// Color supplements [`Severity::marker`] and [`Severity::label`]; it
/// never replaces them (Mechanism 4) — a monochrome terminal still
/// shows the marker glyph and the breached threshold's name.
pub fn severity_color(severity: &Severity) -> Color {
    match severity {
        Severity::Ok => SUCCESS,
        Severity::Breached(_) => DANGER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chart::ThresholdBand;
    use dash9_core::ThresholdOp;

    #[test]
    fn series_palette_is_stable_and_cycles() {
        assert_eq!(series_color(0), PRIMARY);
        assert_eq!(series_color(1), SECONDARY);
        assert_eq!(series_color(8), PRIMARY);
    }

    #[test]
    fn series_palette_covers_every_chart_can_legitimately_show_without_a_collision() {
        // MAX_DISPLAYED_SERIES (chart.rs) is the most a chart ever
        // draws at once -- every one of those slots must get its own
        // color, or two genuinely-different series become
        // indistinguishable on screen (confirmed live).
        let colors: std::collections::HashSet<Color> = (0..crate::chart::MAX_DISPLAYED_SERIES)
            .map(series_color)
            .collect();
        assert_eq!(
            colors.len(),
            crate::chart::MAX_DISPLAYED_SERIES,
            "every series a chart can display at once needs a distinct color"
        );
    }

    #[test]
    fn severity_maps_to_role_color_but_meaning_survives_without_it() {
        assert_eq!(severity_color(&Severity::Ok), SUCCESS);
        let breached = Severity::Breached(ThresholdBand {
            name: "crit".into(),
            op: ThresholdOp::Gte,
            value: 0.9,
        });
        assert_eq!(severity_color(&breached), DANGER);
        // The color alone is not the contract: label/marker must still
        // distinguish this state on their own.
        assert_eq!(breached.label(), "crit");
        assert_eq!(breached.marker(), '▲');
    }
}
