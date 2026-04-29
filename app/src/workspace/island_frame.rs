//! "Island" framing for the workspace content area: chrome margin around the
//! panels so the tab bar / window header reads as a frame around the rounded
//! panes inside.

use warpui::elements::{Border, Container, CornerRadius, Element, Fill, Radius};
use warpui::{AppContext, SingletonEntity as _};

use warp_core::ui::theme::color::internal_colors;

use crate::appearance::Appearance;

const ISLAND_OUTER_MARGIN: f32 = 5.0;
const ISLAND_CORNER_RADIUS: f32 = 8.0;

/// Thickness of the chrome strip rendered between split panes. Matches
/// [`ISLAND_OUTER_MARGIN`] so the gap between two panes reads as a
/// continuation of the chrome margin around the panel area.
pub const SPLIT_DIVIDER_THICKNESS: f32 = ISLAND_OUTER_MARGIN;

/// Bottom-border thickness applied below the tab bar. Zeroed so the tab bar
/// flows seamlessly into the island chrome.
pub const TAB_BAR_BOTTOM_BORDER_HEIGHT: f32 = 0.0;

/// Corner radius applied to the pane `Container` so the rectangular
/// `inactive_pane_overlay` (painted via `with_foreground_overlay`) follows
/// the same rounded path as the pane content. Without this the overlay leaks
/// past the pane's rounded edge and shows up as a "pointed tip" at the
/// corners of inactive panes.
pub fn pane_container_corner_radius() -> CornerRadius {
    CornerRadius::with_all(Radius::Pixels(ISLAND_CORNER_RADIUS))
}

/// Wraps the panels area in a chrome margin. Uses a `Border` (instead of
/// padding + parent background) so the chrome is painted only on the strip
/// around the panels, never bleeding through transparent pane content.
pub fn wrap_panels(panels: Box<dyn Element>, app: &AppContext) -> Box<dyn Element> {
    let theme = Appearance::as_ref(app).theme();
    Container::new(panels)
        .with_border(
            Border::new(ISLAND_OUTER_MARGIN)
                .with_sides(false, true, true, true)
                .with_border_fill(internal_colors::fg_overlay_1(theme)),
        )
        .finish()
}

/// Background fill for the chrome that surrounds the island. Same fill the
/// tab bar uses (`fg_overlay_1` — 5% foreground tint) so the chrome reads
/// as one continuous surface with the tab bar / window header instead of
/// being a different shade beside it.
pub fn outer_chrome_fill(app: &AppContext) -> Fill {
    internal_colors::fg_overlay_1(Appearance::as_ref(app).theme()).into()
}

/// Fill used for the divider strip between split panes. Returns `Fill::None`
/// so the divider's resize hit-target stays in the layout while remaining
/// visually transparent — the chrome painted by `outer_chrome_fill` on the
/// surrounding Container shows through, giving a single uniform gap surface
/// without stacked alpha at 4-way intersections.
pub fn split_divider_fill(_theme: &warp_core::ui::theme::WarpTheme) -> Fill {
    Fill::None
}
