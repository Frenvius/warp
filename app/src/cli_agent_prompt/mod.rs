//! Floating prompt overlay shown over the terminal grid when a Claude Code
//! CLI agent session is active in the pane. Inspired by waveterm's
//! `term-claude-prompt.tsx` (a thin frontend onto Claude Code's stdin), but
//! reuses warp's existing `EditorView` for the input widget rather than a
//! contentEditable replica.
//!
//! Lives outside upstream `app/src/terminal/` so the bulk of the
//! implementation never causes upstream merge conflicts. The only upstream
//! touch points are the per-pane field on `TerminalView`, its construction,
//! and its render-time positioning — all in `terminal/view.rs`.

use warp_core::ui::theme::color::internal_colors;
use warpui::elements::{
    Border, Container, CornerRadius, CrossAxisAlignment, Element, Flex, MainAxisSize,
    OffsetPositioning, ParentAnchor, ParentElement, ParentOffsetBounds, Radius, Shrinkable, Stack,
};
use warpui::geometry::vector::vec2f;
use warpui::presenter::ChildView;
use warpui::{
    AppContext, Entity, EntityId, FocusContext, SingletonEntity as _, View, ViewContext,
    ViewHandle, WeakViewHandle,
};
use warpui::elements::ChildAnchor;

use crate::appearance::Appearance;
use crate::editor::{
    EditorOptions, EditorView, Event as EditorEvent, PropagateAndNoOpNavigationKeys, TextOptions,
};
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::terminal::view::TerminalView;
use crate::terminal::CLIAgent;

const PROMPT_FONT_SIZE: f32 = 13.;
const PROMPT_PLACEHOLDER: &str = "Tell Claude what to do...";
const PROMPT_BORDER_RADIUS: f32 = 8.;
const PROMPT_PADDING_H: f32 = 10.;
const PROMPT_PADDING_V: f32 = 6.;
const OVERLAY_OUTER_MARGIN_X: f32 = 4.;
const OVERLAY_OUTER_MARGIN_Y: f32 = 5.;

/// Per-pane floating overlay hosting an `EditorView` whose submitted text is
/// piped to the terminal's PTY. One instance per `TerminalView`. Visibility
/// is decided by the parent (the overlay simply renders its chrome whenever
/// the parent mounts it).
pub struct ClaudePromptOverlay {
    editor: ViewHandle<EditorView>,
    _terminal_view: WeakViewHandle<TerminalView>,
}

#[derive(Debug)]
pub enum ClaudePromptOverlayEvent {
    /// User pressed Enter on a non-empty buffer. The parent
    /// `TerminalView` writes the text + CR to the PTY.
    Submit { text: String },
}

impl ClaudePromptOverlay {
    pub fn new(
        terminal_view: WeakViewHandle<TerminalView>,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = EditorOptions {
                text: TextOptions::ui_text(Some(PROMPT_FONT_SIZE), appearance),
                propagate_and_no_op_vertical_navigation_keys:
                    PropagateAndNoOpNavigationKeys::Always,
                autogrow: true,
                soft_wrap: true,
                ..Default::default()
            };
            let mut editor = EditorView::new(options, ctx);
            editor.set_placeholder_text(PROMPT_PLACEHOLDER, ctx);
            editor
        });

        ctx.subscribe_to_view(&editor, |me, _, event, ctx| {
            if matches!(event, EditorEvent::Enter) {
                me.handle_send(ctx);
            }
        });

        Self {
            editor,
            _terminal_view: terminal_view,
        }
    }

    fn handle_send(&mut self, ctx: &mut ViewContext<Self>) {
        let text = self.editor.update(ctx, |editor, ctx| {
            let buf = editor.buffer_text(ctx);
            editor.clear_buffer_and_reset_undo_stack(ctx);
            buf
        });
        if text.trim().is_empty() {
            return;
        }
        ctx.emit(ClaudePromptOverlayEvent::Submit { text });
    }
}

impl Entity for ClaudePromptOverlay {
    type Event = ClaudePromptOverlayEvent;
}

impl View for ClaudePromptOverlay {
    fn ui_name() -> &'static str {
        "ClaudePromptOverlay"
    }

    fn on_focus(&mut self, focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        if focus_ctx.is_self_focused() {
            ctx.focus(&self.editor);
            ctx.notify();
        }
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let bg_fill = theme.surface_1();
        let border_color = internal_colors::neutral_3(theme);

        let editor_box = Container::new(ChildView::new(&self.editor).finish())
            .with_padding_left(PROMPT_PADDING_H)
            .with_padding_right(PROMPT_PADDING_H)
            .with_padding_top(PROMPT_PADDING_V)
            .with_padding_bottom(PROMPT_PADDING_V)
            .finish();

        let row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(Shrinkable::new(1., editor_box).finish())
            .finish();

        Container::new(row)
            .with_background(bg_fill)
            .with_border(Border::all(1.).with_border_color(border_color))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(PROMPT_BORDER_RADIUS)))
            .finish()
    }
}

/// True when the pane's currently-running CLI agent is Claude Code.
pub fn is_claude_code_active(view_id: EntityId, app: &AppContext) -> bool {
    matches!(
        CLIAgentSessionsModel::as_ref(app)
            .session(view_id)
            .map(|s| s.agent),
        Some(CLIAgent::Claude)
    )
}

/// If Claude Code is active in this pane, anchors the overlay's view as a
/// positioned child at the bottom of the terminal `Stack` with horizontal
/// margins matching the island chrome and a small lift off the bottom edge.
/// No-op otherwise.
pub fn add_overlay_to_stack(
    stack: &mut Stack,
    view_id: EntityId,
    overlay: &ViewHandle<ClaudePromptOverlay>,
    app: &AppContext,
) {
    if !is_claude_code_active(view_id, app) {
        return;
    }
    let element = Container::new(ChildView::new(overlay).finish())
        .with_margin_left(OVERLAY_OUTER_MARGIN_X)
        .with_margin_right(OVERLAY_OUTER_MARGIN_X)
        .with_margin_bottom(OVERLAY_OUTER_MARGIN_Y)
        .finish();
    stack.add_positioned_overlay_child(
        element,
        OffsetPositioning::offset_from_parent(
            vec2f(0., 0.),
            ParentOffsetBounds::ParentByPosition,
            ParentAnchor::BottomMiddle,
            ChildAnchor::BottomMiddle,
        ),
    );
}
