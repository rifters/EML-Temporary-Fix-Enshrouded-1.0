use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::SystemTime,
};

use serde::Deserialize;

pub const CONFIG_FILE: &str = "m182_native.json";
pub const DEFAULT_TEMPLATE: &str = "642098d4-fc09-48e2-811e-782d438bf551";

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NativeConfig {
    /// Master switch. When false nothing in the game is touched.
    pub enabled: bool,
    /// Players are hostile to each other (all other relations stay vanilla).
    pub pvp: bool,
    /// Also apply the player-vs-player rule to the "friend" checks.
    pub pvp_friend_checks: bool,
    /// With pvp on, members of the same team can hurt each other too.
    pub pvp_friendly_fire: bool,
    pub spawn_probe: bool,
    pub spawner: bool,
    pub spawn_key: String,
    pub spawn_template: String,
    pub spawn_level: u32,
    pub spawn_count: u32,
    pub spawn_distance: f32,
    pub spawn_health: u32,
    pub spawn_damage: f32,
    pub dump_player_key: String,
    pub dump_spawned_key: String,
    pub watch_key: String,
    /// Secret for the chat command channel (`/m182 <key> ...`). Empty = channel off.
    pub chat_key: String,
    /// Load `M182_Panel.dll` (the in-game panel, Insert) when it is next to the game.
    pub panel: bool,
    /// Test switch: act on the character shown on screen even when this game also hosts the world.
    pub force_client: bool,
    /// Only players with the kick/ban permission may use the chat command channel.
    pub chat_admin_only: bool,
    /// Administrators build without spending materials.
    pub free_building: bool,
    /// On the game client, the hammer works away from any Flame Altar.
    pub client_build_anywhere: bool,
}

impl Default for NativeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            free_building: true,
            client_build_anywhere: true,
            pvp: false,
            pvp_friend_checks: false,
            pvp_friendly_fire: false,
            spawn_probe: false,
            spawner: false,
            spawn_key: "F8".to_string(),
            spawn_template: DEFAULT_TEMPLATE.to_string(),
            spawn_level: 20,
            spawn_count: 1,
            spawn_distance: 5.0,
            spawn_health: 0,
            spawn_damage: 1.0,
            dump_player_key: "F7".to_string(),
            dump_spawned_key: "F6".to_string(),
            watch_key: "F5".to_string(),
            chat_key: String::new(),
            panel: true,
            force_client: false,
            chat_admin_only: true,
        }
    }
}

pub fn load(path: &Path) -> Result<NativeConfig, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(NativeConfig::default()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Settings the hooks read on every call.
pub struct LiveState {
    pub pvp: AtomicBool,
    pub pvp_friend_checks: AtomicBool,
    pub spawn_probe: AtomicBool,
    pub spawner: AtomicBool,
    pub force_client: AtomicBool,
    pub chat_admin_only: AtomicBool,
    pub free_building: AtomicBool,
    pub client_build_anywhere: AtomicBool,
    pub chat_key: std::sync::Mutex<String>,
}

#[derive(Debug, Clone)]
pub struct SpawnSettings {
    pub template: String,
    pub level: u32,
    pub count: u32,
    pub distance: f32,
    pub health: u32,
    pub damage: f32,
    pub key: i32,
    pub dump_player_key: i32,
    pub dump_spawned_key: i32,
    pub watch_key: i32,
}

pub static SPAWN: std::sync::Mutex<SpawnSettings> = std::sync::Mutex::new(SpawnSettings {
    template: String::new(),
    level: 20,
    count: 1,
    distance: 5.0,
    health: 0,
    damage: 1.0,
    key: 0x77,
    dump_player_key: 0x76,
    dump_spawned_key: 0x75,
    watch_key: 0x74,
});

pub fn parse_guid(text: &str) -> Option<[u8; 16]> {
    let t = text.trim().trim_start_matches('{').trim_end_matches('}');
    let parts: Vec<&str> = t.split('-').collect();
    if parts.len() != 5 || parts.iter().zip([8usize, 4, 4, 4, 12]).any(|(p, n)| p.len() != n) {
        return None;
    }
    let mut raw = Vec::with_capacity(16);
    for p in &parts {
        for i in (0..p.len()).step_by(2) {
            raw.push(u8::from_str_radix(&p[i..i + 2], 16).ok()?);
        }
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&[raw[3], raw[2], raw[1], raw[0]]);
    out[4..6].copy_from_slice(&[raw[5], raw[4]]);
    out[6..8].copy_from_slice(&[raw[7], raw[6]]);
    out[8..16].copy_from_slice(&raw[8..16]);
    Some(out)
}

pub fn parse_key(text: &str) -> Option<i32> {
    let t = text.trim();
    if let Some(n) = t.strip_prefix(['F', 'f']) {
        if let Ok(n) = n.parse::<i32>() {
            if (1..=12).contains(&n) {
                return Some(0x6F + n);
            }
        }
    }
    if let Ok(n) = t.parse::<i32>() {
        return Some(n);
    }
    let mut chars = t.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) if ch.is_ascii_alphanumeric() => Some(ch.to_ascii_uppercase() as i32),
        _ => None,
    }
}

pub static LIVE: LiveState = LiveState {
    pvp: AtomicBool::new(false),
    pvp_friend_checks: AtomicBool::new(false),
    spawn_probe: AtomicBool::new(false),
    spawner: AtomicBool::new(false),
    force_client: AtomicBool::new(false),
    chat_admin_only: AtomicBool::new(true),
    free_building: AtomicBool::new(true),
    client_build_anywhere: AtomicBool::new(true),
    chat_key: std::sync::Mutex::new(String::new()),
};

impl LiveState {
    pub fn apply(&self, cfg: &NativeConfig) {
        self.pvp.store(cfg.enabled && cfg.pvp, Ordering::Relaxed);
        self.pvp_friend_checks
            .store(cfg.enabled && cfg.pvp && cfg.pvp_friend_checks, Ordering::Relaxed);
        super::faction::set_friendly_fire(cfg.enabled && cfg.pvp && cfg.pvp_friendly_fire);
        super::faction::set_friendly_fire_setting(cfg.enabled && cfg.pvp_friendly_fire);
        self.spawn_probe.store(cfg.enabled && cfg.spawn_probe, Ordering::Relaxed);
        self.spawner.store(cfg.enabled && cfg.spawner, Ordering::Relaxed);
        self.force_client.store(cfg.enabled && cfg.force_client, Ordering::Relaxed);
        self.chat_admin_only.store(cfg.chat_admin_only, Ordering::Relaxed);
        self.free_building.store(cfg.enabled && cfg.free_building, Ordering::Relaxed);
        self.client_build_anywhere.store(cfg.enabled && cfg.client_build_anywhere, Ordering::Relaxed);

        let chat_key = cfg.chat_key.trim();
        let usable = cfg.enabled && super::chat::valid_key(chat_key);
        if !chat_key.is_empty() && !usable {
            tracing::warn!(target: "m182::native", "chat_key must have at least {} characters and no spaces; chat commands stay off", super::chat::MIN_KEY_LEN);
        }
        if let Ok(mut k) = self.chat_key.lock() {
            *k = if usable { chat_key.to_string() } else { String::new() };
        }

        let key = parse_key(&cfg.spawn_key);
        if key.is_none() {
            tracing::warn!(target: "m182::native", value = %cfg.spawn_key, "spawn_key is not valid, keeping the previous one");
        }
        if let Ok(mut s) = SPAWN.lock() {
            s.template = cfg.spawn_template.trim().to_string();
            if let Some(k) = key {
                s.key = k;
            }
            if let Some(k) = parse_key(&cfg.dump_player_key) {
                s.dump_player_key = k;
            }
            if let Some(k) = parse_key(&cfg.dump_spawned_key) {
                s.dump_spawned_key = k;
            }
            s.level = cfg.spawn_level;
            s.count = cfg.spawn_count.clamp(1, 20);
            s.distance = cfg.spawn_distance.clamp(1.0, 60.0);
            s.health = cfg.spawn_health.min(2_000_000_000);
            s.damage = cfg.spawn_damage.clamp(0.0, 1000.0);
            s.watch_key = parse_key(&cfg.watch_key).unwrap_or(s.watch_key);
        }
    }

    pub fn chat_key(&self) -> String {
        self.chat_key.lock().map(|k| k.clone()).unwrap_or_default()
    }

    #[inline]
    pub fn spawner(&self) -> bool {
        self.spawner.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn client_build_anywhere(&self) -> bool {
        self.client_build_anywhere.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn free_building(&self) -> bool {
        self.free_building.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn chat_admin_only(&self) -> bool {
        self.chat_admin_only.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn force_client(&self) -> bool {
        self.force_client.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn spawn_probe(&self) -> bool {
        self.spawn_probe.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn pvp(&self) -> bool {
        self.pvp.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn pvp_friend_checks(&self) -> bool {
        self.pvp_friend_checks.load(Ordering::Relaxed)
    }
}

pub fn config_path(dir: &Path) -> PathBuf {
    dir.join(CONFIG_FILE)
}
