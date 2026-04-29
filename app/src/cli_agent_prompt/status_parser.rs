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
}
