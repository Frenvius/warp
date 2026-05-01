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

pub mod dynamic_commands;
pub mod history_store;
pub mod slash_commands;
pub mod status_parser;

/// Registers fork-owned keybindings for the Claude prompt overlay. Call
/// from `lib.rs::run`.
pub fn init(app: &mut warpui::AppContext) {
    use warpui::keymap::macros::*;
    use warpui::keymap::FixedBinding;
    app.register_fixed_bindings([
        FixedBinding::new(
            "shift-tab",
            ClaudePromptOverlayAction::CycleMode,
            id!(ClaudePromptOverlay::ui_name()),
        ),
        // Editor doesn't consume ctrl-g in non-Integration channels, but
        // the parent `Terminal` scope binding only fires when focus is on
        // the terminal grid, not on our floating editor. Bind it directly
        // on the overlay so toggle works regardless of where focus sits.
        FixedBinding::new(
            "ctrl-g",
            ClaudePromptOverlayAction::ToggleHide,
            id!(ClaudePromptOverlay::ui_name()),
        ),
    ]);
    // NOTE: Tab-from-terminal-back-to-overlay can't be done with a
    // FixedBinding because typed-action dispatch walks the focused view's
    // ancestors and the overlay is a *child* of the terminal — never in
    // that chain. We intercept Tab inside `keydown_on_terminal` instead.
}

use std::time::Duration;

use once_cell::sync::Lazy;
use pathfinder_color::ColorU;
use regex::Regex;
use warp_core::ui::theme::color::internal_colors;
use warp_core::ui::theme::{Fill as ThemeFill, WarpTheme};
use warpui::r#async::Timer;
use warpui::elements::{
    Border, ChildAnchor, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment,
    DispatchEventResult, Element, Empty, EventHandler, Flex, FormattedTextElement,
    MainAxisAlignment, MainAxisSize, OffsetPositioning, ParentAnchor, ParentElement,
    ParentOffsetBounds, Radius, Shrinkable, Stack, Text,
};
use warpui::geometry::vector::vec2f;
use warpui::presenter::ChildView;
use warpui::{
    AppContext, Entity, EntityId, FocusContext, SingletonEntity as _, TypedActionView, View,
    ViewContext, ViewHandle, WeakViewHandle,
};

use crate::settings::ai::AISettings;
use crate::appearance::Appearance;
use crate::editor::{
    EditOrigin, EditorOptions, EditorView, Event as EditorEvent, PropagateAndNoOpNavigationKeys,
    TextOptions,
};
use warp_editor::editor::NavigationKey;
use crate::terminal::cli_agent_sessions::{
    CLIAgentInputState, CLIAgentSession, CLIAgentSessionContext, CLIAgentSessionStatus,
    CLIAgentSessionsModel,
};
use crate::terminal::model_events::ModelEventDispatcher;
use crate::terminal::view::TerminalView;
use crate::terminal::CLIAgent;
use crate::ui_components::icons::Icon;
use warpui::ModelHandle;

use self::history_store::HistoryStore;
use self::slash_commands::{
    all_commands, builtin_models, detect_trigger, filter_entries, CommandEntry, PopoverTrigger,
};
use self::status_parser::{parse_status, ModeKey, ParsedStatus};

const PROMPT_FONT_SIZE: f32 = 13.;
const HINT_FONT_SIZE: f32 = 11.;
const PROMPT_PLACEHOLDER: &str = "Tell Claude what to do...";
const PROMPT_HINT: &str = "Enter ↵ send · Shift+Enter newline";
const PROMPT_HINT_POPOVER: &str = "Enter ↵ select & send · Tab complete · Esc close";
const PROMPT_HINT_SUBMITTING: &str = "Submitting prompt...";
/// Delay between the input pre-clear (Ctrl-C / Ctrl-U) and the actual prompt
/// bytes. Without it Claude may receive the keystrokes too close together to
/// reliably reset its input buffer before our text arrives.
const SUBMIT_PRECLEAR_DELAY_MS: u64 = 80;

/// Matches Claude's "Press Ctrl-C again to exit" footer, which appears for
/// ~2s after the user sends an empty Ctrl-C. If we sent another Ctrl-C while
/// this banner is up, Claude would interpret it as the confirming press and
/// exit — so the send pipeline pre-clears with Ctrl-U (kill line) instead.
static EXIT_BANNER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)press ctrl-?c again").unwrap());
const PROMPT_BORDER_RADIUS: f32 = 8.;
const PROMPT_PADDING_H: f32 = 10.;
const PROMPT_PADDING_V: f32 = 6.;
const OVERLAY_OUTER_MARGIN_X: f32 = 2.;
const OVERLAY_OUTER_MARGIN_Y: f32 = 2.;
const LOGO_SIZE: f32 = 16.;
/// Identifier injected into our editor's keymap context. Upstream's
/// `add_next_occurrence` binding (in `editor/view/mod.rs`) excludes this
/// flag so ctrl-g bubbles up to our `ToggleHide` binding instead of
/// getting consumed by the editor.
const EDITOR_KEYMAP_FLAG: &str = "ClaudePromptOverlayEditor";

/// Per-pane floating overlay hosting an `EditorView` whose submitted text is
/// piped to the terminal's PTY. One instance per `TerminalView`.
pub struct ClaudePromptOverlay {
    editor: ViewHandle<EditorView>,
    terminal_view: WeakViewHandle<TerminalView>,
    /// Hash of the alt-screen contents the last time we rendered. The
    /// poll-tick re-hashes the alt-screen and only `ctx.notify()`s on
    /// change, so chips track Claude's TUI in near-real time without
    /// repainting every frame.
    last_alt_hash: u64,
    /// Submitted prompts in submission order, newest at the back. Bounded
    /// at `HISTORY_MAX` to keep memory finite during long sessions.
    history: Vec<String>,
    history_state: HistoryState,
    /// When true, the prompt overlay collapses to a small chevron pill so
    /// the user can see the terminal grid behind it. Toggled via Ctrl+G
    /// from inside the editor or by clicking the pill.
    is_hidden: bool,
    /// Active completion popover, if any. Recomputed on every editor edit
    /// so the list stays in sync with what the user is currently typing.
    popover: Option<PopoverState>,
    /// On-disk per-workspace history. Resolved lazily because the pane's
    /// `pwd` isn't available at construction time on a fresh terminal.
    history_store: Option<HistoryStore>,
    /// Set once we've attempted store init for this overlay. Used to keep
    /// the lazy-init path idempotent without retrying on every poll tick
    /// (paths that fail to resolve a workspace key won't suddenly start
    /// working mid-session).
    history_loaded: bool,
    /// Snapshot of `slash_commands::all_commands()` taken at construction
    /// time. Plugins / user commands rarely change during a session, so we
    /// avoid re-walking the filesystem on every keystroke.
    commands: Vec<CommandEntry>,
    models: Vec<CommandEntry>,
    /// Most recently submitted prompt. Esc on an empty draft restores it,
    /// matching waveterm's "undo my interrupt" affordance.
    last_sent: Option<String>,
    /// True while a submission is mid-flight (between the pre-clear write
    /// and the actual prompt write). Blocks double-Enter and swaps the hint.
    is_submitting: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PopoverKind {
    Slash,
    Model,
}

#[derive(Debug, Clone)]
struct PopoverState {
    kind: PopoverKind,
    query: String,
    items: Vec<CommandEntry>,
    selected: usize,
}

impl PopoverState {
    fn from_trigger(
        trigger: PopoverTrigger,
        commands: &[CommandEntry],
        models: &[CommandEntry],
    ) -> Option<Self> {
        let (kind, query, items) = match trigger {
            PopoverTrigger::Slash { query } => {
                let items = filter_entries(commands, &query);
                (PopoverKind::Slash, query, items)
            }
            PopoverTrigger::Model { query } => {
                let items = filter_entries(models, &query);
                (PopoverKind::Model, query, items)
            }
        };
        if items.is_empty() {
            None
        } else {
            Some(Self {
                kind,
                query,
                items,
                selected: 0,
            })
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as i32;
        let cur = self.selected as i32;
        let next = (cur + delta).rem_euclid(len);
        self.selected = next as usize;
    }
}

const HISTORY_MAX: usize = 200;

/// Tracks whether the editor is showing the user's live draft or one of
/// the past history entries.
#[derive(Debug, Clone)]
enum HistoryState {
    LiveDraft,
    /// Editor currently shows `history[idx]`. `draft` holds whatever the
    /// user had typed before they started navigating, so we can restore it
    /// when they cursor back past the newest entry.
    Browsing { idx: usize, draft: String },
}

#[derive(Clone, Copy, Debug)]
pub enum ClaudePromptOverlayAction {
    /// Submit the editor's current text to Claude (same as Enter).
    Send,
    /// Send Ctrl-C to the PTY, asking Claude to interrupt the current run.
    Interrupt,
    /// Send Shift+Tab (`\x1b[Z`) to cycle Claude's permission mode.
    CycleMode,
    /// Hide the overlay; user can restore it by clicking the floating
    /// chevron pill that replaces it.
    ToggleHide,
    /// Move keyboard focus to the editor. Dispatched from a click handler
    /// on the prompt body so any click on padding lands focus on the input.
    FocusEditor,
}

#[derive(Debug)]
pub enum ClaudePromptOverlayEvent {
    /// User pressed Enter / clicked Send. Parent `TerminalView` writes
    /// `text` + `\r` to the PTY.
    Submit { text: String },
    /// User clicked an icon that maps to a fixed escape sequence (interrupt,
    /// cycle mode). Parent writes the bytes verbatim.
    WriteRaw { bytes: Vec<u8> },
    /// User pressed Tab in the editor with no popover open. Parent moves
    /// keyboard focus from the overlay back to the terminal grid.
    FocusTerminal,
}

impl ClaudePromptOverlay {
    pub fn new(
        terminal_view: WeakViewHandle<TerminalView>,
        model_events: ModelHandle<ModelEventDispatcher>,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = EditorOptions {
                text: TextOptions::ui_text(Some(PROMPT_FONT_SIZE), appearance),
                // Move the caret inside multi-line drafts; only escalate to
                // history navigation when the caret is already at the
                // first/last visible row.
                propagate_and_no_op_vertical_navigation_keys:
                    PropagateAndNoOpNavigationKeys::AtBoundary,
                autogrow: true,
                soft_wrap: true,
                // Tags this editor's keymap context with a sentinel that
                // upstream's `add_next_occurrence` binding excludes — without
                // it, ctrl-g hits AddNextOccurrence inside this editor before
                // bubbling to our `ToggleHide` binding on the overlay scope.
                keymap_context_modifier: Some(Box::new(|context, _| {
                    context.set.insert(EDITOR_KEYMAP_FLAG);
                })),
                ..Default::default()
            };
            let mut editor = EditorView::new(options, ctx);
            editor.set_placeholder_text(PROMPT_PLACEHOLDER, ctx);
            editor
        });

        ctx.subscribe_to_view(&editor, |me, _, event, ctx| match event {
            EditorEvent::Enter => {
                if me.popover.is_some() {
                    me.popover_complete(ctx);
                } else {
                    me.handle_send(ctx);
                }
            }
            EditorEvent::Escape => {
                if me.popover.take().is_some() {
                    ctx.notify();
                } else {
                    me.handle_escape(ctx);
                }
            }
            EditorEvent::Navigate(NavigationKey::ShiftTab) => {
                ctx.emit(ClaudePromptOverlayEvent::WriteRaw {
                    bytes: vec![0x1b, b'[', b'Z'],
                });
            }
            EditorEvent::Navigate(NavigationKey::Tab) => {
                if me.popover.is_some() {
                    me.popover_complete(ctx);
                } else {
                    ctx.emit(ClaudePromptOverlayEvent::FocusTerminal);
                }
            }
            EditorEvent::Navigate(NavigationKey::Up) => {
                if let Some(pop) = me.popover.as_mut() {
                    pop.move_selection(-1);
                    ctx.notify();
                } else {
                    me.history_prev(ctx);
                }
            }
            EditorEvent::Navigate(NavigationKey::Down) => {
                if let Some(pop) = me.popover.as_mut() {
                    pop.move_selection(1);
                    ctx.notify();
                } else {
                    me.history_next(ctx);
                }
            }
            EditorEvent::Edited(EditOrigin::UserTyped | EditOrigin::UserInitiated) => {
                me.history_state = HistoryState::LiveDraft;
                me.recompute_popover(ctx);
            }
            _ => {}
        });

        // The session model gates overlay visibility (`is_claude_code_active`),
        // so a re-render is required when an agent session starts/ends.
        ctx.subscribe_to_model(&CLIAgentSessionsModel::handle(ctx), |_, _, _, ctx| {
            ctx.notify();
        });

        let _ = model_events;

        let mut me = Self {
            editor,
            terminal_view,
            last_alt_hash: 0,
            history: Vec::new(),
            history_state: HistoryState::LiveDraft,
            is_hidden: false,
            popover: None,
            history_store: None,
            history_loaded: false,
            commands: all_commands(),
            models: builtin_models(),
            last_sent: None,
            is_submitting: false,
        };
        me.try_init_history(ctx);
        me.start_render_poll(ctx);
        me
    }

    /// Best-effort: resolve the pane's `pwd` and load any persisted prompt
    /// history. The `pwd` is read from `TerminalView::pwd()` which depends
    /// on the active block's metadata being populated, so this commonly
    /// fails on a freshly-spawned pane. The render poll calls back into
    /// here on each tick until it succeeds (`history_loaded` then latches
    /// the attempt off).
    fn try_init_history(&mut self, ctx: &mut ViewContext<Self>) {
        if self.history_loaded {
            return;
        }
        let Some(terminal_view) = self.terminal_view.upgrade(ctx) else {
            return;
        };
        let Some(pwd) = terminal_view.as_ref(ctx).pwd() else {
            return;
        };
        if let Some(store) = HistoryStore::for_workspace(&pwd, HISTORY_MAX) {
            self.history = store.load();
            self.history_store = Some(store);
        }
        self.history_loaded = true;
    }

    /// Polls the alt-screen contents every ~80ms and triggers a re-render
    /// when they change. None of the existing `ModelEvent` variants fire
    /// per cell paint, so without this the chips would stay frozen at
    /// whatever value they had when the last structured event landed.
    fn start_render_poll(&self, ctx: &mut ViewContext<Self>) {
        ctx.spawn(
            async move { Timer::after(Duration::from_millis(80)).await },
            |me, _, ctx| {
                let cur = me.read_alt_screen_hash(ctx);
                if cur != me.last_alt_hash {
                    me.last_alt_hash = cur;
                    ctx.notify();
                }
                me.try_init_history(ctx);
                me.maybe_register_claude_from_alt_screen(ctx);
                me.start_render_poll(ctx);
            },
        );
    }

    /// Fallback Claude detection: when the warp plugin doesn't register a
    /// session (e.g. PS1 input mode bypasses the warp shell bootstrap),
    /// no `CLIAgentSession` is created via plugin sentinels or block-list
    /// command detection. The overlay then never shows because
    /// `is_claude_code_active` returns false. Detect Claude directly from
    /// its alt-screen footer (`[Model (context)]`) and synthesize a
    /// listener-less session ourselves so the overlay surfaces regardless
    /// of warp's input plumbing.
    fn maybe_register_claude_from_alt_screen(&self, ctx: &mut ViewContext<Self>) {
        let Some(terminal_view) = self.terminal_view.upgrade(ctx) else {
            return;
        };
        let view_id = terminal_view.as_ref(ctx).id();

        if CLIAgentSessionsModel::as_ref(ctx).session(view_id).is_some() {
            return;
        }

        let parsed = self.read_parsed_status(ctx);
        if parsed.model.is_none() {
            return;
        }

        let should_auto_toggle_input =
            *AISettings::as_ref(ctx).auto_open_rich_input_on_cli_agent_start;
        CLIAgentSessionsModel::handle(ctx).update(ctx, |sessions_model, ctx| {
            // Re-check inside the closure to avoid a TOCTOU race against
            // a concurrent plugin SessionStart that registered between
            // the read above and this update.
            if sessions_model.session(view_id).is_some() {
                return;
            }
            sessions_model.set_session(
                view_id,
                CLIAgentSession {
                    agent: CLIAgent::Claude,
                    status: CLIAgentSessionStatus::InProgress,
                    session_context: CLIAgentSessionContext::default(),
                    input_state: CLIAgentInputState::Closed,
                    should_auto_toggle_input,
                    listener: None,
                    plugin_version: None,
                    remote_host: None,
                    draft_text: None,
                    custom_command_prefix: None,
                },
                ctx,
            );
        });
    }

    fn read_alt_screen_hash(&self, app: &AppContext) -> u64 {
        use std::hash::{Hash, Hasher};
        let Some(terminal_view) = self.terminal_view.upgrade(app) else {
            return self.last_alt_hash;
        };
        let model = terminal_view.as_ref(app).model.lock();
        if !model.is_alt_screen_active() {
            return 0;
        }
        let text = model.alt_screen().output_to_string();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        hasher.finish()
    }

    /// Public toggle invoked from `terminal::view` when warp's existing
    /// Ctrl-G binding fires. We hijack that handler instead of competing
    /// keybindings, since warp's binding lives at the parent `Terminal`
    /// scope and resolves before any binding registered on our overlay.
    pub fn toggle_hidden(&mut self, ctx: &mut ViewContext<Self>) {
        self.is_hidden = !self.is_hidden;
        ctx.notify();
    }

    /// Inspects the editor buffer and opens / refreshes / closes the
    /// completion popover. Preserves the selected entry across re-filters
    /// when possible.
    fn recompute_popover(&mut self, ctx: &mut ViewContext<Self>) {
        let text = self.editor.as_ref(ctx).buffer_text(ctx);
        let prev_name = self
            .popover
            .as_ref()
            .and_then(|p| p.items.get(p.selected).map(|e| e.name.clone()));
        self.popover = detect_trigger(&text)
            .and_then(|trig| {
                PopoverState::from_trigger(trig, &self.commands, &self.models)
            })
            .map(|mut p| {
                if let Some(name) = prev_name {
                    if let Some(idx) = p.items.iter().position(|e| e.name == name) {
                        p.selected = idx;
                    }
                }
                p
            });
        ctx.notify();
    }

    /// Inserts the popover's current selection into the editor buffer.
    /// `/model` is special-cased: it leaves the popover open in `Model`
    /// kind so the user can chain command + arg in one flow.
    fn popover_complete(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(popover) = self.popover.as_ref() else {
            return;
        };
        let Some(selected) = popover.items.get(popover.selected) else {
            return;
        };
        let new_text = match popover.kind {
            PopoverKind::Slash if selected.name == "/model" => "/model ".to_string(),
            PopoverKind::Slash => format!("{} ", selected.name),
            PopoverKind::Model => format!("/model {}", selected.name),
        };
        self.editor.update(ctx, |e, ctx| {
            e.set_buffer_text(&new_text, ctx);
            e.move_to_buffer_end(ctx);
        });
        self.recompute_popover(ctx);
    }

    fn handle_send(&mut self, ctx: &mut ViewContext<Self>) {
        if self.is_submitting {
            return;
        }
        let text = self.editor.update(ctx, |editor, ctx| {
            let buf = editor.buffer_text(ctx);
            editor.clear_buffer_and_reset_undo_stack(ctx);
            buf
        });
        if text.trim().is_empty() {
            return;
        }
        self.push_history(text.clone());
        self.last_sent = Some(text.clone());
        self.history_state = HistoryState::LiveDraft;

        // Pre-clear Claude's input so any text the user typed directly into
        // the TUI gets wiped before our prompt arrives. Ctrl-C is the
        // reliable wipe except when Claude is already showing its
        // "Press Ctrl-C again to exit" banner — a second Ctrl-C there would
        // kill Claude. In that case fall back to Ctrl-U (kill line),
        // which both wipes the input and dismisses the banner safely.
        let banner = self.detect_exit_banner(ctx);
        let clear_byte: u8 = if banner { 0x15 } else { 0x03 };
        ctx.emit(ClaudePromptOverlayEvent::WriteRaw {
            bytes: vec![clear_byte],
        });

        self.is_submitting = true;
        ctx.notify();

        ctx.spawn(
            async move { Timer::after(Duration::from_millis(SUBMIT_PRECLEAR_DELAY_MS)).await },
            move |me, _, ctx| {
                ctx.emit(ClaudePromptOverlayEvent::Submit { text });
                me.is_submitting = false;
                ctx.notify();
            },
        );
    }

    /// Esc when no popover is open: send Ctrl-C to interrupt whatever Claude
    /// is doing, and if the editor is empty restore the most recently sent
    /// prompt. Mirrors waveterm's "I changed my mind" affordance.
    fn handle_escape(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.emit(ClaudePromptOverlayEvent::WriteRaw { bytes: vec![0x03] });
        let cur = self.editor.as_ref(ctx).buffer_text(ctx);
        if !cur.trim().is_empty() {
            return;
        }
        let Some(prev) = self.last_sent.clone() else {
            return;
        };
        self.history_state = HistoryState::LiveDraft;
        self.editor.update(ctx, |e, ctx| {
            // SystemEdit so the Edited handler doesn't reset history state
            // a second time and so this restoration doesn't get pushed onto
            // any undo stack as a user action.
            e.system_reset_buffer_text(prev.as_str(), ctx);
            e.move_to_buffer_end(ctx);
        });
    }

    /// True when Claude's footer is currently showing the "Press Ctrl-C
    /// again to exit" banner. Read off the alt-screen text — same source
    /// as the status parser, so no extra plumbing.
    fn detect_exit_banner(&self, app: &AppContext) -> bool {
        let Some(terminal_view) = self.terminal_view.upgrade(app) else {
            return false;
        };
        let model = terminal_view.as_ref(app).model.lock();
        if !model.is_alt_screen_active() {
            return false;
        }
        let text = model.alt_screen().output_to_string();
        // The banner sits at the very bottom of Claude's TUI; only scan the
        // tail to keep the regex cheap on large alt-screens.
        let tail = if text.len() > 1500 {
            &text[text.len() - 1500..]
        } else {
            text.as_str()
        };
        EXIT_BANNER_RE.is_match(tail)
    }

    fn push_history(&mut self, text: String) {
        // Skip exact-duplicate of the previous submission (shell-style).
        if self.history.last().map(|s| s.as_str()) == Some(text.as_str()) {
            return;
        }
        if let Some(store) = self.history_store.as_ref() {
            if let Err(err) = store.append(&text) {
                log::warn!("claude prompt: history append failed: {err}");
            }
        }
        self.history.push(text);
        if self.history.len() > HISTORY_MAX {
            let drop = self.history.len() - HISTORY_MAX;
            self.history.drain(..drop);
            // Compact on-disk file when in-memory drains, otherwise the
            // jsonl grows unbounded across long-lived workspaces.
            if let Some(store) = self.history_store.as_ref() {
                if let Err(err) = store.rewrite(&self.history) {
                    log::warn!("claude prompt: history rewrite failed: {err}");
                }
            }
        }
    }

    /// Up arrow at the first row of the editor: walk backward through
    /// history. The editor's `AtBoundary` propagation guarantees this only
    /// fires when there's nowhere left for the caret to move within the
    /// buffer, so we don't need to also handle in-buffer caret movement.
    fn history_prev(&mut self, ctx: &mut ViewContext<Self>) {
        if self.history.is_empty() {
            return;
        }
        let next_idx = match &self.history_state {
            HistoryState::LiveDraft => self.history.len() - 1,
            HistoryState::Browsing { idx, .. } if *idx > 0 => *idx - 1,
            HistoryState::Browsing { .. } => return,
        };
        let draft = match &self.history_state {
            HistoryState::LiveDraft => self.editor.as_ref(ctx).buffer_text(ctx),
            HistoryState::Browsing { draft, .. } => draft.clone(),
        };
        let entry = self.history[next_idx].clone();
        self.history_state = HistoryState::Browsing {
            idx: next_idx,
            draft,
        };
        self.editor.update(ctx, |e, ctx| {
            // `system_reset_buffer_text` uses `EditOrigin::SystemEdit`, so
            // our `Edited(UserTyped | UserInitiated)` arm doesn't fire and
            // the Browsing state survives. `set_buffer_text` would emit
            // `UserInitiated` and immediately wipe the saved draft.
            e.system_reset_buffer_text(entry.as_str(), ctx);
            e.move_to_buffer_end(ctx);
        });
    }

    /// Down arrow at the last row: walk forward through history. Past the
    /// newest entry, restore whatever live-draft text the user had typed
    /// before they started navigating.
    fn history_next(&mut self, ctx: &mut ViewContext<Self>) {
        let HistoryState::Browsing { idx, draft } = self.history_state.clone() else {
            return;
        };
        let next = idx + 1;
        if next < self.history.len() {
            let entry = self.history[next].clone();
            self.history_state = HistoryState::Browsing { idx: next, draft };
            self.editor.update(ctx, |e, ctx| {
                e.system_reset_buffer_text(entry.as_str(), ctx);
                e.move_to_buffer_end(ctx);
            });
        } else {
            self.history_state = HistoryState::LiveDraft;
            self.editor.update(ctx, |e, ctx| {
                e.system_reset_buffer_text(draft.as_str(), ctx);
                e.move_to_buffer_end(ctx);
            });
        }
    }

    fn render_logo(theme: &WarpTheme) -> Box<dyn Element> {
        let icon = Icon::ClaudeLogo.to_warpui_icon(theme.foreground()).finish();
        ConstrainedBox::new(icon)
            .with_width(LOGO_SIZE)
            .with_height(LOGO_SIZE)
            .finish()
    }

    /// Slash / model completion dropdown shown above the prompt body
    /// while the user is typing a `/<command>` or a `/model <name>`
    /// argument. List is bounded to keep the dropdown short.
    fn render_popover(
        &self,
        appearance: &Appearance,
        popover: &PopoverState,
    ) -> Box<dyn Element> {
        const MAX_VISIBLE: usize = 8;
        let theme = appearance.theme();
        let bg = theme.surface_2();
        let bg_solid = bg.into_solid();
        let border_color = internal_colors::neutral_3(theme);
        let name_color = internal_colors::text_main(theme, bg_solid);
        let desc_color = internal_colors::text_sub(theme, bg_solid);
        let select_color = theme.ansi_fg_blue();
        let selection_bg = with_alpha(select_color, 20);
        let selection_border = with_alpha(select_color, 60);

        let len = popover.items.len();
        let start = popover.selected.saturating_sub(MAX_VISIBLE - 1);
        let end = (start + MAX_VISIBLE).min(len);

        let mut col = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        for (idx_in_window, entry) in popover.items[start..end].iter().enumerate() {
            let abs_idx = start + idx_in_window;
            let is_selected = abs_idx == popover.selected;

            let name = Text::new(entry.name.to_string(), appearance.ui_font_family(), 12.)
                .with_color(if is_selected { select_color } else { name_color })
                .finish();
            let desc_text = if entry.aliases.is_empty() {
                entry.desc.to_string()
            } else {
                format!("{}  ({})", entry.desc, entry.aliases.join(", "))
            };
            let desc = Text::new(desc_text, appearance.ui_font_family(), 11.)
                .with_color(desc_color)
                .finish();

            let row = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(name)
                .with_child(Shrinkable::new(1., Empty::new().finish()).finish())
                .with_child(desc)
                .finish();

            let mut row_container = Container::new(row)
                .with_padding_left(8.)
                .with_padding_right(8.)
                .with_padding_top(3.)
                .with_padding_bottom(3.);
            if is_selected {
                row_container = row_container
                    .with_background(ThemeFill::Solid(selection_bg))
                    .with_border(Border::all(1.).with_border_color(selection_border))
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)));
            }
            col = col.with_child(row_container.finish());
        }

        let header_label = match popover.kind {
            PopoverKind::Slash => "Slash commands",
            PopoverKind::Model => "Models",
        };
        let header_color = internal_colors::text_sub(theme, bg_solid);
        let header = Text::new(
            format!(
                "{}  ({}/{})",
                header_label,
                popover.selected + 1,
                len.max(1)
            ),
            appearance.ui_font_family(),
            10.,
        )
        .with_color(header_color)
        .finish();

        let body = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(
                Container::new(header)
                    .with_padding_left(8.)
                    .with_padding_right(8.)
                    .with_padding_top(4.)
                    .with_padding_bottom(2.)
                    .finish(),
            )
            .with_child(col.finish())
            .finish();

        Container::new(body)
            .with_background(bg)
            .with_border(Border::all(1.).with_border_color(border_color))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(PROMPT_BORDER_RADIUS)))
            .with_padding_top(2.)
            .with_padding_bottom(2.)
            .finish()
    }

    /// Compact pill rendered in place of the prompt body when the user
    /// has toggled the overlay off. Click anywhere on it to restore the
    /// overlay; same `ToggleHide` action that hid it in the first place.
    fn render_hidden_pill(
        theme: &WarpTheme,
        bg_fill: warp_core::ui::theme::Fill,
        border_color: ColorU,
    ) -> Box<dyn Element> {
        let logo = Self::render_logo(theme);
        let pill = Container::new(logo)
            .with_padding_left(8.)
            .with_padding_right(8.)
            .with_padding_top(4.)
            .with_padding_bottom(4.)
            .with_background(bg_fill)
            .with_border(Border::all(1.).with_border_color(border_color))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(PROMPT_BORDER_RADIUS)))
            .finish();
        EventHandler::new(pill)
            .on_left_mouse_down(|ctx, _, _| {
                ctx.dispatch_typed_action(ClaudePromptOverlayAction::ToggleHide);
                DispatchEventResult::StopPropagation
            })
            .finish()
    }

    /// Builds a single colored "chip" (small rounded badge with text). The
    /// background is a low-alpha tint of the text color so the chip reads
    /// as a wash rather than a solid block.
    fn build_chip(
        appearance: &Appearance,
        text: String,
        sub: Option<String>,
        text_color: ColorU,
        bg_color: ColorU,
        border_color: ColorU,
    ) -> Box<dyn Element> {
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Text::new(text, appearance.ui_font_family(), 10.)
                    .with_color(text_color)
                    .finish(),
            );
        if let Some(sub) = sub {
            let sub_color = ColorU::new(
                text_color.r,
                text_color.g,
                text_color.b,
                ((text_color.a as u32 * 65) / 100) as u8,
            );
            row = row.with_child(
                Container::new(
                    Text::new(sub, appearance.ui_font_family(), 9.)
                        .with_color(sub_color)
                        .finish(),
                )
                .with_margin_left(4.)
                .finish(),
            );
        }
        Container::new(row.finish())
            .with_background(ThemeFill::Solid(bg_color))
            .with_border(Border::all(1.).with_border_color(border_color))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
            .with_padding_left(6.)
            .with_padding_right(6.)
            .with_padding_top(1.)
            .with_padding_bottom(1.)
            .finish()
    }

    /// Reads Claude's parsed status footer from the alt-screen. Returns
    /// an empty `ParsedStatus` when the alt-screen is inactive (e.g. the
    /// agent isn't running yet) — callers gate the chip row on that.
    fn read_parsed_status(&self, app: &AppContext) -> ParsedStatus {
        let Some(terminal_view) = self.terminal_view.upgrade(app) else {
            return ParsedStatus::default();
        };
        let model = terminal_view.as_ref(app).model.lock();
        if !model.is_alt_screen_active() {
            return ParsedStatus::default();
        }
        let text = model.alt_screen().output_to_string();
        parse_status(&text)
    }

    fn render_status_chips(
        &self,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Option<Box<dyn Element>> {
        let terminal_view = self.terminal_view.upgrade(app)?;
        let view_id = terminal_view.as_ref(app).id();
        let session = CLIAgentSessionsModel::as_ref(app).session(view_id)?;
        let theme = appearance.theme();
        let parsed = self.read_parsed_status(app);
        let mut chips: Vec<Box<dyn Element>> = Vec::new();

        if let Some(mode) = parsed.mode.as_deref() {
            let key = ModeKey::from_label(mode);
            let (text, bg, border) = mode_chip_colors(key, theme);
            chips.push(Self::build_chip(
                appearance,
                mode.to_string(),
                None,
                text,
                bg,
                border,
            ));
        }

        let neutral_bg = theme.surface_2().into_solid();
        let neutral_text = internal_colors::text_main(theme, neutral_bg);
        let neutral_border = internal_colors::neutral_3(theme);
        let (model_text, model_sub) = match parsed.model.as_deref() {
            Some(m) => (m.to_string(), parsed.context_info.clone()),
            None => (
                session.agent.display_name().to_string(),
                session.plugin_version.clone().map(|v| format!("v{v}")),
            ),
        };
        chips.push(Self::build_chip(
            appearance,
            model_text,
            model_sub,
            neutral_text,
            neutral_bg,
            neutral_border,
        ));

        if let Some(n) = parsed.shells {
            let blue = theme.ansi_fg_blue();
            let label = if n == 1 {
                "1 shell".to_string()
            } else {
                format!("{n} shells")
            };
            chips.push(Self::build_chip(
                appearance,
                label,
                None,
                blue,
                with_alpha(blue, 30),
                with_alpha(blue, 90),
            ));
        }

        if parsed.remote {
            let magenta = theme.ansi_fg_magenta();
            chips.push(Self::build_chip(
                appearance,
                "remote".to_string(),
                None,
                magenta,
                with_alpha(magenta, 30),
                with_alpha(magenta, 90),
            ));
        }

        if parsed.focused {
            let green = theme.ansi_fg_green();
            chips.push(Self::build_chip(
                appearance,
                "focus".to_string(),
                None,
                green,
                with_alpha(green, 30),
                with_alpha(green, 90),
            ));
        }

        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_main_axis_alignment(MainAxisAlignment::Start)
            .with_spacing(4.);
        for chip in chips {
            row = row.with_child(chip);
        }

        if let Some(progress) = parsed.progress {
            row = row.with_child(Shrinkable::new(1., Empty::new().finish()).finish());
            row = row.with_child(progress_bar_chip(appearance, progress, theme));
        }

        Some(row.finish())
    }
}

fn mode_chip_colors(key: ModeKey, theme: &WarpTheme) -> (ColorU, ColorU, ColorU) {
    let base = match key {
        ModeKey::Plan => theme.ansi_fg_green(),
        ModeKey::Auto => theme.ansi_fg_yellow(),
        ModeKey::Bypass => theme.ansi_fg_red(),
        ModeKey::Accept => theme.ansi_fg_magenta(),
        ModeKey::Default => internal_colors::text_sub(theme, theme.surface_1().into_solid()),
    };
    (base, with_alpha(base, 30), with_alpha(base, 90))
}

fn progress_bar_chip(
    appearance: &Appearance,
    progress: u8,
    theme: &WarpTheme,
) -> Box<dyn Element> {
    let track_color = internal_colors::neutral_3(theme);
    let bar_fill = theme.accent();
    let pct = progress.min(100) as f32;
    let track_width: f32 = 70.;
    let bar_width = track_width * (pct / 100.);

    let bar = Container::new(Empty::new().finish())
        .with_background(bar_fill)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(2.)))
        .finish();
    let bar_box = ConstrainedBox::new(bar)
        .with_width(bar_width.max(0.))
        .with_height(3.)
        .finish();

    let track_inner = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_alignment(MainAxisAlignment::Start)
        .with_child(bar_box)
        .finish();
    let track = ConstrainedBox::new(
        Container::new(track_inner)
            .with_background(ThemeFill::Solid(track_color))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(2.)))
            .finish(),
    )
    .with_width(track_width)
    .with_height(3.)
    .finish();

    let label_color = internal_colors::text_sub(theme, theme.surface_1().into_solid());
    let label = Text::new(
        format!("{progress}%"),
        appearance.ui_font_family(),
        10.,
    )
    .with_color(label_color)
    .finish();

    Flex::row()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(5.)
        .with_child(track)
        .with_child(label)
        .finish()
}

fn with_alpha(c: ColorU, alpha_pct: u8) -> ColorU {
    let a = ((alpha_pct as u32 * 255) / 100).min(255) as u8;
    ColorU::new(c.r, c.g, c.b, a)
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
        let hint_color = internal_colors::text_sub(theme, bg_fill.into_solid());

        if self.is_hidden {
            return Self::render_hidden_pill(theme, bg_fill, border_color);
        }

        let logo_el = Container::new(Self::render_logo(theme))
            .with_margin_right(8.)
            .finish();

        let editor_el = Container::new(ChildView::new(&self.editor).finish()).finish();

        let row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::Start)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(logo_el)
            .with_child(Shrinkable::new(1., editor_el).finish())
            .finish();

        let hint_text = if self.is_submitting {
            PROMPT_HINT_SUBMITTING
        } else if self.popover.is_some() {
            PROMPT_HINT_POPOVER
        } else {
            PROMPT_HINT
        };
        let hint = FormattedTextElement::from_str(
            hint_text,
            appearance.ui_font_family(),
            HINT_FONT_SIZE,
        )
        .with_color(hint_color)
        .with_line_height_ratio(1.2)
        .finish();

        let mut body_col = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(row)
            .with_child(Container::new(hint).with_margin_top(4.).finish());

        if let Some(chips) = self.render_status_chips(appearance, app) {
            body_col =
                body_col.with_child(Container::new(chips).with_margin_top(6.).finish());
        }



        let body = body_col.finish();

        let padded = Container::new(body)
            .with_padding_left(PROMPT_PADDING_H)
            .with_padding_right(PROMPT_PADDING_H)
            .with_padding_top(PROMPT_PADDING_V)
            .with_padding_bottom(PROMPT_PADDING_V)
            .finish();

        let prompt_container = Container::new(padded)
            .with_background(bg_fill)
            .with_border(Border::all(1.).with_border_color(border_color))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(PROMPT_BORDER_RADIUS)))
            .finish();

        // Click anywhere on the prompt body — including padding outside the
        // editor — focuses the editor. `Continue` so a click that lands on
        // the editor itself still reaches it normally for caret placement.
        let prompt_clickable = EventHandler::new(prompt_container)
            .on_left_mouse_down(|ctx, _, _| {
                ctx.dispatch_typed_action(ClaudePromptOverlayAction::FocusEditor);
                DispatchEventResult::PropagateToParent
            })
            .finish();

        let Some(popover) = self.popover.as_ref() else {
            return prompt_clickable;
        };
        let popover_el = self.render_popover(appearance, popover);
        Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(Container::new(popover_el).with_margin_bottom(4.).finish())
            .with_child(prompt_clickable)
            .finish()
    }
}

impl TypedActionView for ClaudePromptOverlay {
    type Action = ClaudePromptOverlayAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ClaudePromptOverlayAction::Send => self.handle_send(ctx),
            ClaudePromptOverlayAction::Interrupt => {
                ctx.emit(ClaudePromptOverlayEvent::WriteRaw { bytes: vec![0x03] });
            }
            ClaudePromptOverlayAction::CycleMode => {
                ctx.emit(ClaudePromptOverlayEvent::WriteRaw {
                    bytes: vec![0x1b, b'[', b'Z'],
                });
            }
            ClaudePromptOverlayAction::ToggleHide => {
                self.is_hidden = !self.is_hidden;
                ctx.notify();
            }
            ClaudePromptOverlayAction::FocusEditor => {
                ctx.focus(&self.editor);
            }
        }
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
