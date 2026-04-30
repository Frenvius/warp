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

/// Wraps the panels area in a chrome margin. The Border only reserves
/// the margin geometry — both the border and the wrapper's background
/// stay transparent so the parent workspace Container's
/// `outer_chrome_fill` shows through unchanged. That guarantees the
/// frame strip, the inter-pane gaps, and the tab bar all paint from the
/// exact same surface (including any window-focus dimming applied
/// upstream of this wrapper).
pub fn wrap_panels(panels: Box<dyn Element>, app: &AppContext) -> Box<dyn Element> {
    let _ = app;
    Container::new(panels)
        .with_border(
            Border::new(ISLAND_OUTER_MARGIN)
                .with_sides(false, true, true, true)
                .with_border_fill(Fill::None),
        )
        .finish()
}

/// Single source of truth for the chrome color: the tab bar / window
/// header, the frame margin around the panels, and the inter-pane gaps
/// all read from this. Anything that needs to render an "outside the
/// pane" surface should call this so there's no chance of drift.
pub fn outer_chrome_fill(app: &AppContext) -> Fill {
    internal_colors::fg_overlay_1(Appearance::as_ref(app).theme()).into()
}

/// Fill used for the divider strip between split panes. Always
/// transparent — the resize handle must not paint over its hit-target.
/// The gap surface is supplied by `outer_chrome_fill` on a parent
/// Container; the divider just reserves the layout slot.
pub fn split_divider_fill(_theme: &warp_core::ui::theme::WarpTheme) -> Fill {
    Fill::None
}
