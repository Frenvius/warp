//! Parses Claude Code's TUI status footer out of the alt-screen's rendered
//! text. The alt-screen (post-parse cell grid) is the source of truth on
//! Windows, where ConPTY emits diff-only byte updates that an upstream
//! byte-stream tap can't reassemble into intact bracketed strings.

use once_cell::sync::Lazy;
use regex::Regex;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ParsedStatus {
    pub model: Option<String>,
    pub context_info: Option<String>,
    pub progress: Option<u8>,
    pub mode: Option<String>,
    pub shells: Option<u32>,
    pub remote: bool,
    pub focused: bool,
    /// True when the alt-screen contains Claude's input box
    /// (`╭...╮` / `╰...╯` borders). When false the user is on a menu /
    /// confirmation screen rather than the prompt.
    pub has_input_box: bool,
    /// True when the alt-screen looks like one of Claude's selection
    /// screens: "resume session", model picker, "(N of M)" pagination, etc.
    pub menu_mode: bool,
}

impl ParsedStatus {
    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.context_info.is_none()
            && self.progress.is_none()
            && self.mode.is_none()
            && self.shells.is_none()
            && !self.remote
            && !self.focused
    }

    /// True when the overlay should auto-hide so it doesn't overlap
    /// Claude's TUI. Mirrors waveterm's `questionMode` heuristic: either
    /// a recognizable menu screen, OR no input box AND no status footer
    /// (which signals a confirmation prompt taking over the whole TUI).
    pub fn is_question_mode(&self) -> bool {
        if self.menu_mode {
            return true;
        }
        let has_status_marker = self.model.is_some();
        let has_focus_marker = self.focused;
        !self.has_input_box && !has_status_marker && !has_focus_marker
    }
}

/// Categorical mode key derived from the free-form mode label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeKey {
    Plan,
    Auto,
    Bypass,
    Accept,
    Default,
}

impl ModeKey {
    pub fn from_label(mode: &str) -> Self {
        let m = mode.to_lowercase();
        if m.contains("plan") {
            ModeKey::Plan
        } else if m.contains("auto") {
            ModeKey::Auto
        } else if m.contains("bypass") {
            ModeKey::Bypass
        } else if m.contains("accept") {
            ModeKey::Accept
        } else {
            ModeKey::Default
        }
    }
}

static MODEL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[\s*([A-Z][\w\.\s-]+?)\s*(?:\(([^)]+)\))?\s*\]").unwrap());
static PROGRESS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(\d{1,3})\s*%").unwrap());
static MODE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)([A-Za-z][A-Za-z \t]*?(?:mode)?)[ \t]+on[ \t]+\(\S+[ \t]+to[ \t]+cycle\)")
        .unwrap()
});
static SHELLS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(\d+)\s+shells?\b").unwrap());
static REMOTE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)remote\s+control\s+active").unwrap());
static FOCUS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\bfocus\b").unwrap());
/// Recognizes Claude's interactive selection screens (resume session, model
/// picker, switch-to dialogs, etc.) and any pagination footer. These are
/// the only TUI states where the prompt is replaced wholesale rather than
/// just minimized to a footer.
static MENU_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(resume session|select an?|switch to|choose)\b|\(\s*\d+\s+of\s+\d+\s*\)",
    )
    .unwrap()
});

pub fn parse_status(text: &str) -> ParsedStatus {
    if text.is_empty() {
        return ParsedStatus::default();
    }
    let mut result = ParsedStatus::default();

    // Last `[Model (context)]` occurrence wins. The alt-screen text is laid
    // out top-to-bottom, so the bottom-most footer line is the latest.
    let mut last_bracket_pos: Option<usize> = None;
    for caps in MODEL_RE.captures_iter(text) {
        let Some(full) = caps.get(0) else { continue };
        last_bracket_pos = Some(full.start());
        result.model = caps.get(1).map(|m| m.as_str().trim().to_string());
        result.context_info = caps.get(2).map(|m| m.as_str().trim().to_string());
    }

    // Progress lives on the same line as the bracket footer.
    if let Some(start) = last_bracket_pos {
        let line_end = text[start..]
            .find('\n')
            .map(|i| start + i)
            .unwrap_or(text.len());
        let line = &text[start..line_end];
        if let Some(pcaps) = PROGRESS_RE.captures_iter(line).last() {
            if let Some(m) = pcaps.get(1) {
                if let Ok(n) = m.as_str().parse::<u16>() {
                    if n <= 100 {
                        result.progress = Some(n as u8);
                    }
                }
            }
        }
    }

    if let Some(caps) = MODE_RE.captures_iter(text).last() {
        if let Some(m) = caps.get(1) {
            let mode = m
                .as_str()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            let mode = mode.trim_end_matches(" mode").trim().to_string();
            if !mode.is_empty() {
                result.mode = Some(mode);
            }
        }
    }

    if let Some(caps) = SHELLS_RE.captures_iter(text).last() {
        if let Some(m) = caps.get(1) {
            if let Ok(n) = m.as_str().parse::<u32>() {
                result.shells = Some(n);
            }
        }
    }

    result.remote = REMOTE_RE.is_match(text);
    result.focused = FOCUS_RE.is_match(text);

    // Box detection: a complete prompt input frame has both a top border
    // (containing `╭` and `╮`) and a bottom border (`╰` and `╯`) on
    // separate lines. Selection screens render a search field with the
    // same characters but no `[Model ...]` footer above it, so this flag
    // alone isn't enough — see `is_question_mode()`.
    let mut has_top_border = false;
    let mut has_bottom_border = false;
    for line in text.lines() {
        if !has_top_border && line.contains('╭') && line.contains('╮') {
            has_top_border = true;
        }
        if !has_bottom_border && line.contains('╰') && line.contains('╯') {
            has_bottom_border = true;
        }
        if has_top_border && has_bottom_border {
            break;
        }
    }
    result.has_input_box = has_top_border && has_bottom_border;

    result.menu_mode = MENU_RE.is_match(text);

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_last_model_when_multiple_in_text() {
        let text = "[Sonnet 4.6 (200k context)]  10%\nlater\n[Opus 4.7 (1M context)]  42%\n";
        let parsed = parse_status(text);
        assert_eq!(parsed.model.as_deref(), Some("Opus 4.7"));
        assert_eq!(parsed.context_info.as_deref(), Some("1M context"));
        assert_eq!(parsed.progress, Some(42));
    }

    #[test]
    fn picks_last_mode_when_multiple_in_text() {
        let text = "auto mode on (shift+tab to cycle)\nlater\nplan mode on (shift+tab to cycle)\n";
        let parsed = parse_status(text);
        assert_eq!(parsed.mode.as_deref(), Some("plan"));
    }

    #[test]
    fn extracts_shell_count() {
        assert_eq!(parse_status("auto mode on  · 1 shell").shells, Some(1));
        assert_eq!(parse_status("auto mode on  · 3 shells").shells, Some(3));
    }

    #[test]
    fn detects_remote_control() {
        assert!(parse_status("[Opus 4.7]  14%        Remote Control active").remote);
        assert!(!parse_status("[Opus 4.7]  14%").remote);
    }

    #[test]
    fn detects_input_box_borders() {
        let with_box = "\
╭───────────╮
│ > what's up │
╰───────────╯
[Opus 4.7]  0%";
        let parsed = parse_status(with_box);
        assert!(parsed.has_input_box);
        assert!(!parsed.is_question_mode());

        let without = "[Opus 4.7]  0%";
        assert!(!parse_status(without).has_input_box);
    }

    #[test]
    fn detects_menu_screens() {
        let menu = "\
Resume session?
  - chat 1
  - chat 2
(2 of 2)";
        let parsed = parse_status(menu);
        assert!(parsed.menu_mode);
        assert!(parsed.is_question_mode());
    }

    #[test]
    fn empty_screen_is_question_mode() {
        // No box, no model, no focus → looks like a confirmation prompt.
        assert!(parse_status("Are you sure? (y/n)").is_question_mode());
    }
}
