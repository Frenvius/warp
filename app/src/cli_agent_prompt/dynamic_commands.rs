//! Discovers user-defined and plugin-defined Claude Code slash commands at
//! runtime. The Claude Code CLI keeps user commands in `~/.claude/commands/`
//! and plugin commands under each plugin's `installPath/commands/` (paths
//! tracked in `~/.claude/plugins/installed_plugins.json`). We surface both
//! in the popover alongside the curated built-ins.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::slash_commands::CommandEntry;

const USER_COMMAND_DESC: &str = "User command";
const PLUGIN_COMMAND_DESC: &str = "Plugin command";

/// `~/.claude` (or `$CLAUDE_HOME` if set). Mirrors the resolution rule used
/// by the rest of warp's Claude integration.
fn claude_home_dir() -> Option<PathBuf> {
    if let Ok(claude_home) = env::var("CLAUDE_HOME") {
        return Some(PathBuf::from(claude_home));
    }
    dirs::home_dir().map(|home| home.join(".claude"))
}

pub fn discover_user_commands() -> Vec<CommandEntry> {
    let Some(dir) = claude_home_dir().map(|d| d.join("commands")) else {
        return Vec::new();
    };
    discover_commands_in_dir(&dir, None)
}

pub fn discover_plugin_commands() -> Vec<CommandEntry> {
    let Some(home) = claude_home_dir() else {
        return Vec::new();
    };
    let manifest = home.join("plugins").join("installed_plugins.json");
    let Ok(text) = fs::read_to_string(&manifest) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let Some(Value::Object(plugins)) = parsed.get("plugins").cloned() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for (key, entries) in plugins {
        // Plugin key is `<name>@<marketplace>`; the slash-command namespace
        // prefix is the segment before `@`. If there's no `@`, fall back to
        // the whole key.
        let namespace = key.split('@').next().unwrap_or(&key).to_string();
        let Value::Array(arr) = entries else {
            continue;
        };
        for entry in arr {
            let Some(install_path) = entry
                .get("installPath")
                .and_then(Value::as_str)
                .map(PathBuf::from)
            else {
                continue;
            };
            let cmd_dir = install_path.join("commands");
            out.extend(discover_commands_in_dir(&cmd_dir, Some(&namespace)));
        }
    }
    out
}

/// Walks `dir` for `*.md` files and turns each into a `CommandEntry`.
/// `prefix`, if set, namespaces the slash command (`/{prefix}:{stem}`).
fn discover_commands_in_dir(dir: &Path, prefix: Option<&str>) -> Vec<CommandEntry> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.extension().map(|e| e == "md").unwrap_or(false) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let name = match prefix {
            Some(p) => format!("/{p}:{stem}"),
            None => format!("/{stem}"),
        };
        let desc = read_description(&path).unwrap_or_else(|| {
            if prefix.is_some() {
                PLUGIN_COMMAND_DESC.to_string()
            } else {
                USER_COMMAND_DESC.to_string()
            }
        });
        out.push(CommandEntry {
            name,
            desc,
            aliases: Vec::new(),
        });
    }
    out
}

/// Pulls the `description:` field from a YAML-style frontmatter block at
/// the top of the file. Returns `None` if no frontmatter or no description.
fn read_description(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "---" {
        return None;
    }
    for line in lines {
        if line == "---" {
            return None;
        }
        if let Some(rest) = line.strip_prefix("description:") {
            let trimmed = rest.trim().trim_matches(|c: char| c == '"' || c == '\'');
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("warp-dyncmds-{name}"));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn parses_frontmatter_description() {
        let dir = temp_dir("frontmatter");
        let path = dir.join("foo.md");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "---\ndescription: Do the foo thing\nallowed-tools: Read\n---\nbody").unwrap();
        assert_eq!(
            read_description(&path).as_deref(),
            Some("Do the foo thing")
        );
    }

    #[test]
    fn discovers_user_commands_in_dir() {
        let dir = temp_dir("user-commands");
        let mut f = File::create(dir.join("alpha.md")).unwrap();
        writeln!(f, "---\ndescription: Alpha\n---\n").unwrap();
        let mut f = File::create(dir.join("beta.md")).unwrap();
        writeln!(f, "no frontmatter").unwrap();

        let mut got = discover_commands_in_dir(&dir, None);
        got.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "/alpha");
        assert_eq!(got[0].desc, "Alpha");
        assert_eq!(got[1].name, "/beta");
        assert_eq!(got[1].desc, USER_COMMAND_DESC);
    }

    #[test]
    fn discovers_plugin_commands_with_namespace() {
        let dir = temp_dir("plugin-commands");
        let mut f = File::create(dir.join("setup.md")).unwrap();
        writeln!(f, "---\ndescription: Set it up\n---\n").unwrap();
        let got = discover_commands_in_dir(&dir, Some("claude-hud"));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "/claude-hud:setup");
    }

    #[test]
    fn ignores_non_markdown_files() {
        let dir = temp_dir("non-md");
        File::create(dir.join("x.txt")).unwrap();
        File::create(dir.join("y.json")).unwrap();
        assert!(discover_commands_in_dir(&dir, None).is_empty());
    }
}
