//! Minimal .env reader for `$env:NAME` indirection in config.jsonc.
//!
//! Process environment always wins; `<config_dir>/.env` is the fallback so
//! keys written by moss-setup work without shell profile changes. The file
//! is re-read when its mtime changes. Values are never written back into the
//! process environment: `std::env::set_var` is unsound in a multithreaded
//! host process (the engine may be embedded inside kitty).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

#[derive(Default)]
struct CacheEntry {
    mtime: Option<SystemTime>,
    values: HashMap<String, String>,
}

static CACHE: Mutex<Option<HashMap<PathBuf, CacheEntry>>> = Mutex::new(None);

fn parse(content: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        let mut value = value.trim();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = &value[1..value.len() - 1];
        }
        out.insert(key.to_string(), value.to_string());
    }
    out
}

/// Look up `name` in `<config_dir>/.env`. Returns None when the file or key
/// is absent or unreadable.
pub fn lookup(config_dir: &Path, name: &str) -> Option<String> {
    let path = config_dir.join(".env");
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let mut guard = CACHE.lock().ok()?;
    let cache = guard.get_or_insert_with(HashMap::new);
    let entry = cache.entry(path.clone()).or_default();
    if entry.mtime != mtime || (mtime.is_some() && entry.values.is_empty()) {
        entry.mtime = mtime;
        entry.values = match std::fs::read_to_string(&path) {
            Ok(content) => parse(&content),
            Err(_) => HashMap::new(),
        };
    }
    entry.values.get(name).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quotes_comments_and_export() {
        let map = parse(
            "# comment\nexport FOO=bar\nBAZ=\"quoted value\"\nQUX='single'\nBAD LINE\n=nokey\nSP = spaced \n",
        );
        assert_eq!(map.get("FOO").unwrap(), "bar");
        assert_eq!(map.get("BAZ").unwrap(), "quoted value");
        assert_eq!(map.get("QUX").unwrap(), "single");
        assert_eq!(map.get("SP").unwrap(), "spaced");
        assert_eq!(map.len(), 4);
    }

    #[test]
    fn lookup_reads_and_tracks_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".env");
        assert_eq!(lookup(dir.path(), "KEY"), None);
        std::fs::write(&env_path, "KEY=one\n").unwrap();
        // Force a different mtime than the "absent" state.
        assert_eq!(lookup(dir.path(), "KEY").as_deref(), Some("one"));
        std::fs::write(&env_path, "KEY=two\n").unwrap();
        let bumped = std::time::SystemTime::now() + std::time::Duration::from_secs(2);
        let _ = filetime_set(&env_path, bumped);
        assert_eq!(lookup(dir.path(), "KEY").as_deref(), Some("two"));
    }

    fn filetime_set(path: &Path, to: SystemTime) -> std::io::Result<()> {
        let file = std::fs::OpenOptions::new().append(true).open(path)?;
        file.set_modified(to)
    }
}
