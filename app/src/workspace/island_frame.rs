//! Fork-only "island" framing for the workspace content area.
//!
//! Adds a chrome margin around the panels area so the tab bar / window
//! header reads as a frame around the rounded panes inside. Lives in its
//! own module so upstream merges only touch a couple of small call sites
//! in `workspace/view.rs` and `pane_group/pane/view/mod.rs`.

use warpui::elements::{Container, CornerRadius, Element, Fill, Radius};
use warpui::{AppContext, SingletonEntity as _};

use crate::appearance::Appearance;

const ISLAND_OUTER_MARGIN: f32 = 3.0;
const ISLAND_CORNER_RADIUS: f32 = 8.0;

/// Bottom-border thickness applied below the tab bar. Zeroed so the tab bar
/// flows seamlessly into the island chrome.
pub const TAB_BAR_BOTTOM_BORDER_HEIGHT: f32 = 0.0;

/// Corner radius applied to the pane `Container` upstream so the rectangular
/// `inactive_pane_overlay` (painted via `with_foreground_overlay`) follows
/// the same rounded path as the pane content. Without this the overlay
/// leaks past the pane's rounded edge into the surrounding chrome and shows
/// up as a "pointed tip" at the corners of inactive panes.
pub fn pane_container_corner_radius() -> CornerRadius {
    CornerRadius::with_all(Radius::Pixels(ISLAND_CORNER_RADIUS))
}

/// Wraps the panels area in a chrome margin. The rounded look of each pane
/// comes from `pane_container_corner_radius` upstream, so this wrap only
/// needs to provide breathing room around the group.
pub fn wrap_panels(panels: Box<dyn Element>, _app: &AppContext) -> Box<dyn Element> {
    Container::new(panels)
        .with_padding_left(ISLAND_OUTER_MARGIN)
        .with_padding_right(ISLAND_OUTER_MARGIN)
        .with_padding_bottom(ISLAND_OUTER_MARGIN)
        .finish()
}

/// Background fill for the chrome that surrounds the island. Uses a more
/// standout surface tint than the pane background so the frame reads as a
/// clear extension of the tab bar / window header.
pub fn outer_chrome_fill(app: &AppContext) -> Fill {
    Appearance::as_ref(app).theme().surface_2().into()
}
