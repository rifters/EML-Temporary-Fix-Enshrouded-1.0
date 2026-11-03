//! Drives a dedicated server from the in-game panel: whatever the panel would write for the
//! native layer is typed into the game chat as `/m182 <key> ...` lines, which the server's chat
//! channel turns into the same files.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub use crate::remote_proto::{DEFAULT_LIMIT, MAX_LIMIT, MIN_LIMIT};
use crate::remote_proto as proto;

pub const SETTINGS_FILE: &str = "m182_panel.json";
pub const LOG_FILE: &str = "m182_panel.log";
pub const MODE_FILE: &str = "m182_mode.json";
pub const SCAN_FILE: &str = "m182_scan.request";
pub const ONLINE_FILE: &str = "m182_online.json";
pub const CONFIG_KEY: &str = "remote_key";

/// Files the server accepts through chat.
pub const FORWARDED: [&str; 7] = [
    "m182_spawn.request",
    "m182_command.request",
    "m182_powers.json",
    "m182_zones.json",
    "m182_teams.json",
    "m182_spawners.json",
    "m182_spawner_op.json",
];

/// Requests that only make sense on the server: they are not written next to the game.
const REMOTE_ONLY: [&str; 3] = ["m182_spawn.request", "m182_command.request", "m182_spawner_op.json"];

/// Types lines into the game; implemented by the Windows host.
pub trait Typist: Send {
    fn begin(&mut self) -> Result<(), String>;
    fn line(&mut self, text: &str) -> Result<(), String>;
    fn end(&mut self);
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub key: String,
    pub limit: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub queued: usize,
    pub sending: bool,
    pub sent_lines: u64,
    pub last: String,
    pub error: String,
}

struct Job {
    file: String,
    lines: Vec<String>,
    tries: u8,
    sum: Option<(String, u64)>,
}

struct State {
    settings: Settings,
    queue: VecDeque<Job>,
    typist: Option<Box<dyn Typist>>,
    installed: bool,
    worker: bool,
    hosting: Option<bool>,
    status: Status,
}

static HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(!cfg!(test));

/// While held, nothing queued is typed into the server.
pub fn hold(on: bool) {
    HELD.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn held() -> bool {
    HELD.load(std::sync::atomic::Ordering::Relaxed)
}

static FORCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn forced() -> bool {
    FORCE.load(std::sync::atomic::Ordering::Relaxed)
}

static STATE: Mutex<State> = Mutex::new(State {
    settings: Settings { enabled: false, key: String::new(), limit: DEFAULT_LIMIT },
    queue: VecDeque::new(),
    typist: None,
    installed: false,
    worker: false,
    hosting: None,
    status: Status { queued: 0, sending: false, sent_lines: 0, last: String::new(), error: String::new() },
});

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn read_settings(dir: &Path) -> Settings {
    let config: Value = std::fs::read_to_string(dir.join("m182_native.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    let key = config.get(CONFIG_KEY).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let panel: Value = std::fs::read_to_string(dir.join(SETTINGS_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    FORCE.store(panel.get("remote_force").and_then(Value::as_bool).unwrap_or(false), std::sync::atomic::Ordering::Relaxed);
    let enabled = panel.get("remote").and_then(Value::as_bool).unwrap_or_else(|| !key.is_empty());
    let limit = panel.get("remote_limit").and_then(Value::as_u64).map(|l| l as usize).unwrap_or(DEFAULT_LIMIT).clamp(MIN_LIMIT, MAX_LIMIT);
    Settings { enabled, key, limit }
}

pub fn save_settings(dir: &Path, settings: &Settings) -> Result<(), String> {
    let mut config: serde_json::Map<String, Value> = std::fs::read_to_string(dir.join("m182_native.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    config.entry("enabled").or_insert(json!(true));
    config.insert(CONFIG_KEY.to_string(), json!(settings.key.trim()));
    let text = serde_json::to_string_pretty(&Value::Object(config)).map_err(|e| e.to_string())?;
    plain_write(dir, "m182_native.json", &text)?;
    let mut panel: serde_json::Map<String, Value> = std::fs::read_to_string(dir.join(SETTINGS_FILE))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    panel.insert("remote".to_string(), json!(settings.enabled));
    panel.insert("remote_limit".to_string(), json!(settings.limit));
    plain_write(dir, SETTINGS_FILE, &Value::Object(panel).to_string())
}

fn plain_write(dir: &Path, name: &str, text: &str) -> Result<(), String> {
    let temp = dir.join(format!("{name}.tmp"));
    std::fs::write(&temp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, dir.join(name)).map_err(|e| e.to_string())
}

pub fn configure(settings: Settings) {
    state().settings = settings;
}

pub fn settings() -> Settings {
    state().settings.clone()
}

pub fn install_typist(typist: Box<dyn Typist>) {
    let mut s = state();
    s.typist = Some(typist);
    s.installed = true;
    if !s.worker {
        s.worker = true;
        drop(s);
        if std::thread::Builder::new().name("m182-remote".into()).spawn(worker).is_err() {
            let mut s = state();
            s.worker = false;
            s.status.error = "No se pudo iniciar el envío al servidor.".to_string();
        }
    }
}

static LOG_DIR: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);
static LOG_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Starts the panel's own log next to the game files.
pub fn open_log(dir: &Path) {
    let _ = std::fs::write(dir.join(LOG_FILE), "# m182 panel log\n");
    *LOG_DIR.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.to_path_buf());
    let _ = LOG_START.set(Instant::now());
}

pub fn note(message: &str) {
    let Some(dir) = LOG_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
    let at = LOG_START.get().map(|t| t.elapsed().as_millis()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(dir.join(LOG_FILE)) {
        use std::io::Write;
        let _ = writeln!(f, "[{at}ms] {message}");
    }
}

/// Tells the panel whether this game hosts the world itself (then nothing is sent anywhere).
pub fn set_hosting(hosting: Option<bool>) {
    let changed = {
        let mut s = state();
        let changed = s.hosting != hosting;
        s.hosting = hosting;
        changed
    };
    if changed {
        note(&match hosting {
            Some(true) => "mode: this game hosts the world, requests are applied locally".to_string(),
            Some(false) => "mode: connected to a server, requests go through the chat".to_string(),
            None => "mode: unknown".to_string(),
        });
    }
}

/// Reads the mode file the native layer writes once it knows how the game is connected.
pub fn read_hosting(dir: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(dir.join(MODE_FILE)).ok()?;
    serde_json::from_str::<Value>(&text).ok()?.get("hosting")?.as_bool()
}

/// True when the panel is set to drive a server and can type into the game.
pub fn active() -> bool {
    let s = state();
    s.settings.enabled && proto::valid_key(&s.settings.key) && s.installed && (s.hosting != Some(true) || forced())
}

/// True when the panel can type into the game chat (used by the connection test).
pub fn can_type() -> bool {
    let s = state();
    s.settings.enabled && proto::valid_key(&s.settings.key) && s.installed
}

/// How a file the panel wants to write must be handled.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Route {
    Local,
    Both,
    ServerOnly,
}

/// Commands that also change something about the sender on the server.
const SHARED_COMMANDS: [&str; 1] = ["heal"];

/// Like `route`, but looks inside the command file because not every command belongs on the server.
pub fn route_for(name: &str, text: &str) -> Route {
    let route = route(name);
    if route == Route::Local || name != "m182_command.request" {
        return route;
    }
    let cmd = serde_json::from_str::<Value>(text).ok().and_then(|v| v.get("cmd").and_then(Value::as_str).map(str::to_string)).unwrap_or_default();
    if SHARED_COMMANDS.contains(&cmd.as_str()) {
        Route::Both
    } else {
        route
    }
}

/// True when this game is connected to a server and can talk to it through the chat.
pub fn connected() -> bool {
    active() && (state().hosting == Some(false) || forced())
}

pub fn route(name: &str) -> Route {
    if !active() || !FORWARDED.contains(&name) {
        return Route::Local;
    }
    if REMOTE_ONLY.contains(&name) {
        Route::ServerOnly
    } else {
        Route::Both
    }
}

static SENT: Mutex<Option<std::collections::HashMap<String, u64>>> = Mutex::new(None);
pub const SENT_FILE: &str = "m182_sent.json";

fn checksum(key: &str, name: &str, body: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes().chain(std::iter::once(0)).chain(name.bytes()).chain(std::iter::once(0)).chain(body.bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn sent_map<R>(f: impl FnOnce(&mut std::collections::HashMap<String, u64>) -> R) -> R {
    let mut guard = SENT.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(|| {
        let dir = LOG_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone();
        dir.and_then(|d| std::fs::read_to_string(d.join(SENT_FILE)).ok())
            .and_then(|t| serde_json::from_str::<std::collections::HashMap<String, u64>>(&t).ok())
            .unwrap_or_default()
    });
    f(map)
}

fn sent_before(file: &str, value: u64) -> bool {
    sent_map(|m| m.get(file) == Some(&value))
}

fn remember_sent(file: &str, value: u64) {
    let snapshot = sent_map(|m| {
        m.insert(file.to_string(), value);
        m.clone()
    });
    if let Some(dir) = LOG_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        if let Ok(text) = serde_json::to_string(&snapshot) {
            let _ = std::fs::write(dir.join(SENT_FILE), text);
        }
    }
}

/// Forgets what the server is known to have of one file, so the next submit of it is sent again.
pub fn forget_one(file: &str) {
    let snapshot = sent_map(|m| {
        m.remove(file);
        m.clone()
    });
    if let Some(dir) = LOG_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        if let Ok(text) = serde_json::to_string(&snapshot) {
            let _ = std::fs::write(dir.join(SENT_FILE), text);
        }
    }
}

/// Drops the in-memory record so it is read again from disk (used when the profile changes).
pub fn reload_sent() {
    *SENT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Forgets what the server is known to have, so the next submit of every file is sent again.
pub fn forget_sent() {
    sent_map(|m| m.clear());
    if let Some(dir) = LOG_DIR.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        let _ = std::fs::remove_file(dir.join(SENT_FILE));
    }
}

fn push(job: Job, replace: bool) {
    let mut s = state();
    if replace {
        s.queue.retain(|j| j.file != job.file);
    }
    s.queue.push_back(job);
    s.status.queued = s.queue.len();
    s.status.error.clear();
}

/// Queues the file for the server. State files replace an older copy that was not sent yet.
pub fn submit(name: &str, text: &str) -> Result<(), String> {
    let (key, limit) = {
        let s = state();
        (s.settings.key.clone(), s.settings.limit)
    };
    let body = serde_json::from_str::<Value>(text).map_err(|e| format!("{name}: {e}"))?.to_string();
    let replace = !REMOTE_ONLY.contains(&name);
    let sum = if replace { Some((name.to_string(), checksum(&key, name, &body))) } else { None };
    if let Some((file, value)) = &sum {
        let busy = {
            let st = state();
            st.status.sending || st.queue.iter().any(|j| &j.file == file)
        };
        if !busy && sent_before(file, *value) {
            note(&format!("skipped {file}: the server already has this exact content"));
            return Ok(());
        }
    }
    push(Job { file: name.to_string(), lines: proto::compose_best(&key, name, &body, limit), tries: 0, sum }, replace);
    Ok(())
}

pub fn submit_pvp(on: bool) {
    let key = state().settings.key.clone();
    push(Job { file: "pvp".to_string(), lines: vec![proto::pvp(&key, on)], tries: 0, sum: None }, true);
}

pub fn submit_friendly_fire(on: bool) {
    let key = state().settings.key.clone();
    push(Job { file: "friendlyfire".to_string(), lines: vec![proto::friendly_fire(&key, on)], tries: 0, sum: None }, true);
}

pub fn submit_players(seq: u32) {
    let key = state().settings.key.clone();
    push(Job { file: "players".to_string(), lines: vec![proto::players(&key, seq)], tries: 0, sum: None }, true);
}

pub fn submit_teams(seq: u32) {
    let key = state().settings.key.clone();
    push(Job { file: "teams".to_string(), lines: vec![proto::teams(&key, seq)], tries: 0, sum: None }, true);
}

pub fn submit_ping() {
    let key = state().settings.key.clone();
    push(Job { file: "ping".to_string(), lines: vec![proto::ping(&key)], tries: 0, sum: None }, true);
}

pub fn status() -> Status {
    let mut s = state();
    let queued = s.queue.len();
    s.status.queued = queued;
    s.status.clone()
}

pub fn clear_queue() {
    let mut s = state();
    s.queue.clear();
    s.status.queued = 0;
}

const RETRIES: u8 = 2;
const RETRY_PAUSE: Duration = Duration::from_millis(1500);

fn worker() {
    loop {
        std::thread::sleep(Duration::from_millis(150));
        if held() {
            continue;
        }
        let (job, typist) = {
            let mut s = state();
            if s.queue.is_empty() || s.typist.is_none() {
                continue;
            }
            let job = s.queue.pop_front();
            s.status.queued = s.queue.len();
            s.status.sending = true;
            (job, s.typist.take())
        };
        let (Some(job), Some(mut typist)) = (job, typist) else { continue };
        let started = Instant::now();
        note(&format!("sending {} ({} lines)", job.file, job.lines.len()));
        let mut error = String::new();
        let mut sent = 0u64;
        match typist.begin() {
            Ok(()) => {
                for line in &job.lines {
                    match typist.line(line) {
                        Ok(()) => sent += 1,
                        Err(e) => {
                            error = e;
                            break;
                        }
                    }
                }
                typist.end();
            }
            Err(e) => error = e,
        }
        if error.is_empty() {
            note(&format!("sent {} in {:.1}s", job.file, started.elapsed().as_secs_f32()));
        } else {
            note(&format!("failed {}: {error}", job.file));
        }
        let stopped = error.starts_with(crate::host::STOP_MARK);
        let error = error.strip_prefix(crate::host::STOP_MARK).map(|e| e.trim().to_string()).unwrap_or(error);
        let retry = !error.is_empty() && !stopped && job.tries < RETRIES;
        let mut s = state();
        s.typist = Some(typist);
        s.status.sending = false;
        s.status.sent_lines += sent;
        if error.is_empty() {
            if let Some((file, value)) = &job.sum {
                remember_sent(file, *value);
            }
            s.status.last = format!("{} enviado ({} mensaje{}, {:.1} s)", job.file, job.lines.len(), if job.lines.len() == 1 { "" } else { "s" }, started.elapsed().as_secs_f32());
        } else if retry {
            note(&format!("retrying {} (attempt {})", job.file, job.tries + 2));
            s.queue.push_front(Job { tries: job.tries + 1, ..job });
            s.status.queued = s.queue.len();
            drop(s);
            std::thread::sleep(RETRY_PAUSE);
        } else {
            s.status.error = format!("{}: {error}", job.file);
            s.queue.clear();
        }
    }
}

#[cfg(test)]
pub fn reset_for_tests() {
    let mut s = state();
    s.queue.clear();
    s.typist = None;
    s.installed = false;
    s.hosting = None;
    s.settings = Settings { enabled: false, key: String::new(), limit: DEFAULT_LIMIT };
    s.status = Status::default();
    HELD.store(false, std::sync::atomic::Ordering::Relaxed);
    *SENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Default::default());
}

#[cfg(test)]
mod tests {
    #[test]
    fn one_file_can_be_forgotten_so_it_is_sent_again() {
        remember_sent("zz_test_forget.json", 5);
        assert!(sent_before("zz_test_forget.json", 5));
        forget_one("zz_test_forget.json");
        assert!(!sent_before("zz_test_forget.json", 5));
    }

    use super::*;
    use std::sync::Arc;

    static GUARD: Mutex<()> = Mutex::new(());

    struct Recorder(Arc<Mutex<Vec<String>>>, bool);

    impl Typist for Recorder {
        fn begin(&mut self) -> Result<(), String> {
            self.0.lock().unwrap().push("<begin>".into());
            Ok(())
        }
        fn line(&mut self, text: &str) -> Result<(), String> {
            if self.1 && text.contains("6661696c") {
                return Err("sin foco".into());
            }
            self.0.lock().unwrap().push(text.to_string());
            Ok(())
        }
        fn end(&mut self) {
            self.0.lock().unwrap().push("<end>".into());
        }
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("m182_remote_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn wait_idle() {
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(25));
            let st = status();
            if st.queued == 0 && !st.sending {
                return;
            }
        }
        panic!("the sender never went idle");
    }

    #[test]
    fn settings_come_from_the_config_and_the_panel_file() {
        let dir = temp("settings");
        assert_eq!(read_settings(&dir), Settings { enabled: false, key: String::new(), limit: DEFAULT_LIMIT });
        std::fs::write(dir.join("m182_native.json"), r#"{"pvp":true,"remote_key":" abcdef1 "}"#).unwrap();
        assert_eq!(read_settings(&dir), Settings { enabled: true, key: "abcdef1".into(), limit: DEFAULT_LIMIT });
        std::fs::write(dir.join(SETTINGS_FILE), r#"{"remote":false,"remote_limit":5000}"#).unwrap();
        assert_eq!(read_settings(&dir), Settings { enabled: false, key: "abcdef1".into(), limit: MAX_LIMIT });
        save_settings(&dir, &Settings { enabled: true, key: "newkey99".into(), limit: 150 }).unwrap();
        let back = read_settings(&dir);
        assert_eq!(back, Settings { enabled: true, key: "newkey99".into(), limit: 150 });
        let config: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("m182_native.json")).unwrap()).unwrap();
        std::fs::write(dir.join(SETTINGS_FILE), r#"{"remote":true,"chat_open_key":"T","typing_burst":6}"#).unwrap();
        save_settings(&dir, &Settings { enabled: false, key: "newkey99".into(), limit: 150 }).unwrap();
        let panel: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap()).unwrap();
        assert_eq!((panel["chat_open_key"].as_str(), panel["typing_burst"].as_u64(), panel["remote"].as_bool()), (Some("T"), Some(6), Some(false)));
        assert_eq!(config["pvp"], true);
        assert_eq!(config["enabled"], true);
    }

    #[test]
    fn nothing_is_forwarded_unless_the_panel_drives_a_server_and_can_type() {
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_tests();
        assert_eq!(route("m182_spawn.request"), Route::Local);
        configure(Settings { enabled: true, key: "abcdef1".into(), limit: DEFAULT_LIMIT });
        assert_eq!(route("m182_spawn.request"), Route::Local);
        install_typist(Box::new(Recorder(Arc::new(Mutex::new(Vec::new())), false)));
        assert_eq!(route("m182_spawn.request"), Route::ServerOnly);
        assert_eq!(route("m182_command.request"), Route::ServerOnly);
        assert_eq!(route("m182_zones.json"), Route::Both);
        assert_eq!(route("m182_places.json"), Route::Local);
        assert_eq!(route("m182_native.json"), Route::Local);
        configure(Settings { enabled: false, key: "abcdef1".into(), limit: DEFAULT_LIMIT });
        assert_eq!(route("m182_zones.json"), Route::Local);
        configure(Settings { enabled: true, key: "abc".into(), limit: DEFAULT_LIMIT });
        assert_eq!(route("m182_zones.json"), Route::Local);
    }

    #[test]
    fn only_the_commands_that_also_change_the_sender_go_both_ways() {
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_tests();
        configure(Settings { enabled: true, key: "abcdef1".into(), limit: DEFAULT_LIMIT });
        install_typist(Box::new(Recorder(Arc::new(Mutex::new(Vec::new())), false)));
        set_hosting(Some(false));
        assert!(connected());
        let command = "m182_command.request";
        assert_eq!(route_for(command, r#"{"cmd":"item_stats","item_id":1}"#), Route::ServerOnly);
        assert_eq!(route_for(command, r#"{"cmd":"heal"}"#), Route::Both);
        assert_eq!(route_for(command, r#"{"cmd":"item_stats"}"#), Route::ServerOnly);
        assert_eq!(route_for(command, "no es json"), Route::ServerOnly);
        assert_eq!(route_for("m182_zones.json", "{}"), Route::Both);
        set_hosting(Some(true));
        assert!(!connected());
        assert_eq!(route_for(command, r#"{"cmd":"item_stats"}"#), Route::Local);
    }

    #[test]
    fn a_game_that_hosts_its_own_world_never_sends_to_a_server() {
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_tests();
        configure(Settings { enabled: true, key: "abcdef1".into(), limit: DEFAULT_LIMIT });
        install_typist(Box::new(Recorder(Arc::new(Mutex::new(Vec::new())), false)));
        assert_eq!(route("m182_spawn.request"), Route::ServerOnly);
        let dir = temp("mode");
        assert_eq!(read_hosting(&dir), None);
        std::fs::write(dir.join(MODE_FILE), r#"{"hosting":true}"#).unwrap();
        set_hosting(read_hosting(&dir));
        assert_eq!(route("m182_spawn.request"), Route::Local);
        assert_eq!(route("m182_zones.json"), Route::Local);
        std::fs::write(dir.join(MODE_FILE), r#"{"hosting":false}"#).unwrap();
        set_hosting(read_hosting(&dir));
        assert_eq!(route("m182_spawn.request"), Route::ServerOnly);
    }

    #[test]
    fn lines_are_typed_in_order_and_state_files_keep_only_the_latest() {
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_tests();
        let typed = Arc::new(Mutex::new(Vec::new()));
        configure(Settings { enabled: true, key: "abcdef1".into(), limit: DEFAULT_LIMIT });
        // queued before a typist exists: nothing is sent yet
        submit("m182_zones.json", r#"{"zones":[]}"#).unwrap();
        submit("m182_zones.json", r#"{"zones":[{"name":"B"}]}"#).unwrap();
        submit("m182_command.request", r#"{"cmd":"heal"}"#).unwrap();
        submit("m182_command.request", r#"{"cmd":"item_stats"}"#).unwrap();
        assert_eq!(status().queued, 3);
        install_typist(Box::new(Recorder(typed.clone(), false)));
        wait_idle();
        let lines = typed.lock().unwrap().clone();
        let sent: Vec<&String> = lines.iter().filter(|l| l.starts_with('!')).collect();
        assert_eq!(sent.len(), 3, "{lines:?}");
        let hex = |t: &str| t.bytes().map(|b| format!("{b:02x}")).collect::<String>();
        assert!(sent[0].contains(&hex(r#"{"zones":[{"name":"B"}]}"#)));
        assert!(sent[1].contains(&hex("heal")) && sent[2].contains(&hex("item_stats")));
        assert_eq!(lines.iter().filter(|l| *l == "<begin>").count(), 3);
        assert_eq!(lines.iter().filter(|l| *l == "<end>").count(), 3);
        assert!(status().last.contains("m182_command.request"));
    }

    #[test]
    fn a_failure_stops_the_queue_and_is_reported() {
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_tests();
        let typed = Arc::new(Mutex::new(Vec::new()));
        configure(Settings { enabled: true, key: "abcdef1".into(), limit: DEFAULT_LIMIT });
        install_typist(Box::new(Recorder(typed.clone(), true)));
        submit("m182_command.request", r#"{"cmd":"fail"}"#).unwrap();
        submit("m182_command.request", r#"{"cmd":"heal"}"#).unwrap();
        wait_idle();
        std::thread::sleep(Duration::from_millis(200));
        let st = status();
        assert!(st.error.contains("sin foco"), "{st:?}");
        assert!(!typed.lock().unwrap().iter().any(|l| l.contains("6865616c")));
        assert!(submit("m182_zones.json", "not json").is_err());
    }
}
