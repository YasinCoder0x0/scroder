use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::util;

const MAX_ENTRIES: usize = 60;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HistoryEntry {
    pub path: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub duration_secs: f64,
    #[serde(default)]
    pub size: u64,
}

fn default_kind() -> String {
    "video".to_string()
}

impl HistoryEntry {
    pub fn new(path: &Path, kind: &str, duration_secs: f64) -> Self {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        Self {
            path: path.to_string_lossy().to_string(),
            kind: kind.to_string(),
            created: util::unix_now(),
            duration_secs,
            size,
        }
    }

    pub fn path_buf(&self) -> PathBuf {
        PathBuf::from(&self.path)
    }

    pub fn file_name(&self) -> String {
        Path::new(&self.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.clone())
    }
}

fn history_path() -> Option<PathBuf> {
    util::config_dir().map(|dir| dir.join("history.json"))
}

pub fn load() -> Vec<HistoryEntry> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    let mut entries: Vec<HistoryEntry> = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    entries.retain(|entry| Path::new(&entry.path).exists());
    entries
}

pub fn save(entries: &[HistoryEntry]) {
    let Some(path) = history_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(entries) {
        let _ = std::fs::write(path, text);
    }
}

pub fn add(mut entries: Vec<HistoryEntry>, entry: HistoryEntry) -> Vec<HistoryEntry> {
    entries.retain(|existing| existing.path != entry.path);
    entries.insert(0, entry);
    entries.truncate(MAX_ENTRIES);
    save(&entries);
    entries
}
