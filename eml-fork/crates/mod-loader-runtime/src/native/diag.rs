//! Diagnostic log (`m182_diag.log`), on only while `m182_diag.json` next to the game says
//! `{"enabled": true}`. It records which game functions ask for a player's permissions so the
//! building, chest and door rules can be located without guessing.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const CONFIG_FILE: &str = "m182_diag.json";
pub const LOG_FILE: &str = "m182_diag.log";
const MAX_LOG_BYTES: u64 = 6_000_000;

static ON: AtomicBool = AtomicBool::new(false);
static CHECKED: Mutex<Option<Instant>> = Mutex::new(None);
static SEEN: Mutex<Option<HashMap<(usize, u32, u32), (u32, Instant)>>> = Mutex::new(None);

pub fn enabled_in(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("enabled").and_then(|e| e.as_bool()))
        .unwrap_or(false)
}

/// True while the diagnostic file asks for logging; the file is looked at every few seconds.
pub fn on() -> bool {
    let now = Instant::now();
    if let Ok(mut checked) = CHECKED.try_lock() {
        if checked.map(|t| now.duration_since(t) >= Duration::from_secs(3)).unwrap_or(true) {
            *checked = Some(now);
            let on = std::fs::read_to_string(CONFIG_FILE).map(|t| enabled_in(&t)).unwrap_or(false);
            ON.store(on, Ordering::Relaxed);
        }
    }
    ON.load(Ordering::Relaxed)
}

fn stamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{:02}:{:02}:{:02}", secs / 3600 % 24, secs / 60 % 60, secs % 60)
}

pub fn line(text: &str) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    if std::fs::metadata(LOG_FILE).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false) {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(LOG_FILE) {
        let _ = writeln!(f, "{} UTC {}", stamp(), text);
    }
}

/// A marker typed in the chat (`!m182 <key> mark <text>`), to tell the actions apart in the log.
pub fn mark(text: &str) {
    on();
    line(&format!("=== MARK {}", text.trim()));
}

/// One permission lookup: who asked (entity), what the game stored and what it will see, and the
/// address (relative to the game image) of the code that asked.
pub fn permission(entity: u32, stored: u32, shown: u32, caller: usize) {
    let now = Instant::now();
    let Ok(mut guard) = SEEN.try_lock() else { return };
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() > 600 {
        map.clear();
    }
    let entry = map.entry((caller, entity, stored)).or_insert((0, now - Duration::from_secs(60)));
    entry.0 += 1;
    if entry.0 == 1 || now.duration_since(entry.1) >= Duration::from_secs(2) {
        let count = entry.0;
        entry.0 = 0;
        entry.1 = now;
        drop(guard);
        line(&format!(
            "perm entity={entity} stored={stored:#04x} shown={shown:#04x} caller=+{caller:#x} x{count}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_diagnostic_file_must_say_enabled() {
        assert!(enabled_in(r#"{"enabled": true}"#));
        assert!(!enabled_in(r#"{"enabled": false}"#));
        assert!(!enabled_in("{}"));
        assert!(!enabled_in("not json"));
    }
}
