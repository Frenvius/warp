//! Curated list of Claude Code slash commands and known model aliases.
//!
//! The built-in set is small, well-known, and rarely changes — keeping it
//! static lets the popover work offline. User and plugin commands are
//! discovered at runtime by `dynamic_commands` and merged on top.

use super::dynamic_commands::{discover_plugin_commands, discover_user_commands};

#[derive(Debug, Clone, Copy)]
pub struct SlashEntry {
    pub name: &'static str,
    pub desc: &'static str,
    pub aliases: &'static [&'static str],
}

/// Owned form of a popover entry. Built-ins are converted from
/// `SlashEntry` at runtime so they share a uniform type with the entries
/// discovered from `~/.claude/commands/` and plugin install paths.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandEntry {
    pub name: String,
    pub desc: String,
    pub aliases: Vec<String>,
}

impl From<&SlashEntry> for CommandEntry {
    fn from(s: &SlashEntry) -> Self {
        Self {
            name: s.name.to_string(),
            desc: s.desc.to_string(),
            aliases: s.aliases.iter().map(|a| a.to_string()).collect(),
        }
    }
}

/// Built-in Claude Code slash commands. Mirrors waveterm's curated list
/// so users get the same feature parity.
pub const SLASH_COMMANDS: &[SlashEntry] = &[
    SlashEntry { name: "/clear", desc: "New conversation", aliases: &["/reset", "/new"] },
    SlashEntry { name: "/compact", desc: "Compact conversation with optional focus", aliases: &[] },
    SlashEntry { name: "/resume", desc: "Resume a session or open picker", aliases: &[] },
    SlashEntry { name: "/branch", desc: "Fork current conversation", aliases: &[] },
    SlashEntry { name: "/rename", desc: "Rename current session", aliases: &[] },
    SlashEntry { name: "/exit", desc: "Exit the CLI", aliases: &[] },
    SlashEntry { name: "/model", desc: "Select/change AI model", aliases: &[] },
    SlashEntry { name: "/effort", desc: "Set effort level (low/medium/high/max)", aliases: &[] },
    SlashEntry { name: "/config", desc: "Open Settings interface", aliases: &[] },
    SlashEntry { name: "/fast", desc: "Toggle fast mode", aliases: &[] },
    SlashEntry { name: "/theme", desc: "Change color theme", aliases: &[] },
    SlashEntry { name: "/plan", desc: "Enter plan mode", aliases: &[] },
    SlashEntry { name: "/diff", desc: "View uncommitted changes", aliases: &[] },
    SlashEntry { name: "/rewind", desc: "Rewind conversation/code to prior point", aliases: &[] },
    SlashEntry { name: "/review", desc: "Review a pull request", aliases: &[] },
    SlashEntry { name: "/context", desc: "Visualize context usage", aliases: &[] },
    SlashEntry { name: "/cost", desc: "Show token usage stats", aliases: &[] },
    SlashEntry { name: "/usage", desc: "Show plan limits/rate limits", aliases: &[] },
    SlashEntry { name: "/help", desc: "Show help", aliases: &[] },
    SlashEntry { name: "/doctor", desc: "Diagnose installation", aliases: &[] },
    SlashEntry { name: "/init", desc: "Initialize CLAUDE.md", aliases: &[] },
    SlashEntry { name: "/memory", desc: "Edit memory files", aliases: &[] },
    SlashEntry { name: "/permissions", desc: "Manage tool permissions", aliases: &[] },
    SlashEntry { name: "/skills", desc: "List available skills", aliases: &[] },
    SlashEntry { name: "/mcp", desc: "Manage MCP server connections", aliases: &[] },
    SlashEntry { name: "/hooks", desc: "View hook configurations", aliases: &[] },
    SlashEntry { name: "/simplify", desc: "Code review for quality/efficiency", aliases: &[] },
    SlashEntry { name: "/loop", desc: "Run a prompt repeatedly", aliases: &[] },
    SlashEntry { name: "/schedule", desc: "Create/manage scheduled tasks", aliases: &[] },
    SlashEntry { name: "/security-review", desc: "Security review of pending changes", aliases: &[] },
];

/// Known Claude model aliases used as completions for `/model <name>`.
pub const MODELS: &[SlashEntry] = &[
    SlashEntry { name: "default", desc: "Recommended default model", aliases: &[] },
    SlashEntry { name: "opus", desc: "Latest Claude Opus", aliases: &[] },
    SlashEntry { name: "sonnet", desc: "Latest Claude Sonnet", aliases: &[] },
    SlashEntry { name: "haiku", desc: "Latest Claude Haiku", aliases: &[] },
    SlashEntry { name: "opusplan", desc: "Opus for planning, Sonnet for execution", aliases: &[] },
    SlashEntry { name: "claude-opus-4-7", desc: "Opus 4.7 — most capable", aliases: &[] },
    SlashEntry { name: "claude-opus-4-6", desc: "Opus 4.6", aliases: &[] },
    SlashEntry { name: "claude-opus-4-5", desc: "Opus 4.5", aliases: &[] },
    SlashEntry { name: "claude-sonnet-4-6", desc: "Sonnet 4.6", aliases: &[] },
    SlashEntry { name: "claude-sonnet-4-5", desc: "Sonnet 4.5", aliases: &[] },
    SlashEntry { name: "claude-haiku-4-5", desc: "Haiku 4.5 — fastest", aliases: &[] },
];

pub fn builtin_commands() -> Vec<CommandEntry> {
    SLASH_COMMANDS.iter().map(CommandEntry::from).collect()
}

pub fn builtin_models() -> Vec<CommandEntry> {
    MODELS.iter().map(CommandEntry::from).collect()
}

/// Built-ins + user `~/.claude/commands/` + plugin commands. Built-ins win
/// on name conflict so a user file can't shadow `/clear`. Sorted by name
/// for stable popover ordering.
pub fn all_commands() -> Vec<CommandEntry> {
    let mut out = builtin_commands();
    let existing: std::collections::HashSet<String> =
        out.iter().map(|e| e.name.clone()).collect();
    for entry in discover_user_commands()
        .into_iter()
        .chain(discover_plugin_commands())
    {
        if !existing.contains(&entry.name) {
            out.push(entry);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[derive(Debug, Clone, PartialEq)]
pub enum PopoverTrigger {
    Slash { query: String },
    Model { query: String },
}

/// Inspects the editor buffer text and returns the active completion
/// trigger, if any.
///
/// - `/<word>` (no whitespace) → slash command popover
/// - `/model ` or `/model <word>` → model name popover
/// - anything else → no popover
pub fn detect_trigger(text: &str) -> Option<PopoverTrigger> {
    if let Some(rest) = text.strip_prefix("/model ") {
        if !rest.contains(char::is_whitespace) {
            return Some(PopoverTrigger::Model {
                query: rest.to_string(),
            });
        }
    }
    if text.starts_with('/') && !text.contains(char::is_whitespace) {
        return Some(PopoverTrigger::Slash {
            query: text[1..].to_string(),
        });
    }
    None
}

/// Returns clones of entries from `pool` whose name / desc / aliases
/// contain the `query` (case-insensitive). Empty query returns the full
/// pool. Cloning keeps the call sites simple at the cost of a few hundred
/// short string clones per keystroke — negligible in this UI.
pub fn filter_entries(pool: &[CommandEntry], query: &str) -> Vec<CommandEntry> {
    if query.is_empty() {
        return pool.to_vec();
    }
    let q = query.to_lowercase();
    pool.iter()
        .filter(|e| {
            e.name.to_lowercase().contains(&q)
                || e.desc.to_lowercase().contains(&q)
                || e.aliases.iter().any(|a| a.to_lowercase().contains(&q))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_slash_trigger() {
        assert_eq!(
            detect_trigger("/he"),
            Some(PopoverTrigger::Slash { query: "he".into() })
        );
        assert_eq!(
            detect_trigger("/"),
            Some(PopoverTrigger::Slash { query: "".into() })
        );
    }

    #[test]
    fn detects_model_trigger() {
        assert_eq!(
            detect_trigger("/model "),
            Some(PopoverTrigger::Model { query: "".into() })
        );
        assert_eq!(
            detect_trigger("/model opus"),
            Some(PopoverTrigger::Model { query: "opus".into() })
        );
    }

    #[test]
    fn no_trigger_for_plain_text() {
        assert_eq!(detect_trigger("hello"), None);
        assert_eq!(detect_trigger("/help and more"), None);
        assert_eq!(detect_trigger(""), None);
    }

    #[test]
    fn filter_matches_name_and_desc() {
        let pool = builtin_commands();
        let r = filter_entries(&pool, "model");
        assert!(r.iter().any(|e| e.name == "/model"));

        let r = filter_entries(&pool, "memory");
        assert!(r.iter().any(|e| e.name == "/memory"));

        let r = filter_entries(&pool, "compact");
        assert!(r.iter().any(|e| e.name == "/compact"));
    }

    #[test]
    fn filter_matches_alias() {
        let pool = builtin_commands();
        let r = filter_entries(&pool, "/reset");
        assert!(r.iter().any(|e| e.name == "/clear"));
    }
}
