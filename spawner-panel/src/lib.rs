pub mod embed;
pub mod host;
pub mod input;
pub mod rawguard;
mod emblems;
mod events;
mod icons;
mod items;
mod powers;
pub mod profile;
pub mod remote;
mod server;
pub mod remote_proto;
mod session;
mod social;
mod spawners;
mod steam;
mod theme;
mod wiki;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, RichText, Rounding, Sense, Stroke, TextureHandle, Vec2};
use serde_json::{json, Map, Value};

use theme::*;

pub const WINDOW_TITLE: &str = "M182 Admin Panel";
const FALLBACK_GAME_DIR: &str = r"D:\SteamLibrary\steamapps\common\Enshrouded";
const CONFIG_FILE: &str = "m182_native.json";
const REQUEST_FILE: &str = "m182_spawn.request";
const REQUEST_TMP: &str = "m182_spawn.request.tmp";
const PLAYER_FILE: &str = "m182_player.json";
const PLACES_FILE: &str = "m182_places.json";
const DONATE_URL: &str = "https://www.paypal.me/MoradoGames";

struct Entry {
    guid: String,
    raw: String,
    label: String,
    alias: Option<String>,
    icon_file: Option<String>,
    group: String,
    boss: bool,
    search: String,
    kind: emblems::Kind,
    internal: bool,
    icon_keys: Vec<String>,
    meta: String,
}

#[derive(Default)]
struct Catalog {
    entries: Vec<Entry>,
    error: Option<String>,
    selected: Option<usize>,
    search: String,
    group: String,
    only_bosses: bool,
    show_internal: bool,
}

impl Catalog {
    fn visible(&self) -> Vec<usize> {
        let query = self.search.trim().to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                (self.group.is_empty() || e.group == self.group)
                    && (!self.only_bosses || e.boss)
                    && (self.show_internal || !e.internal)
                    && (query.is_empty() || e.search.contains(&query))
            })
            .map(|(i, _)| i)
            .collect()
    }
}

#[derive(Clone)]
pub struct Place {
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(PartialEq, Clone, Copy)]
pub enum Tab {
    Creatures,
    Items,
    Powers,
    Pvp,
    SafeZones,
    Teams,
    Events,
    Places,
    Settings,
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Creatures,
    Items,
}

#[derive(PartialEq, Clone, Copy)]
pub enum Target {
    Around,
    Place,
}

enum Status {
    Idle,
    Waiting(Instant),
    Delivered,
    NoAnswer,
    Error(String),
}

pub struct App {
    profiles: Vec<String>,
    profile: String,
    profile_new: String,
    profile_notice: String,
    dir: PathBuf,
    tab: Tab,
    creatures: Catalog,
    item_cat: Catalog,
    items: items::ItemsState,
    downloader: Option<icons::Downloader>,
    powers: powers::PowersState,
    social: social::SocialState,
    ev: events::EventState,
    level: u32,
    count: u32,
    distance: f32,
    custom_health: bool,
    health: u32,
    damage: f32,
    pvp: bool,
    pvp_friends: bool,
    pvp_friendly_fire: bool,
    target: Target,
    place_index: usize,
    respawn_minutes: u32,
    spawners: Vec<spawners::Spawner>,
    spawner_status: HashMap<String, spawners::Live>,
    spawner_checked: Instant,
    places: Vec<Place>,
    new_place: String,
    player: Option<[f64; 3]>,
    player_checked: Instant,
    here: Option<(social::Here, Instant)>,
    synced: bool,
    confirmed: bool,
    icons: HashMap<String, Option<TextureHandle>>,
    status: Status,
    notice: String,
    spawner_notice: String,
    creature_drops: Vec<spawners::Drop>,
    remote_cfg: remote::Settings,
    remote_notice: String,
}

fn faction_label(raw: &str) -> &str {
    match raw {
        "Ancients" => "Ancestrales",
        "Cyclops" => "Cíclope",
        "Drak" => "Drak",
        "Fogger" => "Niebla",
        "HitAll" => "Hostiles a todos",
        "HitOthers" => "Hostiles a otros",
        "Scavenger" => "Carroñeros",
        "Skeleton" => "Esqueletos",
        "Vukah" => "Vukah",
        "WildBeast" => "Bestias",
        "Wildlife" => "Fauna",
        other => other,
    }
}

fn group_color(raw: &str) -> Color32 {
    match raw {
        "Ancients" => Color32::from_rgb(132, 186, 204),
        "Cyclops" => Color32::from_rgb(214, 144, 98),
        "Drak" => Color32::from_rgb(124, 192, 118),
        "Fogger" => Color32::from_rgb(174, 140, 214),
        "HitAll" => Color32::from_rgb(222, 108, 100),
        "HitOthers" => Color32::from_rgb(214, 124, 160),
        "Scavenger" => Color32::from_rgb(210, 180, 104),
        "Skeleton" => Color32::from_rgb(206, 202, 188),
        "Vukah" => Color32::from_rgb(104, 188, 184),
        "WildBeast" => Color32::from_rgb(192, 154, 106),
        other => {
            const PALETTE: [[u8; 3]; 9] = [
                [214, 170, 78],
                [132, 200, 150],
                [150, 190, 110],
                [196, 160, 120],
                [240, 190, 90],
                [170, 140, 214],
                [132, 186, 204],
                [214, 144, 98],
                [190, 170, 150],
            ];
            let hash = other.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
            let c = PALETTE[hash as usize % PALETTE.len()];
            Color32::from_rgb(c[0], c[1], c[2])
        }
    }
}

fn pretty(name: &str) -> String {
    let mut n = name;
    for prefix in ["Base_Enemy_", "Base_", "Enemy_"] {
        if let Some(rest) = n.strip_prefix(prefix) {
            n = rest;
            break;
        }
    }
    let mut out = String::new();
    let mut prev: Option<char> = None;
    for c in n.chars() {
        if c == '_' {
            out.push(' ');
        } else {
            if let Some(p) = prev {
                if c.is_uppercase() && (p.is_lowercase() || p.is_ascii_digit()) {
                    out.push(' ');
                }
            }
            out.push(c);
        }
        prev = Some(c);
    }
    out
}

fn is_internal(raw: &str) -> bool {
    let n = raw.to_lowercase();
    n.starts_with("base_")
        || ["collider", "projectilespawner", "togglestate", "hookpt"].iter().any(|w| n.contains(w))
        || ["sleep", "spawntube", "spawn_tube", "parent", "group", "channel", "organ", "quickattack", "quick_attack", "_add", "dummy", "debug", "trigger", "placeholder", "test"]
            .iter()
            .any(|w| n.contains(w))
}

fn apply_wiki(entries: &mut [Entry], wiki: &std::collections::HashMap<String, wiki::WikiEntry>) {
    for e in entries.iter_mut() {
        let Some(w) = wiki.get(&e.raw.to_lowercase()) else { continue };
        if e.alias.is_none() {
            e.alias = Some(w.name.clone());
        }
        if !w.file.is_empty() {
            e.icon_file = Some(format!("wiki/{}", w.file));
        }
        e.search = format!("{} {}", e.search, w.name.to_lowercase());
    }
    entries.sort_by(|a, b| {
        let ka = a.alias.as_deref().unwrap_or(&a.label).to_lowercase();
        let kb = b.alias.as_deref().unwrap_or(&b.label).to_lowercase();
        ka.cmp(&kb)
    });
}

pub fn find_game_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("M182_GAME_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.join("enshrouded.exe").exists() {
                return dir.to_path_buf();
            }
        }
    }
    PathBuf::from(FALLBACK_GAME_DIR)
}

fn load_enemies(dir: &Path) -> Result<Vec<Entry>, String> {
    let path = dir.join("export").join("m182_enemies.csv");
    let text = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "No encuentro la lista de enemigos ({}). Abre el juego una vez con el mod m182_spawn_variants para generarla.",
            path.display()
        )
    })?;
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 5 || !parts[4].trim().is_empty() {
            continue;
        }
        let label = pretty(parts[0]);
        let group = parts[2].trim().to_string();
        let game_name = parts.get(5).map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
        out.push(Entry {
            guid: parts[1].trim().to_string(),
            raw: parts[0].to_string(),
            search: format!("{} {} {}", label, faction_label(&group), game_name.clone().unwrap_or_default()).to_lowercase(),
            kind: emblems::kind_of(parts[0], &group),
            internal: is_internal(parts[0]),
            icon_keys: enemy_icon_keys(parts[0]),
            meta: String::new(),
            alias: game_name,
            icon_file: None,
            label,
            group,
            boss: parts[3].trim() == "true",
        });
    }
    out.sort_by(|a, b| a.alias.as_deref().unwrap_or(&a.label).to_lowercase().cmp(&b.alias.as_deref().unwrap_or(&b.label).to_lowercase()));
    if out.is_empty() {
        return Err("La lista de enemigos está vacía.".to_string());
    }
    Ok(out)
}

fn enemy_icon_keys(raw: &str) -> Vec<String> {
    let mut keys = icons::enemy_keys(raw);
    for k in icons::derived_keys(raw, false) {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys
}

fn index_item(rows: &[items::Item], index: usize) -> Option<&items::Item> {
    rows.get(index)
}

fn item_labels(rows: &[items::Item], keys: &[Vec<String>], titles: &HashMap<String, String>) -> Vec<String> {
    let found: Vec<Option<String>> = keys.iter().map(|k| icons::display_name(titles, k)).collect();
    let mut uses: HashMap<String, usize> = HashMap::new();
    for t in found.iter().flatten() {
        *uses.entry(t.clone()).or_insert(0) += 1;
    }
    rows.iter()
        .zip(found)
        .map(|(it, title)| {
            let raw = items::friendly_name(&it.debug);
            if it.name != it.debug && !it.name.trim().is_empty() {
                return it.name.clone();
            }
            match title {
                Some(t) if uses.get(&t).copied().unwrap_or(0) > 1 => format!("{t} · {raw}"),
                Some(t) => t,
                None => raw,
            }
        })
        .collect()
}

fn item_entries(rows: &[items::Item], titles: &HashMap<String, String>) -> Vec<Entry> {
    let keys: Vec<Vec<String>> = rows.iter().map(|it| icons::item_keys(&it.name, &it.debug, &it.category)).collect();
    let labels = item_labels(rows, &keys, titles);
    rows.iter()
        .zip(keys)
        .zip(labels)
        .map(|((it, keys), label)| Entry {
            guid: it.guid.clone(),
            raw: it.debug.clone(),
            search: format!("{} {} {} {} {}", label, it.name, it.debug, it.group, it.slot).to_lowercase(),
            label,
            alias: None,
            icon_file: None,
            group: it.group.clone(),
            boss: false,
            kind: item_kind(&it.group),
            internal: items::is_internal_item(&it.debug, &it.category),
            icon_keys: keys,
            meta: items::item_meta(it),
        })
        .collect()
}

fn load_item_catalog(dir: &Path) -> (Catalog, Vec<items::Item>) {
    match items::load_items(dir) {
        Ok(rows) => {
            let entries = item_entries(&rows, &icons::load_titles(dir));
            (Catalog { entries, ..Default::default() }, rows)
        }
        Err(e) => (Catalog { error: Some(e), ..Default::default() }, Vec::new()),
    }
}

fn item_kind(group: &str) -> emblems::Kind {
    match group {
        "Armas" | "Armas a distancia" | "Munición" => emblems::Kind::Fang,
        "Armaduras" | "Ropa cosmética" => emblems::Kind::Helm,
        "Escudos" => emblems::Kind::Shield,
        "Gemas de arma" | "Materiales" | "Anillos" | "Tintes" => emblems::Kind::Gem,
        "Comida y pociones" | "Comida de animales" => emblems::Kind::Tree,
        "Herramientas" | "Equipo y accesorios" => emblems::Kind::Cog,
        _ => emblems::Kind::Crate,
    }
}

fn read_config(dir: &Path) -> Map<String, Value> {
    std::fs::read_to_string(dir.join(CONFIG_FILE))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

pub(crate) fn write_local(dir: &Path, name: &str, tmp: &str, text: &str) -> Result<(), String> {
    let tmp_path = dir.join(tmp);
    std::fs::write(&tmp_path, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp_path, dir.join(name)).map_err(|e| e.to_string())
}

/// Writes a file for the native layer; when the panel drives a server, the files the server
/// understands are also (or only) typed into the game chat.
pub fn write_atomic(dir: &Path, name: &str, tmp: &str, text: &str) -> Result<(), String> {
    match remote::route_for(name, text) {
        remote::Route::Local => write_local(dir, name, tmp, text),
        remote::Route::Both => {
            write_local(dir, name, tmp, text)?;
            remote::submit(name, text)
        }
        remote::Route::ServerOnly => remote::submit(name, text),
    }
}

fn read_player(dir: &Path) -> Option<[f64; 3]> {
    let value: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(PLAYER_FILE)).ok()?).ok()?;
    Some([value.get("x")?.as_f64()?, value.get("y")?.as_f64()?, value.get("z")?.as_f64()?])
}

fn load_places(dir: &Path) -> Vec<Place> {
    let Ok(text) = std::fs::read_to_string(dir.join(PLACES_FILE)) else { return Vec::new() };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&text) else { return Vec::new() };
    items
        .iter()
        .filter_map(|v| {
            Some(Place {
                name: v.get("name")?.as_str()?.to_string(),
                x: v.get("x")?.as_f64()?,
                y: v.get("y")?.as_f64()?,
                z: v.get("z")?.as_f64()?,
            })
        })
        .collect()
}

fn load_icon(ctx: &egui::Context, dir: &Path, entry: &Entry) -> Option<TextureHandle> {
    let base = dir.join("export").join("m182_icons");
    let mut paths = Vec::new();
    if let Some(file) = &entry.icon_file {
        paths.push(base.join(file));
    }
    for key in &entry.icon_keys {
        paths.push(icons::wiki_file(dir, key));
    }
    paths.push(base.join(format!("{}.png", entry.guid)));
    for path in paths {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let Ok(decoded) = image::load_from_memory(&bytes) else { continue };
        let image = decoded.to_rgba8();
        let size = [image.width() as usize, image.height() as usize];
        let color = egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw());
        return Some(ctx.load_texture(format!("icon-{}", entry.guid), color, egui::TextureOptions::LINEAR));
    }
    None
}

fn group_label(mode: Mode, group: &str) -> String {
    match mode {
        Mode::Creatures => faction_label(group).to_string(),
        Mode::Items => group.to_string(),
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, dir: PathBuf) -> Self {
        theme::install(&cc.egui_ctx);
        let remote_cfg = remote::read_settings(&dir);
        remote::open_log(&dir);
        remote::configure(remote_cfg.clone());
        let cfg = read_config(&dir);
        let wiki_index = wiki::load(&dir);
        let creatures = match load_enemies(&dir) {
            Ok(mut entries) => {
                apply_wiki(&mut entries, &wiki_index);
                Catalog { entries, ..Default::default() }
            }
            Err(e) => Catalog { error: Some(e), ..Default::default() },
        };
        let (item_cat, item_rows) = load_item_catalog(&dir);
        let (saved_stats, saved_balance) = items::load_saved(&dir);
        let balance_base = items::load_balance(&dir);
        let mut balance = balance_base.clone();
        for (k, v) in saved_balance {
            balance.insert(k, v);
        }
        let mut item_state = items::ItemsState {
            levels: items::load_levels(&dir),
            rows: item_rows,
            balance_base,
            balance,
            sent: saved_stats.clone(),
            saved: saved_stats,
            level: 0,
            rarity: 0,
            amount: 1,
            generic_pickup: items::load_generic_pickup(&dir),
            direct: true,
            ..Default::default()
        };
        if let Ok(log) = std::fs::read_to_string(dir.join("m182_spawner.log")) {
            item_state.adopt_logged_deliveries(&log);
        }
        if dir.join("enshrouded.exe").exists() {
            let _ = item_state.persist_local(&dir);
        }
        let get_u = |key: &str, default: u32| cfg.get(key).and_then(|v| v.as_u64()).map(|v| v as u32).unwrap_or(default);
        let get_f = |key: &str, default: f32| cfg.get(key).and_then(|v| v.as_f64()).map(|v| v as f32).unwrap_or(default);
        let get_b = |key: &str| cfg.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
        let health = get_u("spawn_health", 0);
        let places = load_places(&dir);
        #[cfg(feature = "linux-preview")]
        let preview_tab = match std::env::var("M182_PREVIEW_TAB").as_deref() {
            Ok("places") => Tab::Places,
            Ok("pvp") => Tab::Pvp,
            Ok("teams") => Tab::Teams,
            Ok("zones") => Tab::SafeZones,
            Ok("events") => Tab::Events,
            Ok("items") => Tab::Items,
            Ok("powers") => Tab::Powers,
            Ok("settings") => Tab::Settings,
            _ => Tab::Creatures,
        };
        #[cfg(not(feature = "linux-preview"))]
        let preview_tab = Tab::Creatures;
        #[allow(unused_mut)]
        let mut app = Self {
            tab: preview_tab,
            creatures,
            item_cat,
            items: item_state,
            downloader: Some(icons::Downloader::start(&dir)),
            powers: powers::PowersState::load(&dir),
            social: social::SocialState::load(&dir),
            ev: events::EventState::default(),
            level: get_u("spawn_level", 20).clamp(1, 100),
            count: get_u("spawn_count", 1).clamp(1, 20),
            distance: get_f("spawn_distance", 5.0).clamp(1.0, 60.0),
            custom_health: health > 0,
            health: if health > 0 { health } else { 1000 },
            damage: get_f("spawn_damage", 1.0).clamp(0.0, 20.0),
            pvp: get_b("pvp"),
            pvp_friends: get_b("pvp_friend_checks"),
            pvp_friendly_fire: get_b("pvp_friendly_fire"),
            target: Target::Around,
            place_index: 0,
            respawn_minutes: spawners::DEFAULT_RESPAWN_MINUTES,
            spawners: spawners::load(&dir),
            spawner_status: spawners::load_status(&dir),
            spawner_checked: Instant::now(),
            player: read_player(&dir),
            player_checked: Instant::now(),
            creature_drops: Vec::new(),
            here: None,
            synced: false,
            confirmed: false,
            places,
            new_place: String::new(),
            icons: HashMap::new(),
            status: Status::Idle,
            notice: String::new(),
            spawner_notice: String::new(),
            remote_cfg: remote_cfg.clone(),
            remote_notice: String::new(),
            profiles: profile::list(&dir),
            profile: profile::active(&dir),
            profile_new: String::new(),
            profile_notice: String::new(),
            dir,
        };
        #[cfg(feature = "linux-preview")]
        if let Ok(q) = std::env::var("M182_PREVIEW_SEARCH") {
            app.creatures.search = q.clone();
            app.item_cat.search = q;
        }
        #[cfg(feature = "linux-preview")]
        if std::env::var("M182_PREVIEW_SELECT").is_ok() {
            app.creatures.selected = Some(120.min(app.creatures.entries.len().saturating_sub(1)));
            app.item_cat.selected = Some(40.min(app.item_cat.entries.len().saturating_sub(1)));
        }
        app.cleanup_stale_request();
        app
    }

    fn profile_gate(&mut self, ui: &mut egui::Ui) {
        if !remote::active() || self.confirmed {
            return;
        }
        let mut chosen: Option<String> = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("PERFIL").color(BAD).size(14.5));
            ui.label(RichText::new("Nada se envía al servidor hasta que confirmes el perfil de este servidor:").color(GOLD_DIM).size(14.5));
            egui::ComboBox::from_id_salt("profile_gate").width(200.0).selected_text(self.profile.clone()).show_ui(ui, |ui| {
                for name in &self.profiles {
                    if ui.selectable_label(*name == self.profile, name).clicked() {
                        chosen = Some(name.clone());
                    }
                }
            });
            if button(ui, "CONFIRMAR", Vec2::new(120.0, 24.0), true, true) {
                self.confirmed = true;
            }
        });
        if let Some(name) = chosen {
            self.change_profile(&name);
        }
    }

    fn change_profile(&mut self, name: &str) {
        match profile::switch(&self.dir, name) {
            Ok(active) => {
                remote::clear_queue();
                remote::reload_sent();
                self.social = social::SocialState::load(&self.dir);
                self.spawners = spawners::load(&self.dir);
                self.places = load_places(&self.dir);
                self.ev = events::EventState::default();
                self.place_index = 0;
                self.profile = active.clone();
                self.profiles = profile::list(&self.dir);
                self.profile_notice = format!("Perfil activo: {active}");
            }
            Err(e) => self.profile_notice = format!("No se pudo cambiar de perfil: {e}"),
        }
    }

    fn cleanup_stale_request(&self) {
        let _ = std::fs::remove_file(self.dir.join(REQUEST_FILE));
    }

    fn effective_health(&self) -> u32 {
        if self.custom_health {
            self.health
        } else {
            0
        }
    }

    fn catalog(&self, mode: Mode) -> &Catalog {
        match mode {
            Mode::Creatures => &self.creatures,
            Mode::Items => &self.item_cat,
        }
    }

    fn set_config(&self, entries: &[(&str, Value)]) -> Result<(), String> {
        let mut cfg = read_config(&self.dir);
        for (key, value) in entries {
            cfg.insert((*key).to_string(), value.clone());
        }
        cfg.entry("enabled").or_insert(json!(true));
        let text = serde_json::to_string_pretty(&Value::Object(cfg)).map_err(|e| e.to_string())?;
        write_atomic(&self.dir, CONFIG_FILE, "m182_native.json.tmp", &text)?;
        if remote::active() {
            if let Some(on) = entries.iter().find(|(key, _)| *key == "pvp").and_then(|(_, v)| v.as_bool()) {
                remote::submit_pvp(on);
            }
            if let Some(on) = entries.iter().find(|(key, _)| *key == "pvp_friendly_fire").and_then(|(_, v)| v.as_bool()) {
                remote::submit_friendly_fire(on);
            }
        }
        Ok(())
    }

    fn save_places(&self) {
        let items: Vec<Value> = self.places.iter().map(|p| json!({"name": p.name, "x": p.x, "y": p.y, "z": p.z})).collect();
        if let Ok(text) = serde_json::to_string_pretty(&Value::Array(items)) {
            let _ = write_atomic(&self.dir, PLACES_FILE, "m182_places.json.tmp", &text);
        }
    }

    /// Creates a creature spawner on the chosen place: it appears there, and comes back every few
    /// minutes after it is killed (and every time the game starts, since summoned creatures are not saved).
    fn create_spawner(&mut self) {
        let Some(index) = self.creatures.selected else { return };
        let Some(entry) = self.creatures.entries.get(index) else { return };
        let (creature, template) = (entry.alias.clone().unwrap_or_else(|| entry.label.clone()), entry.guid.clone());
        let Some(place) = self.places.get(self.place_index).cloned() else {
            self.status = Status::Error("Elige un lugar guardado.".to_string());
            return;
        };
        let spawner = spawners::Spawner {
            id: spawners::new_id(),
            creature,
            template,
            place: place.name.clone(),
            at: [place.x, place.y, place.z],
            level: self.level,
            count: self.count,
            health: self.effective_health(),
            damage: self.damage,
            radius: if self.count > 1 { self.distance } else { 3.0 },
            respawn: self.respawn_minutes.clamp(1, 1440) * 60,
            once: false,
            drops: self.creature_drops.clone(),
            pickup: if self.creature_drops.is_empty() { String::new() } else { self.items.generic_pickup.clone() },
            pool: Vec::new(),
            every: 0,
            area: 0.0,
        };
        let op = spawners::op_add(&spawner);
        self.spawners.push(spawner);
        self.status = match spawners::save_change(&self.dir, &self.spawners, &op) {
            Ok(()) => Status::Delivered,
            Err(e) => Status::Error(e),
        };
        self.spawner_notice = format!("Aparición creada en «{}» (cada {} min).", place.name, self.respawn_minutes);
    }

    /// Summons creatures around the player that drop the chosen loot: a one-time spawner on the
    /// player's position, which is how raids carry their loot.
    fn summon_with_loot(&mut self) {
        let Some(index) = self.creatures.selected else { return };
        let Some(entry) = self.creatures.entries.get(index) else { return };
        let (creature, template) = (entry.alias.clone().unwrap_or_else(|| entry.label.clone()), entry.guid.clone());
        let Some(at) = self.player else {
            self.status = Status::Error("No se conoce tu posición. Entra al mundo y espera unos segundos.".to_string());
            return;
        };
        let spawner = spawners::Spawner {
            id: spawners::new_id(),
            creature,
            template,
            place: "Cerca de ti".to_string(),
            at,
            level: self.level,
            count: self.count,
            health: self.effective_health(),
            damage: self.damage,
            radius: self.distance,
            respawn: 300,
            once: true,
            drops: self.creature_drops.clone(),
            pickup: self.items.generic_pickup.clone(),
            pool: Vec::new(),
            every: 0,
            area: 0.0,
        };
        let op = spawners::op_add(&spawner);
        self.spawners.push(spawner);
        self.status = match spawners::save_change(&self.dir, &self.spawners, &op) {
            Ok(()) => Status::Delivered,
            Err(e) => Status::Error(e),
        };
    }

    fn delete_spawner(&mut self, id: &str) {
        self.spawners.retain(|s| s.id != id);
        self.status = match spawners::save_change(&self.dir, &self.spawners, &spawners::op_delete(id)) {
            Ok(()) => Status::Idle,
            Err(e) => Status::Error(e),
        };
        self.spawner_notice = "Aparición borrada.".to_string();
    }

    fn spawn(&mut self, mode: Mode) {
        let Some(index) = self.catalog(mode).selected else { return };
        let guid = self.catalog(mode).entries[index].guid.clone();
        if mode == Mode::Items {
            if let Some(item) = self.items.rows.get(index).cloned() {
                if self.items.note_delivery(&item, self.items.level) {
                    let dir = self.dir.clone();
                    if let Err(e) = self.items.persist(&dir) {
                        self.status = Status::Error(e);
                        return;
                    }
                }
                if self.items.dirty.contains_key(&item.id) {
                    let dir = self.dir.clone();
                    if let Err(e) = self.items.apply_item(&dir, &item) {
                        self.status = Status::Error(e);
                        return;
                    }
                }
            }
        }
        let saved = match mode {
            Mode::Creatures => self.set_config(&[
                ("spawn_template", json!(guid)),
                ("spawn_level", json!(self.level)),
                ("spawn_count", json!(self.count)),
                ("spawn_distance", json!(self.distance)),
                ("spawn_health", json!(self.effective_health())),
                ("spawn_damage", json!(self.damage)),
                ("spawner", json!(true)),
            ]),
            Mode::Items => Ok(()),
        };
        let result = saved.and_then(|_| {
            let mut request = json!({
                "template": guid,
                "count": self.count,
                "distance": self.distance,
            });
            if mode == Mode::Creatures {
                request["level"] = json!(self.level);
                request["health"] = json!(self.effective_health());
                request["damage"] = json!(self.damage);
            } else {
                request["count"] = json!(1);
                request["template"] = json!(if self.items.direct { items::DIRECT_PICKUP.to_string() } else { self.items.generic_pickup.clone() });
                if let Some(item) = index_item(&self.items.rows, index) {
                    request["item"] = json!(item.id);
                    request["item_guid"] = json!(item.guid);
                    request["item_level"] = json!(self.items.level);
                    request["item_rarity"] = json!(self.items.rarity);
                    request["item_amount"] = json!(self.items.amount);
                    if let Some(stats) = self.items.stats_request(&item) {
                        request["stats"] = stats;
                    }
                }
            }
            write_atomic(&self.dir, REQUEST_FILE, REQUEST_TMP, &request.to_string())
        });
        self.status = match result {
            Ok(()) => Status::Waiting(Instant::now()),
            Err(e) => Status::Error(e),
        };
    }

    fn tick(&mut self, ctx: &egui::Context) {
        self.items_pump();
        self.social_tick();
        if let Some(d) = &self.downloader {
            for id in d.finished() {
                self.icons.remove(&id);
            }
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if !self.items.outbox.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        if let Status::Waiting(since) = self.status {
            if !self.dir.join(REQUEST_FILE).exists() {
                self.status = Status::Delivered;
            } else if since.elapsed() > Duration::from_secs(6) {
                let _ = std::fs::remove_file(self.dir.join(REQUEST_FILE));
                self.status = Status::NoAnswer;
            }
            ctx.request_repaint_after(Duration::from_millis(150));
        }
        if !self.items.names_applied && !self.items.rows.is_empty() {
            let ready = self.downloader.as_ref().map(|d| d.progress.index_ready.load(std::sync::atomic::Ordering::Relaxed) > 0).unwrap_or(true);
            if ready {
                self.items.names_applied = true;
                let titles = icons::load_titles(&self.dir);
                if !titles.is_empty() {
                    self.item_cat.entries = item_entries(&self.items.rows, &titles);
                }
            }
        }
        if self.spawner_checked.elapsed() >= Duration::from_secs(2) {
            self.spawner_checked = Instant::now();
            self.spawner_status = spawners::load_status(&self.dir);
            self.clean_raids();
        }
        if self.player_checked.elapsed() >= Duration::from_millis(700) {
            self.player_checked = Instant::now();
            self.player = read_player(&self.dir);
            remote::set_hosting(remote::read_hosting(&self.dir));
            let connected = remote::connected();
            if connected && !self.synced {
                self.synced = true;
                self.send_powers();
                let dir = self.dir.clone();
                self.social.request_roster(&dir);
            } else if !connected {
                self.synced = false;
                self.confirmed = false;
            }
        }
        ctx.request_repaint_after(Duration::from_millis(700));
    }

    fn send_powers(&self) {
        // Powers only live while the player is connected: they are sent again on every join.
        remote::forget_one(powers::POWERS_FILE);
        if let Ok(text) = std::fs::read_to_string(self.dir.join(powers::POWERS_FILE)) {
            let _ = remote::submit(powers::POWERS_FILE, &text);
        }
    }

    /// Manual button: sends the server everything the panel has configured.
    pub fn resend_state(&mut self) -> usize {
        remote::forget_sent();
        let mut sent = 0;
        for name in ["m182_powers.json", "m182_zones.json", "m182_teams.json"] {
            if let Ok(text) = std::fs::read_to_string(self.dir.join(name)) {
                if remote::submit(name, &text).is_ok() {
                    sent += 1;
                }
            }
        }
        let standing: Vec<_> = self.spawners.iter().filter(|s| !s.once).cloned().collect();
        for op in spawners::op_replace_all(&standing, 4) {
            if remote::submit(spawners::OP_FILE, &op).is_ok() {
                sent += 1;
            }
        }
        if remote::submit(items::DATA_MESSAGE_FILE, &self.items.data_message()).is_ok() {
            sent += 1;
        }
        remote::note(&format!("resent {sent} state file(s) to the server"));
        sent
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("M182  ADMIN  PANEL").font(title_font(26.0)).color(GOLD));
                self.server_strip(ui);
                self.profile_gate(ui);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !DONATE_URL.is_empty() && button(ui, "Paypal Donations Support", Vec2::new(250.0, 34.0), true, false) {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(DONATE_URL));
                }
            });
        });
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        for (tab, label, soon) in [
            (Tab::Creatures, "Criaturas", false),
            (Tab::Items, "Ítems", false),
            (Tab::Places, "Lugares", false),
            (Tab::Powers, "Admin", false),
            (Tab::Pvp, "PvP", false),
            (Tab::SafeZones, "Zonas y bases", false),
            (Tab::Teams, "Equipos", false),
            (Tab::Events, "Eventos", false),
            (Tab::Settings, "Ajustes", false),
        ] {
            let selected = self.tab == tab;
            let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::click());
            let painter = ui.painter();
            let fill = if selected {
                CARD_SELECTED
            } else if response.hovered() {
                CARD_HOVER
            } else {
                CARD
            };
            painter.rect_filled(rect, Rounding::same(4.0), fill);
            painter.rect_stroke(rect, Rounding::same(4.0), Stroke::new(if selected { 1.8_f32 } else { 1.0_f32 }, if selected { GOLD } else { BRONZE }));
            if selected {
                glow_stroke(painter, rect, GOLD, 1.0);
                diamond(painter, rect.left_center() + Vec2::new(12.0, 0.0), 4.0, GOLD);
                diamond(painter, rect.right_center() - Vec2::new(12.0, 0.0), 3.0, GOLD_DIM);
            } else if response.hovered() {
                glow_stroke(painter, rect, GOLD_DIM, 0.6);
            }
            let shift = if soon { -7.0 } else { 0.0 };
            painter.text(
                rect.left_center() + Vec2::new(28.0, shift),
                Align2::LEFT_CENTER,
                label,
                title_font(17.0),
                if soon { MUTED } else { PARCHMENT },
            );
            if soon {
                painter.text(rect.left_center() + Vec2::new(28.0, 11.0), Align2::LEFT_CENTER, "próximamente", egui::FontId::proportional(14.5), GOLD_DIM);
            }
            if response.clicked() {
                self.tab = tab;
            }
        }
    }

    fn catalog_view(&mut self, ui: &mut egui::Ui, mode: Mode) {
        egui::SidePanel::right(match mode {
            Mode::Creatures => "creature_controls",
            Mode::Items => "item_controls",
        })
            .exact_width(320.0)
            .frame(egui::Frame::none().fill(LEATHER).inner_margin(egui::Margin::same(16.0)).stroke(Stroke::new(1.0_f32, BRONZE)))
            .show_inside(ui, |ui| {
                egui::TopBottomPanel::bottom(match mode {
                    Mode::Creatures => "creature_action",
                    Mode::Items => "item_action",
                })
                .frame(egui::Frame::none())
                .show_inside(ui, |ui| self.action_bar(ui, mode));
                egui::ScrollArea::vertical().show(ui, |ui| self.controls(ui, mode));
            });

        egui::CentralPanel::default().frame(egui::Frame::none().inner_margin(egui::Margin { left: 20.0, right: 16.0, top: 16.0, bottom: 8.0 })).show_inside(ui, |ui| {
            let App { creatures, item_cat, icons, dir, downloader, .. } = self;
            let cat = match mode {
                Mode::Creatures => creatures,
                Mode::Items => item_cat,
            };
            if let Some(error) = &cat.error {
                ui.label(RichText::new(error).color(BAD));
                return;
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut cat.search).hint_text("Buscar…").desired_width(240.0));
                let mut groups: Vec<String> = cat.entries.iter().map(|e| e.group.clone()).collect();
                groups.sort_unstable();
                groups.dedup();
                let all = if mode == Mode::Creatures { "Todas las facciones" } else { "Todas las categorías" };
                let salt = match mode {
                    Mode::Creatures => 0,
                    Mode::Items => 2,
                };
                let current = if cat.group.is_empty() { all.to_string() } else { group_label(mode, &cat.group) };
                egui::ComboBox::from_id_salt(("group", salt)).selected_text(current).show_ui(ui, |ui| {
                    ui.selectable_value(&mut cat.group, String::new(), all);
                    for g in groups {
                        let text = group_label(mode, &g);
                        ui.selectable_value(&mut cat.group, g, text);
                    }
                });
                if mode == Mode::Creatures {
                    ui.checkbox(&mut cat.only_bosses, "Solo jefes");
                }
                ui.checkbox(&mut cat.show_internal, "Mostrar entradas internas");
            });
            let visible = cat.visible();
            let noun = match mode {
                Mode::Creatures => "criaturas",
                Mode::Items => "ítems",
            };
            let status = downloader.as_ref().map(|d| d.status()).unwrap_or_default();
            ui.label(RichText::new(format!("{} {}    {}", visible.len(), noun, status)).color(MUTED));
            Self::grid(ui, cat, icons, dir, downloader, &visible, mode);
        });
    }

    fn grid(ui: &mut egui::Ui, cat: &mut Catalog, icons: &mut HashMap<String, Option<TextureHandle>>, dir: &Path, downloader: &mut Option<icons::Downloader>, visible: &[usize], mode: Mode) {
        let spacing = 10.0;
        let min_card = 200.0;
        let card_h = 70.0;
        let width = ui.available_width() - 14.0;
        let cols = (((width + spacing) / (min_card + spacing)).floor() as usize).max(1);
        let card_w = (width - spacing * (cols as f32 - 1.0)) / cols as f32 - 2.0;
        let rows = visible.len().div_ceil(cols);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, card_h, rows, |ui, range| {
            ui.spacing_mut().item_spacing = Vec2::new(spacing, spacing);
            for row in range {
                ui.horizontal(|ui| {
                    for &i in visible.iter().skip(row * cols).take(cols) {
                        let selected = cat.selected == Some(i);
                        if Self::card(ui, icons, dir, downloader, &cat.entries[i], selected, card_w, mode) {
                            cat.selected = Some(i);
                        }
                    }
                });
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn card(ui: &mut egui::Ui, icons: &mut HashMap<String, Option<TextureHandle>>, dir: &Path, downloader: &mut Option<icons::Downloader>, e: &Entry, selected: bool, width: f32, mode: Mode) -> bool {
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 70.0), Sense::click());
        if !ui.is_rect_visible(rect) {
            return response.clicked();
        }
        if !icons.contains_key(&e.guid) {
            let loaded = load_icon(ui.ctx(), dir, e);
            if loaded.is_none() {
                if let Some(d) = downloader {
                    d.request(&e.guid, e.icon_keys.clone());
                }
            }
            icons.insert(e.guid.clone(), loaded);
        }
        let texture = icons.get(&e.guid).and_then(|t| t.clone());
        let fill = if selected {
            CARD_SELECTED
        } else if response.hovered() {
            CARD_HOVER
        } else {
            CARD
        };
        let painter = ui.painter();
        painter.rect_filled(rect, Rounding::same(5.0), fill);
        painter.rect_stroke(rect, Rounding::same(5.0), Stroke::new(if selected { 2.0_f32 } else { 1.0_f32 }, if selected { GOLD } else { BRONZE }));
        let icon = egui::Rect::from_min_size(rect.min + Vec2::new(8.0, 8.0), Vec2::splat(54.0));
        painter.rect_filled(icon, Rounding::same(4.0), Color32::from_rgb(20, 15, 11));
        painter.rect_stroke(icon, Rounding::same(4.0), Stroke::new(1.0_f32, GOLD_DIM));
        match texture {
            Some(texture) => {
                painter.image(texture.id(), icon.shrink(2.0), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            }
            None => emblems::draw(painter, icon.shrink(4.0), e.kind, group_color(&e.group), e.boss),
        }
        let text_left = icon.right() + 10.0;
        let title = e.alias.clone().unwrap_or_else(|| e.label.clone());
        let mut job = egui::text::LayoutJob::simple(title, egui::FontId::proportional(17.0), PARCHMENT, rect.right() - text_left - 8.0);
        job.wrap.max_rows = if e.alias.is_some() { 1 } else { 2 };
        job.wrap.overflow_character = Some('…');
        let galley = painter.layout_job(job);
        painter.galley(egui::pos2(text_left, rect.top() + 7.0), galley, PARCHMENT);
        if e.alias.is_some() {
            let mut sub = egui::text::LayoutJob::simple(e.label.clone(), egui::FontId::proportional(13.5), MUTED, rect.right() - text_left - 8.0);
            sub.wrap.max_rows = 1;
            sub.wrap.overflow_character = Some('…');
            let galley = painter.layout_job(sub);
            painter.galley(egui::pos2(text_left, rect.top() + 27.0), galley, MUTED);
        }
        painter.text(
            egui::pos2(text_left, rect.bottom() - 8.0),
            Align2::LEFT_BOTTOM,
            if e.meta.is_empty() { group_label(mode, &e.group) } else { format!("{} · {}", group_label(mode, &e.group), e.meta) },
            title_font(12.5),
            group_color(&e.group),
        );
        response.clicked()
    }

    fn status_text(&self) -> (String, Color32) {
        match &self.status {
            Status::Idle => (String::new(), MUTED),
            Status::Waiting(_) => ("Enviando al juego…".to_string(), MUTED),
            Status::Delivered if remote::active() => ("Enviado al servidor.".to_string(), GOOD),
            Status::Delivered => ("Enviado.".to_string(), GOOD),
            Status::NoAnswer => ("El juego no respondió.".to_string(), BAD),
            Status::Error(e) => (format!("Error: {e}"), BAD),
        }
    }

    fn target_controls(&mut self, ui: &mut egui::Ui) {
        label_title(ui, "DÓNDE APARECE");
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.target, Target::Around, "A mi alrededor");
            ui.selectable_value(&mut self.target, Target::Place, "En un lugar");
        });
        match self.target {
            Target::Around => {
                ui.label("Distancia de ti (metros)");
                ui.add(egui::Slider::new(&mut self.distance, 1.0..=60.0).fixed_decimals(0));
            }
            Target::Place => {
                if self.places.is_empty() {
                    ui.label(RichText::new("Sin lugares.").color(MUTED));
                } else {
                    self.place_index = self.place_index.min(self.places.len() - 1);
                    let current = self.places[self.place_index].name.clone();
                    egui::ComboBox::from_id_salt("place").selected_text(current).show_ui(ui, |ui| {
                        for (i, p) in self.places.iter().enumerate() {
                            ui.selectable_value(&mut self.place_index, i, &p.name);
                        }
                    });
                    if self.count > 1 {
                        ui.label("Radio alrededor del lugar");
                        ui.add(egui::Slider::new(&mut self.distance, 1.0..=60.0).fixed_decimals(0));
                    }
                    ui.label("Reaparece cada (minutos)");
                    ui.add(egui::Slider::new(&mut self.respawn_minutes, 1..=120));
                }
            }
        }
    }

    fn spawner_list(&mut self, ui: &mut egui::Ui) {
        label_title(ui, &format!("APARICIONES ACTIVAS ({})", self.spawners.len()));
        if self.spawners.is_empty() {
            ui.label(RichText::new("Ninguna.").color(MUTED).size(14.5));
            return;
        }
        let mut remove: Option<String> = None;
        for s in &self.spawners {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    let times = if s.count > 1 { format!("{} × ", s.count) } else { String::new() };
                    ui.label(RichText::new(format!("{times}{}", s.creature)).color(PARCHMENT));
                    let rhythm = if !s.pool.is_empty() && s.once { "raid de plantilla, una vez".to_string() } else if !s.pool.is_empty() { format!("{} plantillas, cada {} min, radio {:.0} m", s.pool.len(), s.every / 60, s.area) } else if s.once { "raid, una sola vez".to_string() } else { format!("cada {} min", s.respawn / 60) };
                    let loot = if s.drops.is_empty() { String::new() } else { format!(" · botín: {}", s.drops.len()) };
                    ui.label(RichText::new(format!("{} · nv.{} · {}{}", s.place, s.level, rhythm, loot)).color(MUTED).size(14.0));
                    if let Some(note) = spawners::distance_note(self.player, s.at) {
                        ui.label(RichText::new(note).color(MUTED).size(13.5));
                    }
                    let live = self.spawner_status.get(&s.id);
                    let state = if remote::connected() || (remote::active() && live.is_none()) { "Activa en el servidor".to_string() } else { spawners::describe(live) };
                    ui.label(RichText::new(state).color(GOLD_DIM).size(14.5));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if button(ui, "Borrar", Vec2::new(70.0, 26.0), true, false) {
                        remove = Some(s.id.clone());
                    }
                });
            });
            ui.add_space(4.0);
        }
        if let Some(id) = remove {
            self.delete_spawner(&id);
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui, mode: Mode) {
        heading(
            ui,
            match mode {
                Mode::Creatures => "Invocación",
                Mode::Items => "Entregar ítem",
            },
            19.0,
        );
        divider(ui);
        let selected = self.catalog(mode).selected;
        match selected {
            Some(i) => {
                let e = &self.catalog(mode).entries[i];
                let icon = ui.allocate_exact_size(Vec2::new(ui.available_width(), 96.0), Sense::hover()).0;
                let painter = ui.painter();
                painter.rect_filled(icon, Rounding::same(4.0), Color32::from_rgb(20, 15, 11));
                ornate_frame(painter, icon);
                match self.icons.get(&e.guid).and_then(|t| t.clone()) {
                    Some(texture) => {
                        let side = icon.height() - 12.0;
                        let target = egui::Rect::from_center_size(icon.center(), Vec2::splat(side));
                        painter.image(texture.id(), target, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    None => {
                        let target = egui::Rect::from_center_size(icon.center(), Vec2::splat(icon.height() - 22.0));
                        emblems::draw(painter, target, e.kind, group_color(&e.group), e.boss);
                    }
                }
                ui.label(RichText::new(e.alias.as_deref().unwrap_or(&e.label)).font(title_font(19.0)).color(PARCHMENT));
                if e.alias.is_some() {
                    ui.label(RichText::new(&e.label).color(MUTED).size(15.0));
                }
                ui.label(RichText::new(group_label(mode, &e.group)).color(group_color(&e.group)));
            }
            None => {
                ui.label(
                    RichText::new(match mode {
                        Mode::Creatures => "Elige una criatura de la lista",
                        Mode::Items => "Elige un ítem de la lista",
                    })
                    .color(MUTED),
                );
            }
        }
        divider(ui);

        if mode == Mode::Items {
            self.item_controls(ui, selected);
            return;
        }
        if mode == Mode::Creatures {
            label_title(ui, "NIVEL");
            ui.add(egui::Slider::new(&mut self.level, 1..=100));
        }
        label_title(ui, "CANTIDAD");
        ui.add(egui::Slider::new(&mut self.count, 1..=20));
        if mode == Mode::Creatures {
            label_title(ui, "VIDA");
            ui.checkbox(&mut self.custom_health, "Vida personalizada");
            ui.add_enabled(self.custom_health, egui::Slider::new(&mut self.health, 10..=1_000_000).logarithmic(true));
            label_title(ui, "DAÑO  (1.00 = normal)");
            ui.add(egui::Slider::new(&mut self.damage, 0.0..=20.0).fixed_decimals(2));
            divider(ui);
            label_title(ui, "BOTÍN PERSONALIZADO");
            let label = if self.creature_drops.is_empty() { "Sin botín extra".to_string() } else { format!("Botín extra: {} objeto(s)", self.creature_drops.len()) };
            ui.label(RichText::new(label).color(GOLD_DIM).size(14.0));
            ui.label(RichText::new("Objetos extra que suelta cada enemigo al morir, con su probabilidad, además de su botín normal del juego. Vale para «Invocar» y para «Crear aparición».").color(MUTED).size(13.5));
            let mut drops = std::mem::take(&mut self.creature_drops);
            let mut changed = false;
            self.loot_editor(ui, &mut drops, &mut changed);
            self.creature_drops = drops;
        }
        divider(ui);

        self.target_controls(ui);
        divider(ui);

        if mode == Mode::Creatures {
            self.spawner_list(ui);
        }
    }

    fn action_bar(&mut self, ui: &mut egui::Ui, mode: Mode) {
        ui.add_space(6.0);
        let selected = self.catalog(mode).selected;
        match mode {
            Mode::Creatures => {
                let enabled = selected.is_some();
                let place_mode = self.target == Target::Place;
                let label = if place_mode { "CREAR APARICIÓN" } else { "INVOCAR" };
                let enabled = enabled && (!place_mode || !self.places.is_empty());
                if button(ui, label, Vec2::new(ui.available_width(), 52.0), enabled, true) {
                    if place_mode {
                        self.create_spawner();
                    } else if self.creature_drops.is_empty() {
                        self.spawn(mode);
                    } else {
                        self.summon_with_loot();
                    }
                }
                let (text, color) = self.status_text();
                if !text.is_empty() {
                    ui.label(RichText::new(text).color(color));
                }
                if place_mode && !self.spawner_notice.is_empty() {
                    ui.label(RichText::new(self.spawner_notice.clone()).color(GOOD).size(15.0));
                }
            }
            Mode::Items => {
                let Some(index) = selected.filter(|i| *i < self.items.rows.len()) else {
                    return;
                };
                let deliverable = self.items.rows[index].deliverable();
                if button(ui, "ENTREGAR", Vec2::new(ui.available_width(), 52.0), deliverable, true) {
                    self.spawn(Mode::Items);
                }
                let (text, color) = self.status_text();
                if !text.is_empty() {
                    ui.label(RichText::new(text).color(color));
                }
            }
        }
        ui.add_space(4.0);
    }

    fn places_tab(&mut self, ui: &mut egui::Ui) {
        heading(ui, "Lugares guardados", 22.0);
        divider(ui);
        match self.player {
            Some(p) => {
                ui.label(format!("Tu posición actual:  X {:.1}   Y {:.1}   Z {:.1}", p[0], p[1], p[2]));
            }
            None => {
                ui.label(RichText::new("Sin posición.").color(MUTED));
            }
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.new_place).hint_text("Nombre del lugar").desired_width(260.0));
            let ready = (self.player.is_some() || remote::active()) && !self.new_place.trim().is_empty() && self.here.is_none();
            if button(ui, "Guardar mi posición", Vec2::new(220.0, 32.0), ready, false) {
                let name = self.new_place.trim().to_string();
                self.new_place.clear();
                self.ask_here(social::Here::Place(name));
            }
        });
        divider(ui);
        let mut remove = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (i, p) in self.places.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&p.name).font(title_font(17.0)).color(PARCHMENT));
                    ui.label(RichText::new(format!("X {:.0}  Y {:.0}  Z {:.0}", p.x, p.y, p.z)).color(MUTED));
                    if button(ui, "Borrar", Vec2::new(80.0, 26.0), true, false) {
                        remove = Some(i);
                    }
                });
            }
            if self.places.is_empty() {
                ui.label(RichText::new("Sin lugares.").color(MUTED));
            }
        });
        if let Some(i) = remove {
            self.places.remove(i);
            self.save_places();
        }
    }

    fn pvp_tab(&mut self, ui: &mut egui::Ui) {
        heading(ui, "Combate entre jugadores", 22.0);
        divider(ui);
        let mut changed = false;
        changed |= ui.checkbox(&mut self.pvp, "Activar PvP: los jugadores se pueden atacar entre sí").changed();
        changed |= ui.add_enabled(self.pvp, egui::Checkbox::new(&mut self.pvp_friends, "También aplicar a las comprobaciones de aliados")).changed();
        changed |= ui.add_enabled(self.pvp, egui::Checkbox::new(&mut self.pvp_friendly_fire, "Fuego amigo: los compañeros de equipo también se pueden dañar")).changed();
        if changed {
            self.notice = match self.set_config(&[("pvp", json!(self.pvp)), ("pvp_friend_checks", json!(self.pvp && self.pvp_friends)), ("pvp_friendly_fire", json!(self.pvp_friendly_fire))]) {
                Ok(()) => "Guardado. El juego lo aplica solo.".to_string(),
                Err(e) => format!("Error: {e}"),
            };
        }
        if !self.notice.is_empty() {
            ui.label(RichText::new(&self.notice).color(GOOD));
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick(ctx);
        remote::hold(remote::active() && !self.confirmed);

        egui::TopBottomPanel::top("header")
            .frame(egui::Frame::none().fill(LEATHER).inner_margin(egui::Margin::symmetric(20.0, 12.0)).stroke(Stroke::new(1.0_f32, BRONZE)))
            .show(ctx, |ui| {
                self.header(ui);
            });

        egui::SidePanel::left("tabs")
            .exact_width(210.0)
            .frame(egui::Frame::none().fill(LEATHER).inner_margin(egui::Margin::same(12.0)).stroke(Stroke::new(1.0_f32, BRONZE)))
            .show(ctx, |ui| self.tabs(ui));

        ctx.request_repaint_after(std::time::Duration::from_millis(66));
        egui::CentralPanel::default().frame(egui::Frame::none().fill(BG)).show(ctx, |ui| {
            let time = ui.input(|i| i.time);
            enchanted_backdrop(ui.painter(), ui.max_rect(), time);
            match self.tab {
            Tab::Creatures => self.catalog_view(ui, Mode::Creatures),
            Tab::Items => self.catalog_view(ui, Mode::Items),
            tab => {
                egui::Frame::none().inner_margin(egui::Margin::same(20.0)).show(ui, |ui| match tab {
                    Tab::Places => self.places_tab(ui),
                    Tab::Powers => self.powers_tab(ui),
                    Tab::Pvp => self.pvp_tab(ui),
                    Tab::SafeZones => self.zones_tab(ui),
                    Tab::Events => self.events_tab(ui),
                    Tab::Settings => self.settings_tab(ui),
                    _ => self.teams_tab(ui),
                });
            }
            }
        });
    }
}

pub fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
        // changed sizing for my needs
            .with_inner_size([853.0, 520.0])
            .with_min_inner_size([600.0, 400.0])
            .with_title(WINDOW_TITLE),
        ..Default::default()
    }
}

pub fn run_standalone() -> eframe::Result {
    let dir = find_game_dir();
    eframe::run_native(WINDOW_TITLE, native_options(), Box::new(move |cc| Ok(Box::new(App::new(cc, dir)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(guid: &str, label: &str, group: &str, internal: bool) -> Entry {
        Entry {
            guid: guid.into(),
            raw: label.into(),
            alias: None,
            icon_file: None,
            label: label.into(),
            group: group.into(),
            boss: false,
            search: label.to_lowercase(),
            kind: emblems::Kind::Fang,
            internal,
            icon_keys: Vec::new(),
            meta: String::new(),
        }
    }

    fn app_in(dir: &Path, entries: Vec<Entry>) -> App {
        App {
            profiles: Vec::new(),
            profile: String::new(),
            profile_new: String::new(),
            profile_notice: String::new(),
            dir: dir.to_path_buf(),
            tab: Tab::Creatures,
            creatures: Catalog { entries, selected: Some(0), ..Default::default() },
            item_cat: Catalog::default(),
            items: Default::default(),
            downloader: None,
            powers: Default::default(),
            social: Default::default(),
            ev: Default::default(),
            level: 30,
            count: 2,
            distance: 7.0,
            custom_health: true,
            health: 500,
            damage: 1.5,
            pvp: true,
            pvp_friends: false,
            pvp_friendly_fire: false,
            target: Target::Around,
            place_index: 0,
            respawn_minutes: 5,
            spawners: Vec::new(),
            spawner_status: HashMap::new(),
            spawner_checked: Instant::now(),
            places: Vec::new(),
            new_place: String::new(),
            player: None,
            player_checked: Instant::now(),
            creature_drops: Vec::new(),
            here: None,
            synced: false,
            confirmed: false,
            icons: HashMap::new(),
            status: Status::Idle,
            notice: String::new(),
            spawner_notice: String::new(),
            remote_cfg: remote::read_settings(&std::env::temp_dir().join("m182_none")),
            remote_notice: String::new(),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("m182_panel_{tag}_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("export")).unwrap();
        dir
    }

    fn read_request(dir: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join(REQUEST_FILE)).unwrap()).unwrap()
    }

    #[test]
    fn names_are_readable() {
        assert_eq!(pretty("Base_Enemy_AncientConstruct_Heavy"), "Ancient Construct Heavy");
        assert_eq!(pretty("Enemy_LizardpeopleDrak01_Melee_Spear"), "Lizardpeople Drak01 Melee Spear");
    }

    #[test]
    fn internal_entries_are_hidden() {
        assert!(is_internal("Base_Enemy_AncientConstruct_Modular01_sleep"));
        assert!(is_internal("ProjectileSpawner_AncientConstruct_Modular_Head02_Turret"));
        assert!(!is_internal("Enemy_Skeleton_Melee01"));
    }

    #[test]
    fn emblem_kinds() {
        assert_eq!(emblems::kind_of("Enemy_Skeleton_Melee01", "Skeleton"), emblems::Kind::Skull);
        assert_eq!(emblems::kind_of("Enemy_Wildbeast_Wolf_Heavy", "WildBeast"), emblems::Kind::Paw);
    }

    #[test]
    fn item_names_come_from_wiki_titles() {
        let mut titles = HashMap::new();
        titles.insert(icons::norm("Warrior Helmet"), "Warrior Helmet".to_string());
        let csv = "item_id,guid,debug_name,name,category,rarity,max_stack,slot,level_min,level_max,give_guid,pickup_guid,pickup_name,give_level,speed_mod,dmg_mod,cut\n1,aaaaaaaa-0000-0000-0000-000000000001,Armor_T2_Warrior_Helmet_Loot,Armor_T2_Warrior_Helmet_Loot,Equipment,Rare,1,Armour_Head,10,10,,,,10,1,1,0\n2,aaaaaaaa-0000-0000-0000-000000000002,Mystery_Thing_Zz,Mystery_Thing_Zz,Equipment,Rare,1,Armour_Head,10,10,,,,10,1,1,0\n";
        let rows = items::parse_items(csv);
        let entries = item_entries(&rows, &titles);
        let by = |raw: &str| entries.iter().find(|e| e.raw == raw).unwrap().label.clone();
        assert_eq!(by("Armor_T2_Warrior_Helmet_Loot"), "Warrior Helmet");
        assert_eq!(by("Mystery_Thing_Zz"), "Mystery Thing Zz");
    }

    #[test]
    fn item_stats_autosave_and_delivery() {
        let dir = temp_dir("autosave");
        let mut st = items::ItemsState::default();
        let mut item = items::Item::default();
        item.id = 77;
        item.guid = "aaaaaaaa-0000-0000-0000-000000000077".into();
        item.dmg[1] = 10.0;
        item.dmg_set = true;
        st.rows.push(item.clone());
        assert!(st.stats_request(&item).is_none());
        let mut edit = items::ItemsState::base_edit(&item);
        edit.dmg.as_mut().unwrap()[1] = 500.0;
        st.set_edit(&item, edit);
        assert!(st.dirty.contains_key(&77));
        let req = st.stats_request(&item).unwrap();
        assert_eq!(req["dmg_new"].as_str().unwrap().len(), 120);
        assert_eq!(req["dmg_old"][0].as_str().unwrap(), items::dmg_hex(&item.dmg));
        assert!(req.get("arm_new").is_none());
        st.apply_item(&dir, &item).unwrap();
        assert!(st.dirty.is_empty());
        assert_eq!(st.outbox.len(), 1);
        let saved = std::fs::read_to_string(dir.join(items::STATS_FILE)).unwrap();
        assert!(saved.contains("77"));
        st.set_edit(&item, items::ItemsState::base_edit(&item));
        let undo = st.stats_request(&item).unwrap();
        assert_eq!(undo["dmg_new"].as_str().unwrap(), items::dmg_hex(&item.dmg));
        st.apply_item(&dir, &item).unwrap();
        assert_eq!(st.outbox.len(), 2);
        assert!(st.stats_request(&item).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn powers_files() {
        let dir = temp_dir("pw");
        let mut st = powers::PowersState::default();
        st.god = true;
        st.fly = true;
        st.fly_speed = 20.0;
        powers::save(&dir, &st).unwrap();
        let back = powers::PowersState::load(&dir);
        assert!(back.god && !back.fly && !back.stamina);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn creature_request_roundtrip() {
        let dir = temp_dir("rt");
        std::fs::write(dir.join("export").join("m182_enemies.csv"), "name,guid,faction,boss,preset\nEnemy_A,642098d4-fc09-48e2-811e-782d438bf551,Drak,false,\nEnemy_A@x10,642098d4-fc09-4182-0002-782d438bf551,Drak,false,x10\n").unwrap();
        std::fs::write(dir.join(CONFIG_FILE), r#"{"pvp":true,"spawn_key":"F8","custom":1}"#).unwrap();
        let list = load_enemies(&dir).unwrap();
        assert_eq!(list.len(), 1);
        let mut app = app_in(&dir, list);
        app.spawn(Mode::Creatures);
        assert!(matches!(app.status, Status::Waiting(_)));
        let req = read_request(&dir);
        assert_eq!(req["health"], 500);
        assert_eq!(req["level"], 30);
        assert!(req.get("at").is_none());
        assert!(req.get("forward").is_none());
        assert!(req.get("object").is_none());
        let cfg = read_config(&dir);
        assert_eq!(cfg["custom"], 1);
        assert_eq!(cfg["spawn_health"], 500);
        assert_eq!(cfg["spawn_key"], "F8");
        std::fs::remove_file(dir.join(REQUEST_FILE)).unwrap();
        let ctx = egui::Context::default();
        app.tick(&ctx);
        assert!(matches!(app.status, Status::Delivered));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn creatures_appear_around_you_or_become_persistent_spawners() {
        let dir = temp_dir("pl");
        std::fs::write(dir.join(PLAYER_FILE), r#"{"x":10.5,"y":20.0,"z":-3.25}"#).unwrap();
        assert_eq!(read_player(&dir), Some([10.5, 20.0, -3.25]));
        let mut app = app_in(&dir, vec![entry("642098d4-fc09-48e2-811e-782d438bf551", "A", "Drak", false)]);
        app.places.push(Place { name: "Torre".into(), x: 1.0, y: 2.0, z: 3.0 });
        app.save_places();
        assert_eq!(load_places(&dir).len(), 1);

        // around you: a one-off request, no coordinates, nothing saved as a spawner
        app.target = Target::Around;
        app.spawn(Mode::Creatures);
        let req = read_request(&dir);
        assert!(req.get("at").is_none());
        assert!(req.get("forward").is_none());
        assert!(req.get("object").is_none());
        let cfg = read_config(&dir);
        assert_eq!(cfg.get("spawn_template").and_then(|v| v.as_str()), Some("642098d4-fc09-48e2-811e-782d438bf551"));
        assert!(spawners::load(&dir).is_empty());

        // on a place: a spawner that comes back every few minutes
        app.target = Target::Place;
        app.respawn_minutes = 5;
        app.create_spawner();
        let saved = spawners::load(&dir);
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].at, [1.0, 2.0, 3.0]);
        assert_eq!(saved[0].respawn, 300);
        assert_eq!(saved[0].level, 30);
        assert_eq!(saved[0].health, 500);
        assert_eq!(saved[0].count, 2);
        assert_eq!(saved[0].template, "642098d4-fc09-48e2-811e-782d438bf551");
        assert_eq!(saved[0].place, "Torre");

        // the game reports the creature dead: the list tells when it comes back
        std::fs::write(
            dir.join(spawners::STATUS_FILE),
            format!(r#"{{"spawners":[{{"id":"{}","alive":0,"want":2,"state":"esperando para reaparecer","next":125,"spawned":2}}]}}"#, saved[0].id),
        )
        .unwrap();
        app.spawner_status = spawners::load_status(&dir);
        assert_eq!(spawners::describe(app.spawner_status.get(&saved[0].id)), "Muerto: reaparece en 2:05");

        let id = saved[0].id.clone();
        app.delete_spawner(&id);
        assert!(spawners::load(&dir).is_empty());

        app.places.clear();
        app.create_spawner();
        assert!(matches!(app.status, Status::Error(_)));
        assert!(spawners::load(&dir).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn enemies_summoned_with_custom_loot_carry_it_to_the_server() {
        let dir = temp_dir("cl");
        std::fs::write(dir.join("m182_player.json"), r#"{"x":10.5,"y":20.0,"z":-3.25}"#).unwrap();
        let mut app = app_in(&dir, vec![entry("642098d4-fc09-48e2-811e-782d438bf551", "A", "Drak", false)]);
        app.items.generic_pickup = "c15a5c9d-4956-49ff-9d72-9f522229cf1c".into();
        app.places.push(Place { name: "Torre".into(), x: 1.0, y: 2.0, z: 3.0 });
        app.creature_drops = vec![spawners::Drop { item: 77, guid: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(), name: "Espada".into(), amount: 4, rarity: 2, level: 30, chance: 35 }];
        app.player = read_player(&dir);
        app.count = 3;
        app.distance = 12.0;
        app.summon_with_loot();
        let around = spawners::load(&dir);
        assert_eq!(around.len(), 1);
        assert!(around[0].once);
        assert_eq!(around[0].at, [10.5, 20.0, -3.25]);
        assert_eq!((around[0].count, around[0].radius), (3, 12.0));
        assert_eq!(around[0].drops.len(), 1);
        assert_eq!(around[0].pickup, "c15a5c9d-4956-49ff-9d72-9f522229cf1c");
        assert!(spawners::op_add(&around[0]).contains("\"k\":[[77,"));

        app.create_spawner();
        let placed = spawners::load(&dir);
        let on_place = placed.iter().find(|s| !s.once).unwrap();
        assert_eq!(on_place.drops.len(), 1);
        assert_eq!(on_place.drops[0].chance, 35);
        assert_eq!(on_place.pickup, "c15a5c9d-4956-49ff-9d72-9f522229cf1c");

        app.creature_drops.clear();
        app.create_spawner();
        let plain = spawners::load(&dir);
        assert!(plain.iter().filter(|s| !s.once).any(|s| s.drops.is_empty() && s.pickup.is_empty()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn item_delivery_request() {
        let dir = temp_dir("it");
        let csv = "item_id,guid,debug_name,name,category,rarity,max_stack,slot,level_min,level_max,give_guid,pickup_guid,pickup_name,give_level,speed_mod,dmg_mod,cut,pierce,blunt,fire,ice,fog,lightning,healing,poison,wood,stone,metal,explosion,dmg_set,ar_physical,ar_blunt,ar_pierce,ar_cut,ar_magical,ar_fire,ar_ice,ar_fog,ar_lightning,ar_set\n\
            1234,res-guid,Sword_Iron,Iron Sword,Weapons,Rare,1,MainHand,3,20,give-guid,,,3,1,1.25,12.5,3,0,7,0,0,0,0,0,0,0,0,0,1,0,0,0,0,0,0,0,0,0,0\n\
            55,res-2,Old_Thing,Old Thing,Materials,Common,50,,1,1,,pick-guid,LootPickup_Old,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0\n";
        std::fs::create_dir_all(dir.join("export")).unwrap();
        std::fs::write(dir.join("export").join("m182_items.csv"), csv).unwrap();
        let (cat, rows) = load_item_catalog(&dir);
        assert_eq!(cat.entries.len(), 2);
        let mut app = app_in(&dir, Vec::new());
        app.item_cat = cat;
        app.items.rows = rows;
        let sword = app.item_cat.entries.iter().position(|e| e.label == "Iron Sword").unwrap();
        app.item_cat.selected = Some(sword);
        app.items.level = 40;
        app.items.rarity = 4;
        app.items.amount = 3;
        app.items.generic_pickup = "c15a5c9d-4956-49ff-9d72-9f522229cf1c".into();
        app.items.direct = false;
        app.target = Target::Place;
        app.spawn(Mode::Items);
        let req = read_request(&dir);
        assert_eq!(req["template"], "c15a5c9d-4956-49ff-9d72-9f522229cf1c");
        assert_eq!(req["item"], 1234);
        assert_eq!(req["item_guid"], "res-guid");
        assert_eq!(req["item_level"], 40);
        assert_eq!(req["item_rarity"], 4);
        assert_eq!(req["item_amount"], 3);
        assert_eq!(req["count"], 1);
        assert!(req.get("object").is_none());
        assert!(req.get("forward").is_none());
        assert!(req.get("at").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn direct_delivery_uses_the_touch_pickup_template() {
        let dir = temp_dir("direct_delivery");
        let csv = "item_id,guid,debug_name,name,category,rarity,max_stack,slot,level_min,level_max,give_guid,pickup_guid,pickup_name,give_level,speed_mod,dmg_mod,cut,pierce,blunt,fire,ice,fog,lightning,healing,poison,wood,stone,metal,explosion,dmg_set,ar_physical,ar_blunt,ar_pierce,ar_cut,ar_magical,ar_fire,ar_ice,ar_fog,ar_lightning,ar_set\n\
            55,res-2,Old_Thing,Old Thing,Materials,Common,50,,1,1,,pick-guid,LootPickup_Old,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0\n";
        std::fs::create_dir_all(dir.join("export")).unwrap();
        std::fs::write(dir.join("export").join("m182_items.csv"), csv).unwrap();
        let (cat, rows) = load_item_catalog(&dir);
        let mut app = app_in(&dir, Vec::new());
        app.item_cat = cat;
        app.items.rows = rows;
        app.item_cat.selected = Some(0);
        app.items.level = 1;
        app.items.amount = 5;
        app.items.direct = true;
        app.target = Target::Place;
        app.spawn(Mode::Items);
        let req = read_request(&dir);
        assert_eq!(req["template"], items::DIRECT_PICKUP);
        assert_eq!(req["item_amount"], 5);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pvp_toggle_keeps_other_settings() {
        let dir = temp_dir("pvp");
        std::fs::write(dir.join(CONFIG_FILE), r#"{"pvp":false,"spawner":true,"spawn_level":9}"#).unwrap();
        let app = app_in(&dir, Vec::new());
        app.set_config(&[("pvp", json!(true)), ("pvp_friend_checks", json!(false))]).unwrap();
        let cfg = read_config(&dir);
        assert_eq!(cfg["pvp"], true);
        assert_eq!(cfg["spawn_level"], 9);
        assert_eq!(cfg["spawner"], true);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn image_coverage_with_the_real_wiki_index() {
        let path = std::path::Path::new("/mnt/user-data/uploads/Enshrouded");
        if !path.join("export/m182_icons/wiki_index.tsv").exists() || !path.join("export/m182_items.csv").exists() {
            return;
        }
        let index = icons::load_index(path);
        let hit = |keys: &[String]| keys.iter().find(|k| index.contains_key(*k)).cloned();
        let (cat, rows) = load_item_catalog(path);
        let mut by_group: HashMap<String, (usize, usize)> = HashMap::new();
        let mut shown = 0;
        for (e, it) in cat.entries.iter().zip(rows.iter()) {
            let slot = by_group.entry(it.category.clone()).or_default();
            slot.1 += 1;
            if let Some(k) = hit(&e.icon_keys) {
                slot.0 += 1;
                if shown < 400 && (shown % 9 == 0) {
                    println!("ITEM {} -> {}", it.debug, k);
                }
                shown += 1;
            }
        }
        println!("ITEM COVERAGE {by_group:?}");
        let enemies = load_enemies(path).unwrap();
        let visible: Vec<&Entry> = enemies.iter().filter(|e| !e.internal).collect();
        let covered = visible.iter().filter(|e| hit(&e.icon_keys).is_some()).count();
        println!("ENEMY COVERAGE {covered}/{}", visible.len());
        for e in visible.iter().filter(|e| hit(&e.icon_keys).is_none()).take(60) {
            println!("ENEMY MISS {}", e.raw);
        }
        assert!(covered * 100 / visible.len().max(1) >= 60, "enemy coverage {covered}/{}", visible.len());
    }

    #[test]
    fn enemy_names_come_from_the_game_column() {
        let dir = std::env::temp_dir().join(format!("m182_enemy_names_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("export")).unwrap();
        std::fs::write(
            dir.join("export/m182_enemies.csv"),
            "name,guid,faction,boss,preset,display\nEnemy_Skeleton_Melee01,aaaa,Skeleton,false,,Skeleton Warrior\nEnemy_Troll_01,bbbb,Troll,false,,\n",
        )
        .unwrap();
        let list = load_enemies(&dir).unwrap();
        let skeleton = list.iter().find(|e| e.guid == "aaaa").unwrap();
        assert_eq!(skeleton.alias.as_deref(), Some("Skeleton Warrior"));
        assert!(skeleton.search.contains("skeleton warrior"));
        assert!(list.iter().find(|e| e.guid == "bbbb").unwrap().alias.is_none());
    }

    #[test]
    fn game_item_names_win_over_wiki_titles() {
        let mut titles = HashMap::new();
        titles.insert(icons::norm("Warrior Helmet"), "Wiki Helmet".to_string());
        let csv = "item_id,guid,debug_name,name,category,rarity,max_stack,slot,level_min,level_max,give_guid,pickup_guid,pickup_name,give_level,speed_mod,dmg_mod,cut\n1,aaaaaaaa-0000-0000-0000-000000000001,Armor_T2_Warrior_Helmet_Loot,Warrior Helmet,Equipment,Rare,1,Armour_Head,10,10,,,,10,1,1,0\n";
        let rows = items::parse_items(csv);
        let entries = item_entries(&rows, &titles);
        assert_eq!(entries[0].label, "Warrior Helmet");
    }

    #[test]
    fn real_catalog_loads() {
        let path = std::path::Path::new("/mnt/user-data/uploads/Enshrouded");
        if path.join("export/m182_enemies.csv").exists() {
            let list = load_enemies(path).unwrap();
            assert!(list.len() > 400);
        }
    }
}
