//! Per-workspace persistence for Claude Code prompt history.
//!
//! Stored as JSON-lines (one submission per line) under the platform's
//! local-data dir. The workspace key is hashed so the on-disk filename is
//! ASCII and length-bounded — collisions are practically impossible at the
//! ~200-entry-per-pane scale we operate at.

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

const HISTORY_DIR: &str = "warp/cli_agent_history";
const HISTORY_FILE_VERSION: u32 = 1;

#[derive(Debug)]
pub struct HistoryStore {
    path: PathBuf,
    /// Soft cap. `append` writes one line; `trim_to` rewrites the file
    /// when it overshoots, so we don't grow without bound across long
    /// project lifetimes.
    max_entries: usize,
}

impl HistoryStore {
    /// Returns a store keyed by `workspace_key` (typically the pane's
    /// `pwd`). `None` if the platform doesn't expose a local-data dir,
    /// which shouldn't happen on any supported OS but is handled
    /// defensively so the overlay still works.
    pub fn for_workspace(workspace_key: &str, max_entries: usize) -> Option<Self> {
        let base = dirs::data_local_dir()?.join(HISTORY_DIR);
        let _ = fs::create_dir_all(&base);
        let mut hasher = DefaultHasher::new();
        workspace_key.hash(&mut hasher);
        let hash = hasher.finish();
        let path = base.join(format!("{hash:016x}.jsonl"));
        Some(Self { path, max_entries })
    }

    pub fn load(&self) -> Vec<String> {
        let Ok(file) = File::open(&self.path) else {
            return Vec::new();
        };
        let reader = BufReader::new(file);
        let mut out = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&line) {
                if let Some(Value::String(text)) = map.get("text") {
                    out.push(text.clone());
                }
            }
        }
        if out.len() > self.max_entries {
            let drop = out.len() - self.max_entries;
            out.drain(..drop);
        }
        out
    }

    pub fn append(&self, entry: &str) -> std::io::Result<()> {
        let line = json!({ "v": HISTORY_FILE_VERSION, "text": entry }).to_string();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Rewrites the file with the given entries when the on-disk count
    /// drifts past `max_entries`. Cheap enough at our scale that we don't
    /// bother with a separate compactor.
    pub fn rewrite(&self, entries: &[String]) -> std::io::Result<()> {
        let mut file = File::create(&self.path)?;
        for entry in entries.iter() {
            let line = json!({ "v": HISTORY_FILE_VERSION, "text": entry }).to_string();
            writeln!(file, "{line}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str, max: usize) -> HistoryStore {
        let base = std::env::temp_dir().join(format!("warp-history-test-{name}"));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let path = base.join("h.jsonl");
        HistoryStore { path, max_entries: max }
    }

    #[test]
    fn append_then_load_roundtrips() {
        let s = temp_store("roundtrip", 10);
        s.append("hello").unwrap();
        s.append("world").unwrap();
        assert_eq!(s.load(), vec!["hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn load_caps_to_max() {
        let s = temp_store("cap", 2);
        for i in 0..5 {
            s.append(&format!("e{i}")).unwrap();
        }
        let got = s.load();
        assert_eq!(got, vec!["e3".to_string(), "e4".to_string()]);
    }

    #[test]
    fn rewrite_replaces_contents() {
        let s = temp_store("rewrite", 10);
        s.append("old").unwrap();
        s.rewrite(&["a".into(), "b".into()]).unwrap();
        assert_eq!(s.load(), vec!["a".to_string(), "b".to_string()]);
    }
}
