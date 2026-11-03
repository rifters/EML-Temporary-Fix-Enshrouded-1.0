use std::ffi::c_void;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;
use std::time::{Duration, Instant};

use super::config::{SpawnSettings, LIVE, SPAWN};
use super::groups;
use super::roster;
use super::spawn_probe::{hex, original_create, read_bytes, u32_at, u64_at};

#[link(name = "user32")]
unsafe extern "system" {
    fn GetAsyncKeyState(vk: i32) -> i16;
    fn GetForegroundWindow() -> *mut c_void;
    fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
}

#[link(name = "winmm")]
unsafe extern "system" {
    fn timeBeginPeriod(period: u32) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcessId() -> u32;
    fn GetCurrentProcess() -> *mut c_void;
    fn WriteProcessMemory(process: *mut c_void, addr: *mut c_void, buf: *const c_void, size: usize, written: *mut usize) -> i32;
    fn VirtualQuery(addr: *const c_void, info: *mut MemInfo, size: usize) -> usize;
    fn VirtualProtect(addr: *mut c_void, size: usize, new_protect: u32, old_protect: *mut u32) -> i32;
}

#[repr(C)]
#[derive(Default)]
pub struct MemInfo {
    base: usize,
    alloc_base: usize,
    alloc_protect: u32,
    pad1: u32,
    size: usize,
    state: u32,
    protect: u32,
    kind: u32,
    pad2: u32,
}

const GUID_PLAYER: [u8; 16] = [0xd6, 0x66, 0xdb, 0xc3, 0xa5, 0x89, 0x6c, 0x4a, 0xa8, 0xe3, 0xd9, 0xa1, 0xf6, 0x00, 0x98, 0x12];

const HASH_CURRENT_TRANSFORM: u32 = 0x027a_c564;
const HASH_OVERRIDE_LEVEL: u32 = 0xa21e_7ee7;
const HASH_PICKUP_ITEM: u32 = 0x0877_5a90;
const PICKUP_ITEM_SIZE: usize = 28;

const TRANSFORM_SIZE: usize = 56;
pub(super) const COMPONENT_SLOTS: usize = 640;
const METERS: f64 = 4_294_967_296.0;
const MAX_CALIBRATION_TRIES: u32 = 400;
const HEALTH_SIGNATURE: u32 = 0x8eb8_4995;
const DAMAGE_SIGNATURE: u32 = 0xb866_36b9;
const STAMINA_SIGNATURE: u32 = 0x04b6_aa8b;
const MANA_SIGNATURE: u32 = 0x6aa2_574e;
const SUSCEPTIBILITY_SIGNATURE: u32 = 2_415_994_894;
const SUSCEPTIBILITY_SIZE: usize = 84;
const FALL_DAMAGE_SIZE: usize = 24;
const FALL_SEQUENCE: [u8; 16] = [0x53, 0xb0, 0xb6, 0x2d, 0x90, 0x4e, 0x82, 0x47, 0xab, 0x37, 0xf7, 0xff, 0xe8, 0x83, 0x50, 0x63];
const FALL_UNLIMITED: f32 = 1.0e9;
const BODY_HEAT_SIGNATURE: u32 = 3_350_114_077;
const ITEM_LEVEL_OFFSET: usize = 1652;
const ITEM_DAMAGE_OFFSET: usize = 1660;
const ITEM_ARMOR_OFFSET: usize = 1724;
const ITEM_INFO_SPAN: usize = 2100;
const POWERS_FILE: &str = "m182_powers.json";
const COMMAND_FILE: &str = "m182_command.request";
const TRAIL_FILE: &str = "m182_trail.csv";
const VK_SPACE: i32 = 0x20;
const VK_LCONTROL: i32 = 0xA2;

static STARTED: AtomicBool = AtomicBool::new(false);
static SERVER_MGR: AtomicUsize = AtomicUsize::new(0);
static PLAYER_ID: AtomicU32 = AtomicU32::new(0);
static CLIENT_MGR: AtomicUsize = AtomicUsize::new(0);
static CLIENT_PLAYER_ID: AtomicU32 = AtomicU32::new(0);
/// Entity of this game's own character, as the panel read it from the server's player list.
static CLIENT_SELF: AtomicU32 = AtomicU32::new(0);
static WATCHING: AtomicBool = AtomicBool::new(false);
static TRANSFORM_INDEX: AtomicI32 = AtomicI32::new(-1);
static CLIENT_TRANSFORM_INDEX: AtomicI32 = AtomicI32::new(-1);
static CLIENT_GUESSES: AtomicU32 = AtomicU32::new(0);
static CALIBRATION_TRIES: AtomicU32 = AtomicU32::new(0);
static CALIBRATION_DUMPS: AtomicU32 = AtomicU32::new(0);
static PENDING: AtomicU32 = AtomicU32::new(0);
static BUSY: AtomicBool = AtomicBool::new(false);
static LOG: Mutex<Option<File>> = Mutex::new(None);
static CLOCK: OnceLock<Instant> = OnceLock::new();

pub fn log(message: &str) {
    let ms = CLOCK.get_or_init(Instant::now).elapsed().as_millis();
    let Ok(mut guard) = LOG.lock() else { return };
    if guard.is_none() {
        if let Ok(dir) = std::env::current_dir() {
            if let Ok(mut f) = File::create(dir.join("m182_spawner.log")) {
                let _ = writeln!(f, "# m182 spawner log");
                *guard = Some(f);
            }
        }
    }
    if let Some(f) = guard.as_mut() {
        let _ = writeln!(f, "[{ms}ms] {message}");
        let _ = f.flush();
    }
}

pub fn start() {
    if !LIVE.spawner() || STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = CLOCK.get_or_init(Instant::now);
    log("spawner enabled");
    let _ = std::fs::remove_file("m182_mode.json");
    if let Err(e) = std::thread::Builder::new().name("m182-spawner-keys".into()).spawn(key_loop) {
        tracing::warn!(target: "m182::spawner", error = %e, "could not start hotkey thread");
    }
    if let Err(e) = std::thread::Builder::new().name("m182-powers".into()).spawn(powers_loop) {
        tracing::warn!(target: "m182::spawner", error = %e, "could not start powers thread");
    }
    if let Err(e) = std::thread::Builder::new().name("m182-keeper".into()).spawn(keeper_loop) {
        tracing::warn!(target: "m182::spawner", error = %e, "could not start keeper thread");
    }
    if let Err(e) = std::thread::Builder::new().name("m182-players".into()).spawn(players_loop) {
        tracing::warn!(target: "m182::spawner", error = %e, "could not start player scan thread");
    }
}

pub(super) fn game_has_focus() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        pid == GetCurrentProcessId()
    }
}

fn key_loop() {
    let mut was_down = [false; 5];
    let mut ticks = 0u32;
    let mut last_report = Instant::now();
    let mut last_pos: Option<(i64, i64, i64)> = None;
    let mut file_pos: Option<(i64, i64, i64)> = None;
    let mut last_file = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(25));
        if !LIVE.spawner() {
            was_down = [false; 5];
            continue;
        }
        ticks = ticks.wrapping_add(1);
        if ticks % 8 == 0 {
            poll_request();
        }
        let keys = match SPAWN.lock() {
            Ok(s) => [s.key, s.dump_player_key, s.dump_spawned_key, s.watch_key, 0x73],
            Err(_) => continue,
        };
        let focus = game_has_focus();
        let mut down = [false; 5];
        for i in 0..5 {
            down[i] = unsafe { GetAsyncKeyState(keys[i]) } as u16 & 0x8000 != 0 && focus;
        }
        if down[0] && !was_down[0] {
            PENDING.store(1, Ordering::SeqCst);
            log("spawn requested");
        }
        if down[1] && !was_down[1] {
            dump_entity("player (server)", SERVER_MGR.load(Ordering::SeqCst), PLAYER_ID.load(Ordering::SeqCst));
            dump_entity("player (client)", CLIENT_MGR.load(Ordering::SeqCst), CLIENT_PLAYER_ID.load(Ordering::SeqCst));
        }
        if down[2] && !was_down[2] {
            dump_entity("last spawned", SERVER_MGR.load(Ordering::SeqCst), LAST_SPAWNED.load(Ordering::SeqCst));
        }
        if down[3] && !was_down[3] {
            census();
        }
        if down[4] && !was_down[4] {
            toggle_watch();
        }
        was_down = down;

        if last_file.elapsed() >= Duration::from_secs(1) {
            last_file = Instant::now();
            check_hosted_world();
            write_mode();
            apply_own_character();
            if remote_world() && CLIENT_TRANSFORM_INDEX.load(Ordering::SeqCst) < 0 {
                if let Some((m, id)) = world_pair() {
                    guess_client_transform(m, id);
                }
            }
            if let Some(t) = player_transform() {
                let pos = (le_i64(&t, 0), le_i64(&t, 8), le_i64(&t, 16));
                if file_pos != Some(pos) {
                    if let Some(prev) = file_pos {
                        facing_update(prev, pos, &t);
                    }
                    file_pos = Some(pos);
                    write_position(pos);
                    record_trail(pos);
                }
            }
        }
        if last_report.elapsed() >= Duration::from_secs(4) {
            last_report = Instant::now();
            if let Some(t) = player_transform() {
                let pos = (le_i64(&t, 0), le_i64(&t, 8), le_i64(&t, 16));
                if last_pos != Some(pos) {
                    last_pos = Some(pos);
                    log(&format!(
                        "player position x={:.2} y={:.2} z={:.2}",
                        pos.0 as f64 / METERS,
                        pos.1 as f64 / METERS,
                        pos.2 as f64 / METERS
                    ));
                }
            }
        }
    }
}

static DUMP_COUNTER: AtomicU32 = AtomicU32::new(0);
static LAST_SPAWNED: AtomicU32 = AtomicU32::new(0);

struct Slot {
    index: usize,
    addr: usize,
    stride: usize,
}

fn layout(p: &Placement) -> Option<Vec<Slot>> {
    let tables = read_bytes(p.def, 0xf90)?;
    if tables.len() < 0xf90 {
        return None;
    }
    let mut out = Vec::new();
    for index in 0..COMPONENT_SLOTS {
        let word = u64_at(&tables, (index >> 6) * 8);
        if (word >> (index & 63)) & 1 == 0 {
            continue;
        }
        let offset = u16::from_le_bytes([tables[0x84 + index * 2], tables[0x85 + index * 2]]) as usize;
        let stride = u16::from_le_bytes([tables[0xa84 + index * 2], tables[0xa85 + index * 2]]) as usize;
        if stride == 0 {
            continue;
        }
        out.push(Slot { index, addr: p.chunk + offset + stride * p.slot, stride });
    }
    Some(out)
}

fn entity_layout(manager: usize, id: u32) -> Option<(usize, Placement, Vec<Slot>)> {
    if manager == 0 || id == 0 {
        return None;
    }
    let record = find_record(manager, id)?;
    let p = placement(record)?;
    let slots = layout(&p)?;
    Some((record, p, slots))
}

pub fn dump_entity(label: &str, manager: usize, id: u32) {
    let n = DUMP_COUNTER.fetch_add(1, Ordering::SeqCst) + 1;
    let Some((record, p, slots)) = entity_layout(manager, id) else {
        log(&format!("dump #{n} {label}: entity {id} not available"));
        return;
    };
    log(&format!("dump #{n} {label} id={id} record={record:#x} chunk={:#x} slot={}", p.chunk, p.slot));
    for slot in &slots {
        let bytes = read_bytes(slot.addr, slot.stride.min(1024)).unwrap_or_default();
        log(&format!("dump #{n} idx={} size={} data={}", slot.index, slot.stride, hex(&bytes)));
    }
    log(&format!("dump #{n} end"));
}

fn snapshot(manager: usize, id: u32) -> Option<Vec<Option<Vec<u8>>>> {
    let (_, _, slots) = entity_layout(manager, id)?;
    let mut out: Vec<Option<Vec<u8>>> = vec![None; COMPONENT_SLOTS];
    for slot in &slots {
        out[slot.index] = read_bytes(slot.addr, slot.stride.min(1024));
    }
    Some(out)
}

fn toggle_watch() {
    if WATCHING.swap(false, Ordering::SeqCst) {
        log("watch stopped");
        return;
    }
    WATCHING.store(true, Ordering::SeqCst);
    if let Err(e) = std::thread::Builder::new().name("m182-spawner-watch".into()).spawn(watch_loop) {
        WATCHING.store(false, Ordering::SeqCst);
        log(&format!("watch could not start: {e}"));
    }
}

fn watch_loop() {
    let targets = || {
        [
            ("server", SERVER_MGR.load(Ordering::SeqCst), PLAYER_ID.load(Ordering::SeqCst)),
            ("client", CLIENT_MGR.load(Ordering::SeqCst), CLIENT_PLAYER_ID.load(Ordering::SeqCst)),
        ]
    };
    log("watch started: measuring which components change on their own (3 seconds), then reporting the rest");

    let mut noisy: Vec<Vec<u32>> = vec![vec![0; COMPONENT_SLOTS]; 2];
    let mut previous: Vec<Option<Vec<Option<Vec<u8>>>>> = vec![None, None];
    for _ in 0..150 {
        if !WATCHING.load(Ordering::SeqCst) {
            return;
        }
        for (t, (_, mgr, id)) in targets().iter().enumerate() {
            let now = snapshot(*mgr, *id);
            if let (Some(prev), Some(cur)) = (previous[t].as_ref(), now.as_ref()) {
                for i in 0..COMPONENT_SLOTS {
                    if prev[i] != cur[i] {
                        noisy[t][i] += 1;
                    }
                }
            }
            previous[t] = now;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let skipped: usize = noisy.iter().map(|v| v.iter().filter(|c| **c >= 3).count()).sum();
    log(&format!("watch ready: {skipped} constantly changing components ignored. Do your action now."));

    let mut reported = 0u32;
    while WATCHING.load(Ordering::SeqCst) && reported < 4000 {
        for (t, (label, mgr, id)) in targets().iter().enumerate() {
            let Some(cur) = snapshot(*mgr, *id) else { continue };
            if let Some(prev) = previous[t].as_ref() {
                for i in 0..COMPONENT_SLOTS {
                    if noisy[t][i] >= 3 || prev[i] == cur[i] {
                        continue;
                    }
                    let old = prev[i].as_ref().map(|b| hex(b)).unwrap_or_default();
                    let new = cur[i].as_ref().map(|b| hex(b)).unwrap_or_default();
                    log(&format!("watch {label} idx={i} old={old} new={new}"));
                    reported += 1;
                }
            }
            previous[t] = Some(cur);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    WATCHING.store(false, Ordering::SeqCst);
    log("watch ended");
}

pub fn write_bytes(addr: usize, data: &[u8]) -> bool {
    let mut written = 0usize;
    let ok = unsafe { WriteProcessMemory(GetCurrentProcess(), addr as *mut c_void, data.as_ptr().cast(), data.len(), &mut written) };
    ok != 0 && written == data.len()
}

fn f32_at(b: &[u8], off: usize) -> f32 {
    f32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

struct Keep {
    manager: usize,
    id: u32,
    health: u32,
    damage: f32,
    orig: Option<[f32; 2]>,
    born: Instant,
    ready: bool,
    missing: u8,
    rewrites: u32,
}

static KEEPS: Mutex<Vec<Keep>> = Mutex::new(Vec::new());
const KEEP_LIMIT: usize = 400;

/// Brings one spawned creature to the wanted health and damage. Only the maximum health is compared:
/// when the game resets it (it recomputes it from the level whenever the creature is activated), the
/// whole health is set again; otherwise the damage the creature has taken is left alone.
fn try_apply(manager: usize, id: u32, health: u32, damage: f32, orig: &mut Option<[f32; 2]>, rewrites: &mut u32) -> bool {
    let want_damage = (damage - 1.0).abs() >= 1e-6;
    if health == 0 && !want_damage {
        return true;
    }
    let Some((_, _, slots)) = entity_layout(manager, id) else { return false };
    let mut health_done = health == 0;
    let mut damage_done = !want_damage;
    for slot in &slots {
        let Some(mut b) = read_bytes(slot.addr, slot.stride) else { continue };
        if b.len() < slot.stride {
            continue;
        }
        if slot.stride == 40 && health > 0 && u32_at(&b, 0) == HEALTH_SIGNATURE {
            let current = u32_at(&b, 8);
            if current == 0 {
                // dead (or not initialised yet): never bring it back
                if u32_at(&b, 36) > 0 {
                    open_loot_watch(id);
                }
                health_done = true;
                continue;
            }
            let maximum = u32_at(&b, 36);
            if maximum != health {
                for off in [8usize, 16, 36] {
                    b[off..off + 4].copy_from_slice(&health.to_le_bytes());
                }
                let ok = write_bytes(slot.addr, &b);
                *rewrites += 1;
                log(&format!("stats: entity {id} health {current}/{maximum} -> {health} (written={ok}, times={})", *rewrites));
            } else if current > health {
                b[8..12].copy_from_slice(&health.to_le_bytes());
                write_bytes(slot.addr, &b);
            }
            health_done = true;
        } else if slot.stride == 24 && want_damage && u32_at(&b, 0) == DAMAGE_SIGNATURE && f32_at(&b, 8) > 0.0 {
            let base = *orig.get_or_insert([f32_at(&b, 8), f32_at(&b, 12)]);
            let target = [base[0] * damage, base[1] * damage];
            if f32_at(&b, 8) != target[0] || f32_at(&b, 12) != target[1] {
                b[8..12].copy_from_slice(&target[0].to_le_bytes());
                b[12..16].copy_from_slice(&target[1].to_le_bytes());
                let ok = write_bytes(slot.addr, &b);
                log(&format!("stats: entity {id} damage {} -> {} (x{damage}, written={ok})", base[0], target[0]));
            }
            damage_done = true;
        }
    }
    health_done && damage_done
}

pub fn apply_stats(manager: usize, id: u32, health: u32, damage: f32) -> bool {
    let mut orig = None;
    let mut rewrites = 0;
    try_apply(manager, id, health, damage, &mut orig, &mut rewrites)
}

/// One pass over every creature that has custom stats. Runs forever in its own thread, so the
/// values survive however late the game decides to reset them.
pub fn keeper_pass() {
    let current = SERVER_MGR.load(Ordering::SeqCst);
    let Ok(mut list) = KEEPS.lock() else { return };
    list.retain(|k| k.manager == current);
    for k in list.iter_mut() {
        if entity_layout(k.manager, k.id).is_none() {
            k.missing = k.missing.saturating_add(1);
            continue;
        }
        k.missing = 0;
        let done = try_apply(k.manager, k.id, k.health, k.damage, &mut k.orig, &mut k.rewrites);
        if done && !k.ready {
            k.ready = true;
            log(&format!("stats: entity {} ready after {} ms", k.id, k.born.elapsed().as_millis()));
        }
    }
    list.retain(|k| k.missing < 40);
}

fn keeper_loop() {
    let mut ticks = 0u32;
    loop {
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::panic::catch_unwind(keeper_pass);
        ticks = ticks.wrapping_add(1);
        if ticks % 130 == 0 {
            let _ = std::panic::catch_unwind(save_loot_nodes);
        }
    }
}

pub fn start_stats(manager: usize, id: u32, health: u32, damage: f32) {
    if health == 0 && (damage - 1.0).abs() < 1e-6 {
        return;
    }
    let Ok(mut list) = KEEPS.lock() else { return };
    list.retain(|k| !(k.manager == manager && k.id == id));
    list.push(Keep { manager, id, health, damage, orig: None, born: Instant::now(), ready: false, missing: 0, rewrites: 0 });
    if list.len() > KEEP_LIMIT {
        let extra = list.len() - KEEP_LIMIT;
        list.drain(0..extra);
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct StatsPatch {
    dmg_new: String,
    dmg_old: Vec<String>,
    arm_new: String,
    arm_old: Vec<String>,
    lvl_new: String,
    lvl_old: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Request {
    template: String,
    level: Option<u32>,
    count: Option<u32>,
    distance: Option<f32>,
    health: Option<u32>,
    damage: Option<f32>,
    at: Option<[f64; 3]>,
    object: Option<bool>,
    forward: Option<bool>,
    item: Option<u32>,
    item_level: Option<u32>,
    item_rarity: Option<u32>,
    item_amount: Option<u32>,
    item_guid: Option<String>,
    stats: Option<StatsPatch>,
}

static DROP_FACTOR: OnceLock<f64> = OnceLock::new();

fn server_drop_factor() -> f64 {
    *DROP_FACTOR.get_or_init(|| {
        let factor = std::env::current_dir()
            .ok()
            .and_then(|d| std::fs::read_to_string(d.join("enshrouded_server.json")).ok())
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v["gameSettings"]["resourceDropStackAmountFactor"].as_f64())
            .filter(|f| f.is_finite() && *f > 0.0)
            .unwrap_or(1.0);
        log(&format!("loot: the server multiplies picked-up stacks by {factor}; configured amounts are divided by it"));
        factor
    })
}

fn compensated_amount(amount: u32) -> u32 {
    let f = server_drop_factor();
    if (f - 1.0).abs() < 1e-6 {
        return amount.clamp(1, 65535);
    }
    ((amount as f64 / f).round() as u32).clamp(1, 65535)
}

#[derive(Debug, Clone, Copy)]
pub struct GiveSpec {
    pub item: u32,
    pub level: u32,
    pub rarity: u32,
    pub amount: u32,
    pub guid: [u8; 16],
}

static REQUEST_GIVE: Mutex<Option<GiveSpec>> = Mutex::new(None);
static REQUEST_AT: Mutex<Option<[f64; 3]>> = Mutex::new(None);
static REQUEST_SENDER: AtomicU32 = AtomicU32::new(0);
static REQUEST_OBJECT: AtomicBool = AtomicBool::new(false);
static REQUEST_FORWARD: AtomicBool = AtomicBool::new(false);
static FACE_SCORES: Mutex<[f64; 4]> = Mutex::new([0.0; 4]);
static FACE_SAMPLES: AtomicU32 = AtomicU32::new(0);
static FACE_CHOSEN: AtomicU32 = AtomicU32::new(0);
static REQUEST: Mutex<Option<SpawnSettings>> = Mutex::new(None);
static SPAWN_TAG: Mutex<Option<String>> = Mutex::new(None);
static CREATED: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());
const REQUEST_FILE: &str = "m182_spawn.request";

static DELIVERIES: Mutex<std::collections::VecDeque<(GiveSpec, String, u32)>> = Mutex::new(std::collections::VecDeque::new());

fn deliveries_tick() {
    if PENDING.load(Ordering::SeqCst) != 0 || REQUEST.lock().map(|r| r.is_some()).unwrap_or(true) {
        return;
    }
    let Some((spec, template, entity)) = DELIVERIES.lock().ok().and_then(|mut q| q.pop_front()) else {
        drops_tick();
        return;
    };
    let settings = SpawnSettings {
        template,
        level: 1,
        count: 1,
        distance: 2.0,
        health: 0,
        damage: 1.0,
        key: 0,
        dump_player_key: 0,
        dump_spawned_key: 0,
        watch_key: 0,
    };
    if let Ok(mut r) = REQUEST.lock() {
        *r = Some(settings);
    }
    if let Ok(mut g) = REQUEST_GIVE.lock() {
        *g = Some(spec);
    }
    if let Ok(mut r) = REQUEST_AT.lock() {
        *r = None;
    }
    REQUEST_SENDER.store(entity, Ordering::SeqCst);
    REQUEST_OBJECT.store(true, Ordering::SeqCst);
    REQUEST_FORWARD.store(false, Ordering::SeqCst);
    if let Ok(mut t) = SPAWN_TAG.lock() {
        *t = None;
    }
    PENDING.store(1, Ordering::SeqCst);
    log(&format!("delivery: dropping item {} next to entity {entity}", spec.item));
}

pub fn poll_request() {
    let Ok(dir) = std::env::current_dir() else { return };
    let path = dir.join(REQUEST_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let _ = std::fs::remove_file(&path);
    let Ok(req) = serde_json::from_str::<Request>(&text) else {
        log("panel request ignored: invalid file");
        return;
    };
    if let Ok(mut t) = SPAWN_TAG.lock() {
        *t = None;
    }
    REQUEST_SENDER.store(super::chat::REQUESTER.swap(0, Ordering::SeqCst), Ordering::SeqCst);
    let mut s = match SPAWN.lock() {
        Ok(s) => s.clone(),
        Err(_) => return,
    };
    if !req.template.trim().is_empty() {
        s.template = req.template.trim().to_string();
    }
    if let Some(v) = req.level {
        s.level = v.min(1000);
    }
    if let Some(v) = req.count {
        s.count = v.clamp(1, 20);
    }
    if let Some(v) = req.distance {
        s.distance = v.clamp(1.0, 60.0);
    }
    if let Some(v) = req.health {
        s.health = v.min(2_000_000_000);
    }
    if let Some(v) = req.damage {
        s.damage = v.clamp(0.0, 1000.0);
    }
    if let Ok(mut r) = REQUEST.lock() {
        *r = Some(s);
    }
    if let Ok(mut g) = REQUEST_GIVE.lock() {
        *g = req.item.filter(|i| *i != 0).map(|item| GiveSpec {
            item,
            level: req.item_level.unwrap_or(1).clamp(1, 255),
            rarity: req.item_rarity.unwrap_or(0).min(4),
            amount: req.item_amount.unwrap_or(1).clamp(1, 65535),
            guid: req.item_guid.as_deref().and_then(super::config::parse_guid).unwrap_or([0u8; 16]),
        });
    }
    REQUEST_OBJECT.store(req.object.unwrap_or(false), Ordering::SeqCst);
    REQUEST_FORWARD.store(req.forward.unwrap_or(false), Ordering::SeqCst);
    if let Ok(mut r) = REQUEST_AT.lock() {
        *r = req.at.filter(|p| p.iter().all(|v| v.is_finite() && v.abs() < 1.0e6));
    }
    let give = REQUEST_GIVE.lock().ok().and_then(|g| *g);
    match (req.stats, give) {
        (Some(stats), Some(spec)) if spec.guid != [0u8; 16] => {
            let started = std::thread::Builder::new().name("m182-item-patch".into()).spawn(move || {
                apply_stats_patch(spec.item, &spec.guid, &stats);
                PENDING.store(1, Ordering::SeqCst);
                log("spawn requested from the panel");
            });
            if started.is_err() {
                PENDING.store(1, Ordering::SeqCst);
                log("spawn requested from the panel");
            }
        }
        _ => {
            PENDING.store(1, Ordering::SeqCst);
            log("spawn requested from the panel");
        }
    }
}

static PATCHED: Mutex<Option<HashMap<u32, Vec<(usize, Vec<u8>)>>>> = Mutex::new(None);

fn patched_still_holds(item: u32, wanted: &[Vec<u8>]) -> bool {
    let Ok(guard) = PATCHED.lock() else { return false };
    let Some(map) = guard.as_ref() else { return false };
    let Some(list) = map.get(&item) else { return false };
    if list.is_empty() {
        return false;
    }
    let same = wanted.iter().all(|w| list.iter().any(|(_, bytes)| bytes == w));
    same && list.iter().all(|(addr, bytes)| matches!(read_bytes(*addr, bytes.len()), Some(b) if &b == bytes))
}

pub fn patch_item_struct(
    guid: &[u8; 16],
    item_id: u32,
    blocks: &[(usize, Vec<u8>, Vec<Vec<u8>>)],
    regions: &[(usize, usize)],
) -> (Vec<(usize, Vec<u8>)>, String) {
    let hits = scan_regions(regions, guid, 256);
    let mut notes = format!("{} guid hits", hits.len());
    let mut bases: Vec<usize> = Vec::new();
    for h in hits {
        if h < 0x10000 {
            continue;
        }
        let base = h - 4;
        let Some(head) = read_bytes(base, 4) else { continue };
        if head.len() < 4 || u32_at(&head, 0) != item_id {
            continue;
        }
        bases.push(base);
    }
    let done = patch_item_bases(&bases, blocks, &mut notes);
    (done, notes)
}

/// Same patch for item copies: a copy has its own item id but the identifiers kept inside its
/// definition are not guaranteed to match the copy's resource guid, so it is found by its id and
/// accepted only when the damage block it holds is exactly the one expected.
pub fn patch_item_struct_by_id(
    item_id: u32,
    blocks: &[(usize, Vec<u8>, Vec<Vec<u8>>)],
    regions: &[(usize, usize)],
) -> (Vec<(usize, Vec<u8>)>, String) {
    let hits = scan_regions(regions, &item_id.to_le_bytes(), 8192);
    let mut notes = format!("{} id hits", hits.len());
    let bases: Vec<usize> = hits.into_iter().filter(|h| h % 4 == 0 && *h >= 0x10000).collect();
    let done = patch_item_bases(&bases, blocks, &mut notes);
    (done, notes)
}

fn patch_item_bases(bases: &[usize], blocks: &[(usize, Vec<u8>, Vec<Vec<u8>>)], notes: &mut String) -> Vec<(usize, Vec<u8>)> {
    let mut done: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    for &base in bases {
        let span = read_window(base, ITEM_INFO_SPAN);
        for (offset, new, olds) in blocks {
            if new.is_empty() || span.len() < offset + new.len() {
                notes.push_str("; struct too short");
                continue;
            }
            let current = &span[*offset..*offset + new.len()];
            let mut target: Option<usize> = None;
            if current == &new[..] || olds.iter().any(|o| o.len() == new.len() && o == current) {
                target = Some(base + offset);
            } else {
                for old in olds {
                    if old.len() != new.len() || old.len() < 16 || old.iter().all(|b| *b == 0) {
                        continue;
                    }
                    let mut found: Vec<usize> = Vec::new();
                    let mut i = 0usize;
                    while i + old.len() <= span.len() {
                        if span[i..i + old.len()] == old[..] {
                            found.push(i);
                        }
                        i += 4;
                    }
                    if found.len() == 1 {
                        notes.push_str(&format!("; block found at +{} instead of +{}", found[0], offset));
                        target = Some(base + found[0]);
                        break;
                    }
                }
            }
            match target {
                Some(addr) => {
                    if !seen.insert(addr) {
                        continue;
                    }
                    let ok = read_bytes(addr, new.len()).map(|b| b == *new).unwrap_or(false) || (write_anywhere(addr, new) && read_bytes(addr, new.len()).map(|b| b == *new).unwrap_or(false));
                    if ok {
                        done.push((addr, new.clone()));
                    } else {
                        notes.push_str("; write refused");
                    }
                }
                None => notes.push_str(&format!("; layout differs at +{} (found {})", offset, hex(&current[..current.len().min(16)]))),
            }
        }
    }
    done
}

fn apply_stats_patch(item: u32, guid: &[u8; 16], stats: &StatsPatch) {
    let mut blocks: Vec<(usize, Vec<u8>, Vec<Vec<u8>>)> = Vec::new();
    let dmg_new = hex_to_bytes(&stats.dmg_new);
    if dmg_new.len() == 60 {
        blocks.push((ITEM_DAMAGE_OFFSET, dmg_new, stats.dmg_old.iter().map(|s| hex_to_bytes(s)).filter(|v| v.len() == 60).collect()));
    }
    let arm_new = hex_to_bytes(&stats.arm_new);
    if arm_new.len() == 36 {
        blocks.push((ITEM_ARMOR_OFFSET, arm_new, stats.arm_old.iter().map(|s| hex_to_bytes(s)).filter(|v| v.len() == 36).collect()));
    }
    let lvl_new = hex_to_bytes(&stats.lvl_new);
    if lvl_new.len() == 8 {
        blocks.push((ITEM_LEVEL_OFFSET, lvl_new, stats.lvl_old.iter().map(|s| hex_to_bytes(s)).filter(|v| v.len() == 8).collect()));
    }
    if blocks.is_empty() {
        return;
    }
    let wanted: Vec<Vec<u8>> = blocks.iter().map(|b| b.1.clone()).collect();
    if patched_still_holds(item, &wanted) {
        log(&format!("item stats {}: already applied", item));
        return;
    }
    let started = Instant::now();
    let regions = readable_regions();
    let (mut done, mut notes) = patch_item_struct(guid, item, &blocks, &regions);
    if done.is_empty() {
        let (by_id, more) = patch_item_struct_by_id(item, &blocks, &regions);
        notes.push_str(&format!("; {more}"));
        done = by_id;
    }
    log(&format!(
        "item stats {}: patched {} block(s) ({notes}) in {} ms over {} regions",
        item,
        done.len(),
        started.elapsed().as_millis(),
        regions.len()
    ));
    if !done.is_empty() {
        if let Ok(mut guard) = PATCHED.lock() {
            let list = guard.get_or_insert_with(HashMap::new).entry(item).or_default();
            for (addr, bytes) in done {
                match list.iter_mut().find(|(a, _)| *a == addr) {
                    Some(slot) => slot.1 = bytes,
                    None => list.push((addr, bytes)),
                }
            }
        }
    }
}

const FACE_AXES: [[f64; 3]; 4] = [[0.0, 0.0, 1.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0], [-1.0, 0.0, 0.0]];
const FACE_NAMES: [&str; 4] = ["+Z", "-Z", "+X", "-X"];

fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let u = [q[0], q[1], q[2]];
    let t = [
        2.0 * (u[1] * v[2] - u[2] * v[1]),
        2.0 * (u[2] * v[0] - u[0] * v[2]),
        2.0 * (u[0] * v[1] - u[1] * v[0]),
    ];
    let c = [u[1] * t[2] - u[2] * t[1], u[2] * t[0] - u[0] * t[2], u[0] * t[1] - u[1] * t[0]];
    [v[0] + q[3] * t[0] + c[0], v[1] + q[3] * t[1] + c[1], v[2] + q[3] * t[2] + c[2]]
}

fn quat_of(t: &[u8]) -> [f64; 4] {
    let f = |o: usize| f32::from_le_bytes([t[o], t[o + 1], t[o + 2], t[o + 3]]) as f64;
    [f(24), f(28), f(32), f(36)]
}

fn horizontal(v: [f64; 3]) -> Option<(f64, f64)> {
    let len = (v[0] * v[0] + v[2] * v[2]).sqrt();
    if len < 1.0e-3 || !len.is_finite() {
        None
    } else {
        Some((v[0] / len, v[2] / len))
    }
}

pub fn facing_update(prev: (i64, i64, i64), cur: (i64, i64, i64), transform: &[u8]) {
    if transform.len() < TRANSFORM_SIZE {
        return;
    }
    let dx = (cur.0 - prev.0) as f64 / METERS;
    let dz = (cur.2 - prev.2) as f64 / METERS;
    let dist = (dx * dx + dz * dz).sqrt();
    if !(2.0..=40.0).contains(&dist) {
        return;
    }
    let movement = (dx / dist, dz / dist);
    let q = quat_of(transform);
    let Ok(mut scores) = FACE_SCORES.lock() else { return };
    for (i, axis) in FACE_AXES.iter().enumerate() {
        if let Some(d) = horizontal(rotate(q, *axis)) {
            scores[i] += d.0 * movement.0 + d.1 * movement.1;
        }
    }
    let samples = FACE_SAMPLES.fetch_add(1, Ordering::SeqCst) + 1;
    let best = (0..4).max_by(|a, b| scores[*a].partial_cmp(&scores[*b]).unwrap_or(std::cmp::Ordering::Equal)).unwrap_or(0);
    if samples >= 3 && FACE_CHOSEN.swap(best as u32, Ordering::SeqCst) != best as u32 {
        log(&format!("facing axis {} (samples={samples})", FACE_NAMES[best]));
    }
}

pub fn facing_direction(transform: &[u8]) -> (f64, f64) {
    if transform.len() < TRANSFORM_SIZE {
        return (0.0, 1.0);
    }
    let axis = FACE_AXES[(FACE_CHOSEN.load(Ordering::SeqCst) as usize).min(3)];
    horizontal(rotate(quat_of(transform), axis)).unwrap_or((0.0, 1.0))
}

static HOSTING: AtomicU8 = AtomicU8::new(2);
static SERVER_PLAYER_MS: AtomicU64 = AtomicU64::new(0);
const WORLD_PAIR_MS: u64 = 45_000;

pub fn drop_stale_world(client_manager: usize, previous_client: usize, now: u64, server_player_ms: u64) -> bool {
    previous_client != client_manager && server_player_ms.saturating_add(WORLD_PAIR_MS) < now
}

const HOSTED_WORLD_GONE_CHECKS: u32 = 10;

/// A game that hosted a world and is now connected to a server keeps pointing at the world it
/// left, and would go on applying the panel's commands to it: forget that world once the player
/// of it can no longer be read for a while.
pub fn check_hosted_world() {
    static GONE: AtomicU32 = AtomicU32::new(0);
    let (sm, sid) = (SERVER_MGR.load(Ordering::SeqCst), PLAYER_ID.load(Ordering::SeqCst));
    if sm == 0 || CLIENT_MGR.load(Ordering::SeqCst) == 0 || TRANSFORM_INDEX.load(Ordering::SeqCst) < 0 || transform_of(sm, sid).is_some() {
        GONE.store(0, Ordering::SeqCst);
        return;
    }
    if GONE.fetch_add(1, Ordering::SeqCst) + 1 < HOSTED_WORLD_GONE_CHECKS {
        return;
    }
    GONE.store(0, Ordering::SeqCst);
    if SERVER_MGR.compare_exchange(sm, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
        PLAYER_ID.store(0, Ordering::SeqCst);
        log("the world this game hosted is gone; now connected to a remote server");
    }
}

/// A game connected to a dedicated server sees every player's character, and the first one it
/// meets is not necessarily its own. The panel reads the server's player list, finds this
/// account in it and writes the entity number; from then on that character is "me".
fn apply_own_character() {
    static LOADED_AT: OnceLock<std::time::SystemTime> = OnceLock::new();
    static REFUSED: AtomicU32 = AtomicU32::new(0);
    let started = *LOADED_AT.get_or_init(std::time::SystemTime::now);
    if SERVER_MGR.load(Ordering::SeqCst) != 0 {
        return;
    }
    let manager = CLIENT_MGR.load(Ordering::SeqCst);
    if manager == 0 {
        return;
    }
    let Ok(dir) = std::env::current_dir() else { return };
    let path = dir.join(roster::SELF_FILE);
    let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else { return };
    if modified < started {
        return;
    }
    let Some(entity) = std::fs::read_to_string(&path).ok().and_then(|t| roster::parse_self(&t)) else { return };
    if CLIENT_SELF.load(Ordering::SeqCst) == entity && CLIENT_PLAYER_ID.load(Ordering::SeqCst) == entity {
        return;
    }
    if find_record(manager, entity).is_none() {
        if REFUSED.swap(entity, Ordering::Relaxed) != entity {
            log(&format!("client: entity {entity} from the player list is not in this game's world yet"));
        }
        return;
    }
    CLIENT_SELF.store(entity, Ordering::SeqCst);
    let before = CLIENT_PLAYER_ID.swap(entity, Ordering::SeqCst);
    CLIENT_TRANSFORM_INDEX.store(-1, Ordering::SeqCst);
    CLIENT_GUESSES.store(0, Ordering::SeqCst);
    log(&format!("client: this game's character is entity {entity} (it was following entity {before})"));
}

fn write_mode() {
    let hosting = if SERVER_MGR.load(Ordering::SeqCst) != 0 {
        1
    } else if CLIENT_MGR.load(Ordering::SeqCst) != 0 {
        0
    } else {
        return;
    };
    if HOSTING.swap(hosting, Ordering::SeqCst) == hosting {
        return;
    }
    let Ok(dir) = std::env::current_dir() else { return };
    let tmp = dir.join("m182_mode.json.tmp");
    if std::fs::write(&tmp, format!("{{\"hosting\":{}}}", hosting == 1)).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join("m182_mode.json"));
    }
    log(&format!("mode: {}", if hosting == 1 { "this game hosts the world" } else { "connected to a remote server" }));
}

pub fn write_position(pos: (i64, i64, i64)) {
    let Ok(dir) = std::env::current_dir() else { return };
    let text = format!(
        "{{\"x\":{:.2},\"y\":{:.2},\"z\":{:.2}}}",
        pos.0 as f64 / METERS,
        pos.1 as f64 / METERS,
        pos.2 as f64 / METERS
    );
    let tmp = dir.join("m182_player.json.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join("m182_player.json"));
    }
}

fn entity_ids(manager: usize) -> Option<HashSet<u32>> {
    if manager == 0 {
        return None;
    }
    let head = read_bytes(manager + 0x148, 0x58)?;
    let capacity = u64_at(&head, 0x10);
    if capacity == 0 || capacity > (1 << 22) || capacity & (capacity - 1) != 0 {
        return None;
    }
    let words = (capacity as usize).div_ceil(64);
    let bitmap = read_bytes(u64_at(&head, 8) as usize, words * 8)?;
    let keys = read_bytes(u64_at(&head, 0x28) as usize, capacity as usize * 4)?;
    if bitmap.len() < words * 8 || keys.len() < capacity as usize * 4 {
        return None;
    }
    let mut out = HashSet::new();
    for slot in 0..capacity as usize {
        if (u64_at(&bitmap, (slot >> 6) * 8) >> (slot & 63)) & 1 == 1 {
            out.insert(u32_at(&keys, slot * 4));
        }
    }
    Some(out)
}

static CENSUS: Mutex<Option<(HashSet<u32>, HashSet<u32>)>> = Mutex::new(None);

pub fn census() {
    let server = SERVER_MGR.load(Ordering::SeqCst);
    let client = CLIENT_MGR.load(Ordering::SeqCst);
    let now = (entity_ids(server).unwrap_or_default(), entity_ids(client).unwrap_or_default());
    let Ok(mut saved) = CENSUS.lock() else { return };
    let Some(before) = saved.take() else {
        log(&format!("census saved: {} server and {} client entities. Do your action, then press the key again.", now.0.len(), now.1.len()));
        *saved = Some(now);
        return;
    };
    drop(saved);
    for (label, manager, old, new) in [("server", server, &before.0, &now.0), ("client", client, &before.1, &now.1)] {
        let mut added: Vec<u32> = new.difference(old).copied().collect();
        let mut removed: Vec<u32> = old.difference(new).copied().collect();
        added.sort_unstable();
        removed.sort_unstable();
        log(&format!("census {label}: {} new, {} removed", added.len(), removed.len()));
        log(&format!("census {label} removed ids: {:?}", &removed[..removed.len().min(40)]));
        log(&format!("census {label} new ids: {:?}", &added[..added.len().min(40)]));
        for id in added.iter().take(12) {
            dump_entity(&format!("census new {label}"), manager, *id);
        }
    }
}

fn le_i64(b: &[u8], off: usize) -> i64 {
    u64_at(b, off) as i64
}

fn is_server(manager: usize) -> bool {
    matches!(read_bytes(manager + 8, 1), Some(b) if b[0] == 0)
}

fn bit_set(def: usize, index: usize) -> bool {
    match read_bytes(def + (index >> 6) * 8, 8) {
        Some(b) => (u64_at(&b, 0) >> (index & 63)) & 1 == 1,
        None => false,
    }
}

fn hash_slot(id: u32, capacity: u64) -> u64 {
    let mut a = (id >> 16) ^ id;
    a = a.wrapping_mul(0x045d_9f3b);
    let mut b = (a >> 16) ^ a;
    b = b.wrapping_mul(0x045d_9f3b);
    ((b as u64 >> 16) ^ b as u64) & capacity.wrapping_sub(1)
}

pub(super) fn find_record(manager: usize, id: u32) -> Option<usize> {
    let map = manager + 0x148;
    let head = read_bytes(map, 0x58)?;
    let capacity = u64_at(&head, 0x10);
    let count = u32_at(&head, 0x54);
    if count == 0 || capacity == 0 || capacity > (1 << 26) || capacity & (capacity - 1) != 0 {
        return None;
    }
    let bitmap = u64_at(&head, 8) as usize;
    let keys = u64_at(&head, 0x28) as usize;
    let values = u64_at(&head, 0x40) as usize;

    let mut slot = hash_slot(id, capacity);
    for _ in 0..capacity.min(4096) {
        let word = read_bytes(bitmap + (slot as usize >> 6) * 8, 8)?;
        if (u64_at(&word, 0) >> (slot & 63)) & 1 == 0 {
            return None;
        }
        let key = read_bytes(keys + slot as usize * 4, 4)?;
        if u32_at(&key, 0) == id {
            let value = read_bytes(values + slot as usize * 8, 8)?;
            let record = u64_at(&value, 0) as usize;
            if record == 0 {
                return None;
            }
            let check = read_bytes(record + 0x10, 4)?;
            return if u32_at(&check, 0) == id { Some(record) } else { None };
        }
        slot = if slot + 1 < capacity { slot + 1 } else { 0 };
    }
    None
}

pub(super) struct Placement {
    def: usize,
    chunk: usize,
    slot: usize,
}

pub(super) fn placement(record: usize) -> Option<Placement> {
    let b = read_bytes(record, 0x38)?;
    let def = u64_at(&b, 0x18) as usize;
    let chunk = u64_at(&b, 0x20) as usize;
    let slot = u32_at(&b, 0x30) as usize;
    if def < 0x10000 || chunk < 0x10000 {
        return None;
    }
    Some(Placement { def, chunk, slot })
}

pub(super) fn component_address(p: &Placement, index: usize) -> Option<(usize, usize)> {
    if index >= COMPONENT_SLOTS || !bit_set(p.def, index) {
        return None;
    }
    let offset = read_bytes(p.def + 0x84 + index * 2, 2)?;
    let stride = read_bytes(p.def + 0xa84 + index * 2, 2)?;
    let offset = u16::from_le_bytes([offset[0], offset[1]]) as usize;
    let stride = u16::from_le_bytes([stride[0], stride[1]]) as usize;
    Some((p.chunk + offset + stride * p.slot, stride))
}

fn calibrate(manager: usize, id: u32, expected: &[u8]) -> bool {
    let Some(record) = find_record(manager, id) else {
        dump_failure(manager, id, None, "record lookup failed");
        return false;
    };
    let Some(p) = placement(record) else {
        dump_failure(manager, id, Some(record), "record fields unreadable");
        return false;
    };
    for index in 0..COMPONENT_SLOTS {
        let Some((addr, stride)) = component_address(&p, index) else { continue };
        if stride != TRANSFORM_SIZE {
            continue;
        }
        if let Some(b) = read_bytes(addr, 24) {
            if b == expected {
                transform_cell(manager).store(index as i32, Ordering::SeqCst);
                log(&format!("transform component found: index={index} id={id} record={record:#x} chunk={:#x} slot={} ({})", p.chunk, p.slot, if is_client_manager(manager) { "client" } else { "server" }));
                return true;
            }
        }
    }
    dump_failure(manager, id, Some(record), "no 56-byte component matched");
    false
}

fn dump_failure(manager: usize, id: u32, record: Option<usize>, why: &str) {
    if CALIBRATION_DUMPS.fetch_add(1, Ordering::Relaxed) >= 3 {
        return;
    }
    let mut text = format!("calibration failed ({why}) id={id} mgr={manager:#x}");
    if let Some(r) = record {
        if let Some(b) = read_bytes(r, 0x60) {
            text.push_str(&format!(" record={r:#x} bytes={}", hex(&b)));
            let def = u64_at(&b, 0x18) as usize;
            if let Some(d) = read_bytes(def, 0x90) {
                text.push_str(&format!(" def={def:#x} defbytes={}", hex(&d)));
            }
        }
    }
    log(&text);
}

fn is_client_manager(manager: usize) -> bool {
    let c = CLIENT_MGR.load(Ordering::SeqCst);
    c != 0 && manager == c && manager != SERVER_MGR.load(Ordering::SeqCst)
}

fn transform_cell(manager: usize) -> &'static AtomicI32 {
    if is_client_manager(manager) { &CLIENT_TRANSFORM_INDEX } else { &TRANSFORM_INDEX }
}

/// The entity this game controls: the server's player when this process hosts the world,
/// otherwise the character shown on screen (a game connected to a dedicated server).
fn world_pair() -> Option<(usize, u32)> {
    let (sm, sid) = (SERVER_MGR.load(Ordering::SeqCst), PLAYER_ID.load(Ordering::SeqCst));
    let (cm, cid) = (CLIENT_MGR.load(Ordering::SeqCst), CLIENT_PLAYER_ID.load(Ordering::SeqCst));
    let forced = LIVE.force_client();
    if sm != 0 && sid != 0 && !forced {
        return Some((sm, sid));
    }
    if cm != 0 && cid != 0 && (sm == 0 || forced || cm != sm) {
        return Some((cm, cid));
    }
    None
}

fn remote_world() -> bool {
    world_pair().map(|(m, _)| is_client_manager(m)).unwrap_or(false)
}

fn plausible_position(b: &[u8]) -> bool {
    if b.len() < TRANSFORM_SIZE {
        return false;
    }
    let (x, y, z) = (le_i64(b, 0) as f64 / METERS, le_i64(b, 8) as f64 / METERS, le_i64(b, 16) as f64 / METERS);
    x.abs() < 40_000.0 && z.abs() < 40_000.0 && (-2_000.0..6_000.0).contains(&y) && (x != 0.0 || z != 0.0)
}

/// Finds the transform component of the character shown on screen when the descriptor it was
/// created from did not reveal it.
fn guess_client_transform(manager: usize, id: u32) {
    if CLIENT_TRANSFORM_INDEX.load(Ordering::SeqCst) >= 0 || CLIENT_GUESSES.load(Ordering::Relaxed) >= 40 {
        return;
    }
    CLIENT_GUESSES.fetch_add(1, Ordering::Relaxed);
    let Some(record) = find_record(manager, id) else { return };
    let Some(p) = placement(record) else { return };
    let mut candidates: Vec<(usize, Vec<u8>)> = Vec::new();
    for index in 0..COMPONENT_SLOTS {
        let Some((addr, stride)) = component_address(&p, index) else { continue };
        if stride != TRANSFORM_SIZE {
            continue;
        }
        if let Some(b) = read_bytes(addr, TRANSFORM_SIZE) {
            if plausible_position(&b) {
                candidates.push((index, b));
            }
        }
    }
    let known = TRANSFORM_INDEX.load(Ordering::SeqCst);
    let pick = candidates
        .iter()
        .find(|(i, _)| *i as i32 == known)
        .or_else(|| candidates.iter().find(|(i, _)| *i == 133))
        .or_else(|| candidates.first());
    if let Some((index, b)) = pick {
        CLIENT_TRANSFORM_INDEX.store(*index as i32, Ordering::SeqCst);
        log(&format!(
            "transform component found by position: index={index} of {} candidates, player at x={:.1} y={:.1} z={:.1} (client)",
            candidates.len(),
            le_i64(b, 0) as f64 / METERS,
            le_i64(b, 8) as f64 / METERS,
            le_i64(b, 16) as f64 / METERS
        ));
    } else if CLIENT_GUESSES.load(Ordering::Relaxed) == 1 || CLIENT_GUESSES.load(Ordering::Relaxed) == 40 {
        log("transform component of the character on screen not found yet");
    }
}

pub(super) fn transform_of(manager: usize, id: u32) -> Option<Vec<u8>> {
    let index = transform_cell(manager).load(Ordering::SeqCst);
    if index < 0 || id == 0 || manager == 0 {
        return None;
    }
    let record = find_record(manager, id)?;
    let p = placement(record)?;
    let (addr, stride) = component_address(&p, index as usize)?;
    if stride != TRANSFORM_SIZE {
        return None;
    }
    read_bytes(addr, TRANSFORM_SIZE)
}

fn player_transform() -> Option<Vec<u8>> {
    let (manager, id) = world_pair()?;
    transform_of(manager, id)
}

fn component_data(entries: &[u8], count: usize, hash: u32, size: usize) -> Option<Vec<u8>> {
    for i in 0..count {
        if u32_at(entries, i * 24) == hash {
            let data = u64_at(entries, i * 24 + 8) as usize;
            let len = u64_at(entries, i * 24 + 16) as usize;
            if len >= size {
                return read_bytes(data, size);
            }
        }
    }
    None
}

pub fn tick() {
    if PENDING.load(Ordering::Relaxed) == 0 && AURA_PENDING.load(Ordering::Relaxed) == 0 {
        return;
    }
    let manager = SERVER_MGR.load(Ordering::Relaxed);
    if manager != 0 {
        before(manager, 1);
    }
}

pub fn before(manager: usize, edx: u32) {
    if PENDING.load(Ordering::Relaxed) == 0 && AURA_PENDING.load(Ordering::Relaxed) == 0 && !super::vfx::pending() {
        return;
    }
    if edx == 0 || edx >= 0x8000_0000 || manager != SERVER_MGR.load(Ordering::Relaxed) || !is_server(manager) {
        return;
    }
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    if PENDING.compare_exchange(1, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
        spawn_now(manager);
    }
    run_aura_queue(manager);
    super::vfx::run(manager);
    BUSY.store(false, Ordering::SeqCst);
}

static AURA_PENDING: AtomicU32 = AtomicU32::new(0);
static AURA_QUEUE: Mutex<Vec<(i64, i64, i64, bool)>> = Mutex::new(Vec::new());
static AURA_FAILED: AtomicU32 = AtomicU32::new(0);

fn run_aura_queue(manager: usize) {
    if AURA_PENDING.swap(0, Ordering::SeqCst) == 0 {
        return;
    }
    let points: Vec<(i64, i64, i64, bool)> = match AURA_QUEUE.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => return,
    };
    let Some(pulse) = aura_guid(AURA_TEMPLATE) else { return };
    let entry = aura_guid(ENTRY_TEMPLATE).unwrap_or(pulse);
    for (x, y, z, entering) in points.into_iter().take(8) {
        let template = if entering { entry } else { pulse };
        if spawn_effect(manager, template, x, y, z) == 0 && AURA_FAILED.fetch_add(1, Ordering::Relaxed) == 0 {
            log("zones: the aura effect could not be created");
        }
    }
}

fn calibration_done() -> bool {
    TRANSFORM_INDEX.load(Ordering::Relaxed) >= 0 || CALIBRATION_TRIES.load(Ordering::Relaxed) >= MAX_CALIBRATION_TRIES
}

fn guid_text(b: &[u8]) -> String {
    if b.len() < 16 {
        return String::new();
    }
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

static LOOT_WATCH_UNTIL: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    CLOCK.get_or_init(Instant::now).elapsed().as_millis() as u64
}

static LOOT_WATCHED: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn open_loot_watch(id: u32) {
    if let Ok(mut seen) = LOOT_WATCHED.lock() {
        if seen.contains(&id) {
            return;
        }
        seen.push(id);
        if seen.len() > 64 {
            seen.remove(0);
        }
    }
    LOOT_WATCH_UNTIL.store(now_ms() + 20_000, Ordering::Relaxed);
    log(&format!("loot: spawned creature {id} died"));
}

static LOOT_GUIDS: OnceLock<Vec<([u8; 16], String)>> = OnceLock::new();
static LOOT_REPORTS: AtomicU32 = AtomicU32::new(0);
static LOOT_WORLD_REPORTS: AtomicU32 = AtomicU32::new(0);

fn loot_guids() -> &'static Vec<([u8; 16], String)> {
    LOOT_GUIDS.get_or_init(|| {
        let mut out = Vec::new();
        let Ok(dir) = std::env::current_dir() else { return out };
        let Ok(text) = std::fs::read_to_string(dir.join("export").join("m182_objects.csv")) else { return out };
        for line in text.lines().skip(1) {
            let mut parts = line.split(',');
            let (Some(name), Some(guid)) = (parts.next(), parts.next()) else { continue };
            if name.starts_with("LootContainer") {
                if let Some(g) = super::config::parse_guid(guid) {
                    out.push((g, name.to_string()));
                }
            }
        }
        out
    })
}

fn container_report(label: &str, manager: usize, id: u32) {
    let Some((_, _, slots)) = entity_layout(manager, id) else {
        log(&format!("loot: {label}: entity {id} is gone"));
        return;
    };
    let mut loot = Vec::new();
    let mut inventory = String::new();
    for slot in &slots {
        let Some(b) = read_bytes(slot.addr, slot.stride) else { continue };
        if slot.stride == 12 {
            loot.push(format!("{}:{}", slot.index, hex(&b)));
        } else if slot.stride == 96 && inventory.is_empty() {
            inventory = hex(&b);
        }
    }
    log(&format!("loot: {label}: entity {id} 12-byte components [{}] inventory {inventory}", loot.join(" ")));
}

/// Writes down the loot container the game builds when a creature dies, now and a little later,
/// so what a player finds when opening it can be compared with a creature from the world.
fn loot_container_check(manager: usize, desc: usize, ret: u32) {
    let ours = LOOT_WATCH_UNTIL.load(Ordering::Relaxed) > now_ms();
    if !ours && (LOOT_WORLD_REPORTS.load(Ordering::Relaxed) >= 2 || now_ms() < 60_000) {
        return;
    }
    if LOOT_REPORTS.load(Ordering::Relaxed) >= 8 {
        return;
    }
    let Some(head) = read_bytes(desc + 4, 16) else { return };
    let Some((_, name)) = loot_guids().iter().find(|(g, _)| head.len() >= 16 && g[..] == head[..16]) else { return };
    if !ours {
        LOOT_WORLD_REPORTS.fetch_add(1, Ordering::Relaxed);
    }
    if LOOT_REPORTS.fetch_add(1, Ordering::Relaxed) >= 8 {
        return;
    }
    let label = format!("{name} ({})", if ours { "creature from the panel" } else { "creature from the world" });
    let _ = std::thread::Builder::new().name("m182-loot-report".into()).spawn(move || {
        let mut waited = 0u64;
        for (k, at) in [1_500u64, 30_000, 90_000].into_iter().enumerate() {
            std::thread::sleep(Duration::from_millis(at - waited));
            waited = at;
            container_report(&format!("{label} after {} s", at / 1000), manager, ret);
            if !ours && k == 0 {
                break;
            }
        }
    });
}

const HASH_RANDOM_LOOT_CONTAINER: u32 = 0x061d_da01;
const RANDOM_LOOT_CONTAINER_SIZE: usize = 12;
const LOOT_NODES_FILE: &str = "m182_loot_nodes.txt";
const LOOT_NODE_LIMIT: usize = 1500;

#[derive(Clone, Copy, PartialEq)]
struct LootNode {
    template: [u8; 16],
    x: f32,
    z: f32,
    node: u32,
}

struct LootNodes {
    list: Vec<LootNode>,
    loaded: bool,
    dirty: bool,
}

static LOOT_NODES: Mutex<LootNodes> = Mutex::new(LootNodes { list: Vec::new(), loaded: false, dirty: false });
static ENEMY_TEMPLATES: OnceLock<HashSet<[u8; 16]>> = OnceLock::new();

fn enemy_templates() -> &'static HashSet<[u8; 16]> {
    ENEMY_TEMPLATES.get_or_init(|| {
        let mut out = HashSet::new();
        let Ok(dir) = std::env::current_dir() else { return out };
        let Ok(text) = std::fs::read_to_string(dir.join("export").join("m182_enemies.csv")) else { return out };
        for line in text.lines().skip(1) {
            let mut parts = line.split(',');
            let (Some(_), Some(guid)) = (parts.next(), parts.next()) else { continue };
            if let Some(g) = super::config::parse_guid(guid) {
                out.insert(g);
            }
        }
        out
    })
}

fn parse_loot_nodes(text: &str) -> Vec<LootNode> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(guid), Some(x), Some(z), Some(node)) = (parts.next(), parts.next(), parts.next(), parts.next()) else { continue };
        let (Some(template), Ok(x), Ok(z), Ok(node)) = (super::config::parse_guid(guid), x.parse::<f32>(), z.parse::<f32>(), u32::from_str_radix(node, 16)) else { continue };
        if node != 0 && x.is_finite() && z.is_finite() {
            out.push(LootNode { template, x, z, node });
        }
    }
    out
}

fn format_loot_nodes(list: &[LootNode]) -> String {
    let mut text = String::new();
    for n in list {
        text.push_str(&format!("{} {:.1} {:.1} {:08x}\n", guid_text(&n.template), n.x, n.z, n.node));
    }
    text
}

fn load_loot_nodes(nodes: &mut LootNodes) {
    if nodes.loaded {
        return;
    }
    nodes.loaded = true;
    let Ok(dir) = std::env::current_dir() else { return };
    if let Ok(text) = std::fs::read_to_string(dir.join(LOOT_NODES_FILE)) {
        nodes.list = parse_loot_nodes(&text);
        if !nodes.list.is_empty() {
            log(&format!("loot: {} loot nodes remembered from earlier runs", nodes.list.len()));
        }
    }
}

fn remember_loot_node(sample: LootNode) {
    let Ok(mut nodes) = LOOT_NODES.lock() else { return };
    load_loot_nodes(&mut nodes);
    if let Some(known) = nodes.list.iter_mut().find(|n| n.template == sample.template && n.node == sample.node) {
        if known.x != sample.x || known.z != sample.z {
            known.x = sample.x;
            known.z = sample.z;
        }
        return;
    }
    let first = nodes.list.is_empty();
    nodes.list.push(sample);
    if nodes.list.len() > LOOT_NODE_LIMIT {
        let extra = nodes.list.len() - LOOT_NODE_LIMIT;
        nodes.list.drain(0..extra);
    }
    nodes.dirty = true;
    if first {
        log("loot: first loot node taken from a creature of the world");
    }
}

/// Keeps what the game itself puts on every creature it spawns: the loot node the creature rolls
/// its loot from. Creatures made from the panel adopt one of these nodes, so they drop what a
/// creature of the world would drop in that place.
fn harvest_loot_node(manager: usize, edx: u32, desc: usize) {
    if edx == 0 || edx >= 0x8000_0000 {
        return;
    }
    let Some(head) = read_bytes(desc, 0x30) else { return };
    if head.len() < 0x30 {
        return;
    }
    let mut template = [0u8; 16];
    template.copy_from_slice(&head[4..20]);
    if !enemy_templates().contains(&template) || !is_server(manager) {
        return;
    }
    let array = u64_at(&head, 0x18) as usize;
    let count = (u64_at(&head, 0x20) as usize).min(96);
    let Some(entries) = read_bytes(array, count * 24) else { return };
    let count = entries.len() / 24;
    let Some(loot) = component_data(&entries, count, HASH_RANDOM_LOOT_CONTAINER, RANDOM_LOOT_CONTAINER_SIZE) else { return };
    let Some(transform) = component_data(&entries, count, HASH_CURRENT_TRANSFORM, TRANSFORM_SIZE) else { return };
    let node = u32_at(&loot, 4);
    if node == 0 {
        return;
    }
    remember_loot_node(LootNode {
        template,
        x: (le_i64(&transform, 0) as f64 / METERS) as f32,
        z: (le_i64(&transform, 16) as f64 / METERS) as f32,
        node,
    });
}

/// The loot node a creature made at `x`,`z` should roll from: the closest one used by a creature of
/// the same kind, otherwise the closest of any kind.
fn loot_node_for(template: &[u8; 16], x: f64, z: f64) -> Option<(u32, bool, f64)> {
    let Ok(mut nodes) = LOOT_NODES.lock() else { return None };
    load_loot_nodes(&mut nodes);
    let distance = |n: &LootNode| ((n.x as f64 - x).powi(2) + (n.z as f64 - z).powi(2)).sqrt();
    let best = |same: bool| {
        nodes
            .list
            .iter()
            .filter(|n| (n.template == *template) == same)
            .map(|n| (n.node, distance(n)))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    };
    best(true).map(|(node, d)| (node, true, d)).or_else(|| best(false).map(|(node, d)| (node, false, d)))
}

pub fn save_loot_nodes() {
    let text = {
        let Ok(mut nodes) = LOOT_NODES.lock() else { return };
        if !nodes.dirty {
            return;
        }
        nodes.dirty = false;
        format_loot_nodes(&nodes.list)
    };
    let Ok(dir) = std::env::current_dir() else { return };
    let tmp = dir.join("m182_loot_nodes.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(LOOT_NODES_FILE));
    }
}

pub fn after(manager: usize, edx: u32, desc: usize, ret: u32) {
    if ret == 0 {
        return;
    }
    if LIVE.spawner() {
        loot_container_check(manager, desc, ret);
        harvest_loot_node(manager, edx, desc);
    }
    let settled = PLAYER_ID.load(Ordering::Relaxed) != 0 && calibration_done();
    let server = is_server(manager);
    if settled && edx != 1 && !server {
        return;
    }

    let Some(head) = read_bytes(desc, 0x30) else { return };
    if head.len() < 0x30 {
        return;
    }
    if !server {
        if edx == 1 && head[4..20] == GUID_PLAYER {
            let previous_client = CLIENT_MGR.swap(manager, Ordering::SeqCst);
            if drop_stale_world(manager, previous_client, now_ms(), SERVER_PLAYER_MS.load(Ordering::SeqCst)) && SERVER_MGR.swap(0, Ordering::SeqCst) != 0 {
                PLAYER_ID.store(0, Ordering::SeqCst);
                log("the world this game hosted is gone; now connected to a remote server");
            }
            if previous_client != manager {
                CLIENT_SELF.store(0, Ordering::SeqCst);
            }
            if CLIENT_SELF.load(Ordering::SeqCst) != 0 {
                return;
            }
            CLIENT_PLAYER_ID.store(ret, Ordering::SeqCst);
            CLIENT_TRANSFORM_INDEX.store(-1, Ordering::SeqCst);
            CLIENT_GUESSES.store(0, Ordering::SeqCst);
            log(&format!("client player entity id={ret} mgr={manager:#x}"));
            let array = u64_at(&head, 0x18) as usize;
            let count = (u64_at(&head, 0x20) as usize).min(96);
            if let Some(entries) = read_bytes(array, count * 24) {
                let count = entries.len() / 24;
                if let Some(expected) = component_data(&entries, count, HASH_CURRENT_TRANSFORM, TRANSFORM_SIZE) {
                    if let Some(first) = expected.get(..24) {
                        calibrate(manager, ret, first);
                    }
                }
            }
        }
        return;
    }
    if head[4..20] == GUID_PLAYER && settled && edx != 1 {
        track_player(manager, ret);
        log(&format!("player entity id={ret} mgr={manager:#x} created with kind {edx}"));
        return;
    }
    if head[4..20] == GUID_PLAYER && edx == 1 {
        let current = PLAYER_ID.load(Ordering::SeqCst);
        let previous_manager = SERVER_MGR.load(Ordering::SeqCst);
        let primary = current == 0
            || current == ret
            || previous_manager != manager
            || find_record(manager, current).is_none();
        if primary {
            PLAYER_ID.store(ret, Ordering::SeqCst);
            SERVER_MGR.store(manager, Ordering::SeqCst);
            SERVER_PLAYER_MS.store(now_ms(), Ordering::SeqCst);
        }
        track_player(manager, ret);
        log(&format!("player entity id={ret} mgr={manager:#x} primary={primary}"));
    }
    if settled {
        return;
    }

    let _ = SERVER_MGR.compare_exchange(0, manager, Ordering::SeqCst, Ordering::SeqCst);
    if TRANSFORM_INDEX.load(Ordering::Relaxed) >= 0 || edx >= 0x8000_0000 || SERVER_MGR.load(Ordering::SeqCst) != manager {
        return;
    }
    if CALIBRATION_TRIES.fetch_add(1, Ordering::Relaxed) >= MAX_CALIBRATION_TRIES {
        return;
    }
    let array = u64_at(&head, 0x18) as usize;
    let count = (u64_at(&head, 0x20) as usize).min(96);
    let Some(entries) = read_bytes(array, count * 24) else { return };
    let count = entries.len() / 24;
    if let Some(expected) = component_data(&entries, count, HASH_CURRENT_TRANSFORM, TRANSFORM_SIZE) {
        if let Some(first) = expected.get(..24) {
            calibrate(manager, ret, first);
        }
    }
}

pub fn resolve_template(text: &str) -> Option<[u8; 16]> {
    if let Some(guid) = super::config::parse_guid(text) {
        return Some(guid);
    }
    let path = std::env::current_dir().ok()?.join("export").join("m182_enemies.csv");
    let content = std::fs::read_to_string(path).ok()?;
    let wanted = text.trim();
    for line in content.lines().skip(1) {
        let mut parts = line.split(',');
        let (Some(name), Some(guid)) = (parts.next(), parts.next()) else { continue };
        if name.eq_ignore_ascii_case(wanted) {
            return super::config::parse_guid(guid);
        }
    }
    None
}

#[repr(C, align(16))]
struct Block([u8; 0x100]);

fn write_u32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn write_u64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

/// Chat clients are numbered like the players of the world: client 64 is the first player
/// (entity 1), 65 the second (entity 2). The spawn is placed next to whoever asked for it when
/// that entity is a known player, otherwise next to the primary player.
fn requester_transform(manager: usize) -> Option<Vec<u8>> {
    let sender = REQUEST_SENDER.swap(0, Ordering::SeqCst);
    if sender == 0 {
        return None;
    }
    let client = sender - 1;
    let id = (client & 0x3f) + 1;
    if !tracked_ids().contains(&id) {
        log(&format!("spawn: chat client {client} is not a known player (entity {id}), using the primary player"));
        return None;
    }
    let transform = transform_of(manager, id);
    if transform.is_some() {
        log(&format!("spawn: placed next to chat client {client} (entity {id})"));
    }
    transform
}

fn spawn_now(manager: usize) {
    let tag = SPAWN_TAG.lock().ok().and_then(|mut t| t.take());
    let requested = REQUEST.lock().ok().and_then(|mut r| r.take());
    let settings = match requested {
        Some(s) => s,
        None => match SPAWN.lock() {
            Ok(s) => s.clone(),
            Err(_) => return,
        },
    };
    let Some(template) = resolve_template(&settings.template) else {
        log(&format!("spawn skipped: \"{}\" is not a known template name or id", settings.template));
        return;
    };
    let Some(t) = requester_transform(manager).or_else(player_transform) else {
        log(&format!(
            "spawn skipped: player position not available (player id={}, transform index={}, tries={})",
            PLAYER_ID.load(Ordering::Relaxed),
            TRANSFORM_INDEX.load(Ordering::Relaxed),
            CALIBRATION_TRIES.load(Ordering::Relaxed)
        ));
        return;
    };
    let at = REQUEST_AT.lock().ok().and_then(|mut r| r.take());
    let mut base = (le_i64(&t, 0), le_i64(&t, 8), le_i64(&t, 16));
    if let Some(p) = at {
        base = ((p[0] * METERS) as i64, (p[1] * METERS) as i64, (p[2] * METERS) as i64);
    }
    let give = REQUEST_GIVE.lock().ok().and_then(|mut g| g.take());
    let delivery = give.is_some();
    let mut settings = settings;
    if delivery {
        settings.count = 1;
    }
    let object = REQUEST_OBJECT.swap(false, Ordering::SeqCst) || delivery;
    let forward = REQUEST_FORWARD.swap(false, Ordering::SeqCst) && at.is_none() && !delivery;
    let mut radius = if (at.is_some() || delivery) && settings.count == 1 { 0.0 } else { settings.distance as f64 };
    if forward {
        let (fx, fz) = facing_direction(&t);
        base.0 = base.0.wrapping_add((fx * settings.distance as f64 * METERS) as i64);
        base.2 = base.2.wrapping_add((fz * settings.distance as f64 * METERS) as i64);
        radius = if settings.count == 1 { 0.0 } else { (settings.distance as f64).min(4.0) };
    }
    log(&format!(
        "spawning {} ({}) x{} level {} near x={:.2} y={:.2} z={:.2}",
        settings.template,
        hex(&template),
        settings.count,
        settings.level,
        base.0 as f64 / METERS,
        base.1 as f64 / METERS,
        base.2 as f64 / METERS
    ));

    for k in 0..settings.count {
        let angle = std::f64::consts::TAU * k as f64 / settings.count as f64;
        let dx = (radius * angle.cos() * METERS) as i64;
        let dz = (radius * angle.sin() * METERS) as i64;
        let dy = if delivery { 0 } else { (0.5 * METERS) as i64 };

        let mut transform = [0u8; TRANSFORM_SIZE];
        write_u64(&mut transform, 0, base.0.wrapping_add(dx) as u64);
        write_u64(&mut transform, 8, base.1.wrapping_add(dy) as u64);
        write_u64(&mut transform, 16, base.2.wrapping_add(dz) as u64);
        transform[36..40].copy_from_slice(&1.0f32.to_le_bytes());
        transform[40..44].copy_from_slice(&1.0f32.to_le_bytes());
        transform[44..48].copy_from_slice(&1.0f32.to_le_bytes());
        transform[48..52].copy_from_slice(&1.0f32.to_le_bytes());
        let level = settings.level.to_le_bytes();

        let mut entries = Block([0u8; 0x100]);
        write_u32(&mut entries.0, 0, HASH_CURRENT_TRANSFORM);
        write_u64(&mut entries.0, 8, transform.as_ptr() as u64);
        write_u64(&mut entries.0, 16, TRANSFORM_SIZE as u64);
        if !object {
            write_u32(&mut entries.0, 24, HASH_OVERRIDE_LEVEL);
            write_u64(&mut entries.0, 32, level.as_ptr() as u64);
            write_u64(&mut entries.0, 40, 4);
        }

        let mut loot_node = [0u8; RANDOM_LOOT_CONTAINER_SIZE];
        let mut entry_count = if delivery { 2 } else if object { 1 } else { 2 };
        if !object && !delivery {
            let (px, pz) = ((base.0.wrapping_add(dx)) as f64 / METERS, (base.2.wrapping_add(dz)) as f64 / METERS);
            match loot_node_for(&template, px, pz) {
                Some((node, same_kind, meters)) => {
                    loot_node[4..8].copy_from_slice(&node.to_le_bytes());
                    write_u32(&mut entries.0, 48, HASH_RANDOM_LOOT_CONTAINER);
                    write_u64(&mut entries.0, 56, loot_node.as_ptr() as u64);
                    write_u64(&mut entries.0, 64, RANDOM_LOOT_CONTAINER_SIZE as u64);
                    entry_count = 3;
                    log(&format!(
                        "loot: creature rolls from loot node {node:08x} of a creature of the world ({}, {meters:.0} m away)",
                        if same_kind { "same kind" } else { "other kind" }
                    ));
                }
                None => log("loot: no loot node known yet; it is taken from the first creatures the game spawns, walk around and try again"),
            }
        }

        let mut pickup = [0u8; PICKUP_ITEM_SIZE];
        if let Some(g) = &give {
            pickup[0..4].copy_from_slice(&g.item.to_le_bytes());
            pickup[4..20].copy_from_slice(&g.guid);
            pickup[20..24].copy_from_slice(&g.level.to_le_bytes());
            pickup[24] = g.rarity as u8;
            pickup[26..28].copy_from_slice(&(compensated_amount(g.amount) as u16).to_le_bytes());
            write_u32(&mut entries.0, 24, HASH_PICKUP_ITEM);
            write_u64(&mut entries.0, 32, pickup.as_ptr() as u64);
            write_u64(&mut entries.0, 40, PICKUP_ITEM_SIZE as u64);
        }

        let mut desc = Block([0u8; 0x100]);
        desc.0[4..20].copy_from_slice(&template);
        write_u64(&mut desc.0, 0x18, entries.0.as_ptr() as u64);
        write_u64(&mut desc.0, 0x20, entry_count);

        let id = original_create(manager, 0, desc.0.as_ptr() as usize, 0);
        LAST_SPAWNED.store(id, Ordering::SeqCst);
        log(&format!("create returned entity id={id}"));
        if let Some(g) = give {
            if id != 0 {
                verify_pickup(manager, id, &g);
            }
        }
        if id != 0 && !object {
            start_stats(manager, id, settings.health, settings.damage);
            if let Some(tag) = &tag {
                if let Ok(mut created) = CREATED.lock() {
                    created.push((tag.clone(), id));
                }
            }
        }
    }
}

const AURA_TEMPLATE: &str = "VFX_Resource_Increase_Health_NoSound";
const ENTRY_TEMPLATE: &str = "VFX_Resource_Increase_Health";
const AURA_PERIOD: Duration = Duration::from_secs(3);

static AURA_GUIDS: Mutex<Vec<(&'static str, [u8; 16])>> = Mutex::new(Vec::new());
static AURA_MISSING_LOGGED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

fn aura_guid(wanted: &'static str) -> Option<[u8; 16]> {
    if let Ok(known) = AURA_GUIDS.lock() {
        if let Some((_, guid)) = known.iter().find(|(n, _)| *n == wanted) {
            return Some(*guid);
        }
    }
    let path = std::env::current_dir().ok()?.join("export").join("m182_objects.csv");
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines().skip(1) {
        let mut parts = line.split(',');
        let (Some(name), Some(guid)) = (parts.next(), parts.next()) else { continue };
        if name == wanted {
            let parsed = super::config::parse_guid(guid)?;
            if let Ok(mut known) = AURA_GUIDS.lock() {
                known.push((wanted, parsed));
            }
            log(&format!("zones: effect {wanted} found"));
            return Some(parsed);
        }
    }
    if let Ok(mut logged) = AURA_MISSING_LOGGED.lock() {
        if !logged.contains(&wanted) {
            logged.push(wanted);
            log(&format!("zones: effect {wanted} is not in the object list"));
        }
    }
    None
}

fn spawn_effect(manager: usize, template: [u8; 16], x: i64, y: i64, z: i64) -> u32 {
    let mut transform = [0u8; TRANSFORM_SIZE];
    write_u64(&mut transform, 0, x as u64);
    write_u64(&mut transform, 8, y as u64);
    write_u64(&mut transform, 16, z as u64);
    transform[36..40].copy_from_slice(&1.0f32.to_le_bytes());
    transform[40..44].copy_from_slice(&1.0f32.to_le_bytes());
    transform[44..48].copy_from_slice(&1.0f32.to_le_bytes());
    transform[48..52].copy_from_slice(&1.0f32.to_le_bytes());
    let mut entries = Block([0u8; 0x100]);
    write_u32(&mut entries.0, 0, HASH_CURRENT_TRANSFORM);
    write_u64(&mut entries.0, 8, transform.as_ptr() as u64);
    write_u64(&mut entries.0, 16, TRANSFORM_SIZE as u64);
    let mut desc = Block([0u8; 0x100]);
    desc.0[4..20].copy_from_slice(&template);
    write_u64(&mut desc.0, 0x18, entries.0.as_ptr() as u64);
    write_u64(&mut desc.0, 0x20, 1);
    original_create(manager, 0, desc.0.as_ptr() as usize, 0)
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Powers {
    god: bool,
    stamina: bool,
    fly: bool,
    fly_speed: Option<f32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Command {
    cmd: String,
    label: Option<String>,
    guid: Option<String>,
    item_id: Option<u32>,
    stats: Option<StatsPatch>,
}

#[derive(Clone, Default)]
struct Saved {
    susceptibility: Vec<u8>,
    fall: Option<[u8; 8]>,
    retries: u32,
    seen: bool,
}

struct PowerState {
    powers: Powers,
    mtime: Option<std::time::SystemTime>,
    hover: Option<i64>,
    protect_until: Option<Instant>,
    stamina_peak: u32,
    mana_peak: u32,
    heal_once: bool,
    stamina_peak_c: u32,
    mana_peak_c: u32,
    last_fly: Option<Instant>,
    last_written: Option<(i64, i64, i64)>,
    gravity: [Option<usize>; 2],
    gravity_saved: [u8; 2],
    loco: [Option<usize>; 2],
    net_loco: [Option<usize>; 2],
    loco_overwritten: [u32; 2],
    loco_reported: Option<Instant>,
    gravity_off: bool,
    client_logged: bool,
    groups: GroupState,
    saved: HashMap<(usize, u32), Saved>,
    zone_ids: Vec<u32>,
    diag_left: u32,
    diag_last: Option<Instant>,
    spawners: SpawnerSet,
    buffs_seen: Vec<u32>,
    client_powers: HashMap<u32, ClientPowers>,
    client_scan: Option<Instant>,
}

#[derive(Default)]
struct ClientPowers {
    powers: Powers,
    mtime: Option<std::time::SystemTime>,
    stamina_peak: u32,
    mana_peak: u32,
    seen: Vec<u32>,
    heal: bool,
    absent_since: Option<Instant>,
}

static RES_INDEX: [[AtomicI32; 6]; 2] = [
    [AtomicI32::new(-1), AtomicI32::new(-1), AtomicI32::new(-1), AtomicI32::new(-1), AtomicI32::new(-1), AtomicI32::new(-1)],
    [AtomicI32::new(-1), AtomicI32::new(-1), AtomicI32::new(-1), AtomicI32::new(-1), AtomicI32::new(-1), AtomicI32::new(-1)],
];
static FALL_INDEX: [AtomicI32; 2] = [AtomicI32::new(-1), AtomicI32::new(-1)];
static FALL_TRIES: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

fn kind_of(manager: usize) -> usize {
    let c = CLIENT_MGR.load(Ordering::Relaxed);
    if c != 0 && manager == c { 1 } else { 0 }
}

fn resource(manager: usize, id: u32, which: usize, signature: u32) -> Option<(usize, Vec<u8>)> {
    resource_sized(manager, id, which, signature, 40)
}

fn resource_sized(manager: usize, id: u32, which: usize, signature: u32, size: usize) -> Option<(usize, Vec<u8>)> {
    let record = find_record(manager, id)?;
    let p = placement(record)?;
    let kind = kind_of(manager);
    let cached = RES_INDEX[kind][which].load(Ordering::Relaxed);
    if cached >= 0 {
        if let Some((addr, stride)) = component_address(&p, cached as usize) {
            if stride == size {
                if let Some(b) = read_bytes(addr, size) {
                    if b.len() >= size && u32_at(&b, 0) == signature {
                        return Some((addr, b));
                    }
                }
            }
        }
    }
    for index in 0..COMPONENT_SLOTS {
        let Some((addr, stride)) = component_address(&p, index) else { continue };
        if stride != size {
            continue;
        }
        if let Some(b) = read_bytes(addr, size) {
            if b.len() >= size && u32_at(&b, 0) == signature {
                RES_INDEX[kind][which].store(index as i32, Ordering::Relaxed);
                log(&format!("powers: resource {which} found at component {index} ({})", if kind == 1 { "client" } else { "server" }));
                return Some((addr, b));
            }
        }
    }
    None
}

fn put_u32(addr: usize, value: u32) -> bool {
    write_bytes(addr, &value.to_le_bytes())
}

pub fn refill_health(manager: usize, id: u32) -> bool {
    let Some((addr, b)) = resource(manager, id, 0, HEALTH_SIGNATURE) else { return false };
    let current = u32_at(&b, 8);
    let maximum = u32_at(&b, 36);
    if maximum > 0 && current < maximum {
        return put_u32(addr + 8, maximum);
    }
    true
}

pub fn refill_stamina(manager: usize, id: u32, peak: &mut u32) -> bool {
    let Some((addr, b)) = resource(manager, id, 1, STAMINA_SIGNATURE) else { return false };
    let current = u32_at(&b, 8);
    let base = u32_at(&b, 32);
    let modifier = u32_at(&b, 36) as i32;
    let effective = base as i64 + modifier as i64;
    if base > 0 && base < 1_000_000 && effective > 0 && effective < 1_000_000 {
        let target = effective as u32;
        *peak = target;
        if current < target || u32_at(&b, 16) < target {
            put_u32(addr + 8, target);
            put_u32(addr + 16, target);
        }
        return true;
    }
    if current > *peak && current < 1_000_000 {
        *peak = current;
    }
    let target = (*peak).max(base);
    if target > 0 && (current < target || u32_at(&b, 16) < target) {
        put_u32(addr + 8, target);
        put_u32(addr + 16, target);
    }
    true
}

static BUFF_FOUND: AtomicBool = AtomicBool::new(false);
const BUFF_CONTAINER_SIZE: usize = 472;
const BUFF_STRIDE: usize = 12;
const DEBUFF_SLOTS: [(usize, usize); 2] = [(240, 16), (444, 1)];
const BUFF_SLOTS: [(usize, usize); 5] = [(0, 4), (48, 16), (240, 16), (432, 1), (444, 1)];

fn buff_container_plausible(b: &[u8]) -> bool {
    if b.len() != BUFF_CONTAINER_SIZE {
        return false;
    }
    for (start, count) in BUFF_SLOTS {
        for i in 0..count {
            let at = start + i * BUFF_STRIDE;
            let entity = u32_at(b, at);
            let kind = u32_at(b, at + 4);
            if b[at + 8] > 1 || b[at + 9..at + 12].iter().any(|v| *v != 0) {
                return false;
            }
            if (entity == 0) != (kind == 0) {
                return false;
            }
        }
    }
    true
}

pub fn cleanse_debuffs(manager: usize, id: u32, seen: &mut Vec<u32>) -> usize {
    let Some((_, _, slots)) = entity_layout(manager, id) else { return 0 };
    let mut cleansed = 0;
    for slot in &slots {
        if slot.stride != BUFF_CONTAINER_SIZE {
            continue;
        }
        let Some(b) = read_bytes(slot.addr, BUFF_CONTAINER_SIZE) else { continue };
        if !buff_container_plausible(&b) {
            continue;
        }
        if !BUFF_FOUND.swap(true, Ordering::Relaxed) {
            log(&format!("states: buff container found (entity {id})"));
        }
        for (start, count) in DEBUFF_SLOTS {
            for i in 0..count {
                let at = start + i * BUFF_STRIDE;
                let kind = u32_at(&b, at + 4);
                if kind == 0 || b[at + 8] != 0 {
                    continue;
                }
                if write_bytes(slot.addr + at + 8, &[1]) {
                    cleansed += 1;
                    if seen.len() < 40 && !seen.contains(&kind) {
                        seen.push(kind);
                        log(&format!("states: debuff type {kind} (entity {}) cleansed at slot offset {at}", u32_at(&b, at)));
                    }
                }
            }
        }
        return cleansed;
    }
    cleansed
}

pub fn refill_mana(manager: usize, id: u32, peak: &mut u32) -> bool {
    let Some((addr, b)) = resource(manager, id, 2, MANA_SIGNATURE) else { return false };
    let current = u32_at(&b, 8);
    let limit = u32_at(&b, 32);
    if current > *peak && current < 1_000_000 {
        *peak = current;
    }
    let target = (*peak).max(limit);
    if target > 0 && current < target {
        put_u32(addr + 8, target);
        put_u32(addr + 12, target);
    }
    true
}

fn find_fall(manager: usize, id: u32) -> Option<usize> {
    let kind = kind_of(manager);
    let record = find_record(manager, id)?;
    let p = placement(record)?;
    let cached = FALL_INDEX[kind].load(Ordering::Relaxed);
    let matches = |addr: usize| -> bool {
        matches!(read_bytes(addr, FALL_DAMAGE_SIZE), Some(b) if b.len() >= FALL_DAMAGE_SIZE && b[8..24] == FALL_SEQUENCE)
    };
    if cached >= 0 {
        if let Some((addr, stride)) = component_address(&p, cached as usize) {
            if stride == FALL_DAMAGE_SIZE && matches(addr) {
                return Some(addr);
            }
        }
    }
    if FALL_TRIES[kind].load(Ordering::Relaxed) >= 6 {
        return None;
    }
    FALL_TRIES[kind].fetch_add(1, Ordering::Relaxed);
    for index in 0..COMPONENT_SLOTS {
        let Some((addr, stride)) = component_address(&p, index) else { continue };
        if stride == FALL_DAMAGE_SIZE && matches(addr) {
            FALL_INDEX[kind].store(index as i32, Ordering::Relaxed);
            log(&format!("shield: fall damage found at component {index} ({})", if kind == 1 { "client" } else { "server" }));
            return Some(addr);
        }
    }
    None
}

static LAST_GOOD_SUSCEPTIBILITY: Mutex<Vec<u8>> = Mutex::new(Vec::new());
const SHIELD_RETRIES: u32 = 600;

/// Zeroes the damage susceptibility (and lifts the fall damage limit) of one entity.
/// The original values are stored exactly once, the first time they are seen, and are never
/// overwritten afterwards: whatever the game writes in between must not become the "original".
fn shield_on(st: &mut PowerState, manager: usize, id: u32) {
    let key = (manager, id);
    if let Some((addr, b)) = resource_sized(manager, id, 3, SUSCEPTIBILITY_SIGNATURE, SUSCEPTIBILITY_SIZE) {
        let data = &b[8..SUSCEPTIBILITY_SIZE];
        let zero = data.iter().all(|v| *v == 0);
        let known = st.saved.get(&key).map(|e| e.seen).unwrap_or(false);
        if !known {
            if !zero {
                if let Ok(mut last) = LAST_GOOD_SUSCEPTIBILITY.lock() {
                    *last = data.to_vec();
                }
                log(&format!("shield: entity {id} original susceptibility {}", hex(data)));
                let entry = st.saved.entry(key).or_default();
                entry.susceptibility = data.to_vec();
                entry.seen = true;
            } else {
                // already zero and never seen otherwise: borrow the last healthy values for the way back
                let guess = LAST_GOOD_SUSCEPTIBILITY.lock().map(|l| l.clone()).unwrap_or_default();
                log(&format!("shield: entity {id} susceptibility was already zero (fallback {} bytes)", guess.len()));
                let entry = st.saved.entry(key).or_default();
                entry.susceptibility = guess;
                entry.seen = true;
            }
        }
        if !zero {
            write_bytes(addr + 8, &[0u8; SUSCEPTIBILITY_SIZE - 8]);
        }
        st.saved.entry(key).or_default();
    }
    if let Some(addr) = find_fall(manager, id) {
        if let Some(b) = read_bytes(addr, 8) {
            if b.len() >= 8 {
                let start = f32_at(&b, 0);
                let entry = st.saved.entry(key).or_default();
                if start < FALL_UNLIMITED * 0.5 {
                    if entry.fall.is_none() {
                        entry.fall = Some([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
                    }
                    let mut huge = [0u8; 8];
                    huge[0..4].copy_from_slice(&FALL_UNLIMITED.to_le_bytes());
                    huge[4..8].copy_from_slice(&FALL_UNLIMITED.to_le_bytes());
                    write_bytes(addr, &huge);
                }
            }
        }
    }
}

/// Gives the saved originals back, value by value, and checks the result. The entry is only dropped
/// once everything is verified (or after many tries), so a failed write is retried on the next tick.
fn shield_off(st: &mut PowerState, manager: usize, id: u32) {
    let key = (manager, id);
    let Some(saved) = st.saved.get(&key).cloned() else { return };
    let mut done = true;
    if !saved.susceptibility.is_empty() {
        match resource_sized(manager, id, 3, SUSCEPTIBILITY_SIGNATURE, SUSCEPTIBILITY_SIZE) {
            Some((addr, b)) => {
                if b[8..SUSCEPTIBILITY_SIZE] != saved.susceptibility[..] {
                    write_bytes(addr + 8, &saved.susceptibility);
                    let ok = read_bytes(addr + 8, saved.susceptibility.len()).map(|r| r == saved.susceptibility).unwrap_or(false);
                    done &= ok;
                }
            }
            None => done = false,
        }
    }
    if let Some(original) = saved.fall {
        match find_fall(manager, id) {
            Some(addr) => {
                if let Some(b) = read_bytes(addr, 8) {
                    if b.len() >= 8 && b[..8] != original {
                        write_bytes(addr, &original);
                        done &= read_bytes(addr, 8).map(|r| r[..8] == original).unwrap_or(false);
                    }
                }
            }
            None => done = false,
        }
    }
    if done {
        st.saved.remove(&key);
        log(&format!("shield: entity {id} restored"));
    } else if let Some(entry) = st.saved.get_mut(&key) {
        entry.retries += 1;
        if entry.retries >= SHIELD_RETRIES {
            st.saved.remove(&key);
            log(&format!("shield: entity {id} could not be restored, giving up"));
        }
    }
}

fn shield_tick(st: &mut PowerState, want: &[(usize, u32)]) {
    for &(manager, id) in want {
        shield_on(st, manager, id);
        refill_health(manager, id);
    }
    let stale: Vec<(usize, u32)> = st.saved.keys().filter(|k| !want.contains(k)).cloned().collect();
    for (manager, id) in stale {
        shield_off(st, manager, id);
    }
}

fn transform_slots(manager: usize, id: u32, old: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let Some((_, p, slots)) = entity_layout(manager, id) else { return out };
    let _ = p;
    for slot in &slots {
        if slot.stride >= TRANSFORM_SIZE && slot.stride <= 128 {
            if let Some(b) = read_bytes(slot.addr, 24) {
                if b.len() >= 24 && b[..24] == old[..24] {
                    out.push(slot.addr);
                }
            }
        }
    }
    out
}

pub fn set_player_position(manager: usize, id: u32, pos: [Option<i64>; 3]) -> bool {
    if manager == 0 || id == 0 {
        return false;
    }
    let index = transform_cell(manager).load(Ordering::SeqCst);
    if index < 0 {
        return false;
    }
    let Some(record) = find_record(manager, id) else { return false };
    let Some(p) = placement(record) else { return false };
    let Some((main, stride)) = component_address(&p, index as usize) else { return false };
    if stride != TRANSFORM_SIZE {
        return false;
    }
    let Some(old) = read_bytes(main, TRANSFORM_SIZE) else { return false };
    if old.len() < TRANSFORM_SIZE {
        return false;
    }
    let addrs = transform_slots(manager, id, &old);
    let mut ok = false;
    for addr in addrs {
        for (axis, value) in pos.iter().enumerate() {
            if let Some(v) = value {
                ok |= write_bytes(addr + axis * 8, &v.to_le_bytes());
            }
        }
    }
    ok
}

fn metres(v: f64) -> i64 {
    (v * METERS) as i64
}

fn refresh_powers(st: &mut PowerState, dir: &std::path::Path) {
    let path = dir.join(POWERS_FILE);
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    if mtime == st.mtime {
        return;
    }
    st.mtime = mtime;
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    match serde_json::from_str::<Powers>(&text) {
        Ok(p) => {
            if st.powers.fly && !p.fly {
                st.protect_until = Some(Instant::now() + Duration::from_secs(8));
                st.hover = None;
            }
            log(&format!("powers: god={} stamina={} fly={}", p.god, p.stamina, p.fly));
            st.powers = p;
        }
        Err(e) => log(&format!("powers: invalid file ({e})")),
    }
}

fn run_command(st: &mut PowerState, dir: &std::path::Path) {
    let path = dir.join(COMMAND_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let _ = std::fs::remove_file(&path);
    let Ok(cmd) = serde_json::from_str::<Command>(&text) else {
        log("command ignored: invalid file");
        return;
    };
    let name = cmd.cmd.clone();
    let requester = super::chat::REQUESTER.swap(0, Ordering::SeqCst);
    let via_chat = requester != 0;
    match name.as_str() {
        "heal" => {
            if via_chat {
                let target = ((requester - 1) & 0x3f) + 1;
                st.client_powers.entry(requester - 1).or_default().heal = true;
                log(&format!("command: heal for entity {target}"));
            } else {
                st.heal_once = true;
                log("command: heal");
            }
        }
        "item_stats" => {
            if let Err(e) = std::thread::Builder::new().name("m182-item-stats".into()).spawn(move || run_item_stats(cmd)) {
                log(&format!("item stats: could not start worker: {e}"));
            }
        }
        other => log(&format!("command ignored: unknown command \"{other}\"")),
    }
}

struct Tracked {
    id: u32,
    number: u32,
    misses: u8,
}

static LISTED: Mutex<String> = Mutex::new(String::new());

static TRACKED: Mutex<Vec<Tracked>> = Mutex::new(Vec::new());
static TRACKED_MGR: AtomicUsize = AtomicUsize::new(0);
static NEXT_NUMBER: AtomicU32 = AtomicU32::new(1);
const PLAYER_SLOTS: u32 = 17;

fn track_player(manager: usize, id: u32) {
    let Ok(mut list) = TRACKED.lock() else { return };
    if TRACKED_MGR.swap(manager, Ordering::SeqCst) != manager {
        list.clear();
        NEXT_NUMBER.store(1, Ordering::SeqCst);
    }
    if list.iter().any(|p| p.id == id) {
        return;
    }
    let number = if id <= PLAYER_SLOTS { id } else { NEXT_NUMBER.fetch_add(1, Ordering::SeqCst).max(PLAYER_SLOTS + 1) };
    list.push(Tracked { id, number, misses: 0 });
    log(&format!("players: entity {id} is player {number}"));
}

struct GroupState {
    zones: Vec<groups::Zone>,
    teams: Vec<groups::TeamDef>,
    zones_mtime: Option<std::time::SystemTime>,
    teams_mtime: Option<std::time::SystemTime>,
    members_mtime: Option<std::time::SystemTime>,
    last_fast: Option<Instant>,
    last_slow: Option<Instant>,
    aura_last: Option<Instant>,
}

impl GroupState {
    fn new() -> Self {
        Self { zones: Vec::new(), teams: Vec::new(), zones_mtime: None, teams_mtime: None, members_mtime: None, last_fast: None, last_slow: None, aura_last: None }
    }
}

const FACTION_COMPONENT: usize = 205;

static LENT_FACTIONS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn is_player_faction(value: u8) -> bool {
    matches!(value, 7 | 8 | 11)
}

fn faction_addresses(manager: usize, id: u32) -> Vec<usize> {
    let mut genuine = Vec::new();
    let mut guessed = Vec::new();
    let Some((_, _, slots)) = entity_layout(manager, id) else { return genuine };
    let lent = LENT_FACTIONS.lock().map(|l| l.clone()).unwrap_or_default();
    for slot in &slots {
        if slot.stride != 1 {
            continue;
        }
        let Some(b) = read_bytes(slot.addr, 1) else { continue };
        let Some(&value) = b.first() else { continue };
        if slot.index == FACTION_COMPONENT && is_player_faction(value) {
            genuine.push(slot.addr);
        } else if value == super::faction::PLAYER || (lent.contains(&slot.addr) && is_player_faction(value)) {
            guessed.push(slot.addr);
        }
    }
    if genuine.is_empty() { guessed } else { genuine }
}

fn lend_faction(addr: usize, value: u8) -> bool {
    let current = read_bytes(addr, 1).and_then(|b| b.first().copied());
    match current {
        Some(c) if c == value => true,
        Some(c) if is_player_faction(c) => {
            if !write_bytes(addr, &[value]) {
                return false;
            }
            log(&format!("teams: faction at {addr:#x} {c} -> {value}"));
            if let Ok(mut l) = LENT_FACTIONS.lock() {
                if value == super::faction::PLAYER {
                    l.retain(|a| *a != addr);
                } else if !l.contains(&addr) {
                    l.push(addr);
                }
            }
            true
        }
        _ => false,
    }
}

fn team_factions(present: &[usize], pvp: bool) -> Vec<u8> {
    let mut by_team: Vec<u8> = vec![super::faction::PLAYER; present.len()];
    if !pvp || super::faction::friendly_fire_setting() {
        return by_team;
    }
    let mut next = 0usize;
    for (index, present) in present.iter().enumerate() {
        if *present >= 2 && next < super::faction::TEAM_FACTIONS.len() {
            by_team[index] = super::faction::TEAM_FACTIONS[next];
            next += 1;
        }
    }
    by_team
}

/// Factions for the teams inside always-PvP zones while PvP is off elsewhere: the first team with
/// two or more people inside is protected from its own members; the faction left for outsiders
/// (`PEACE`) is never lent to a team.
fn zone_team_factions(present: &[usize]) -> Vec<u8> {
    let mut by_team: Vec<u8> = vec![super::faction::PLAYER; present.len()];
    if super::faction::friendly_fire_setting() {
        return by_team;
    }
    if let Some(index) = present.iter().position(|n| *n >= 2) {
        by_team[index] = super::faction::TEAM_FACTIONS[1];
    }
    by_team
}

static PVP_ZONE_STATE: Mutex<String> = Mutex::new(String::new());

static NO_FACTION: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn note_faction(id: u32, missing: bool) {
    let Ok(mut noted) = NO_FACTION.lock() else { return };
    let was = noted.contains(&id);
    if missing && !was {
        noted.push(id);
        log(&format!("teams: entity {id} is in a team but its faction cannot be found, so teammates may still hurt it"));
    } else if !missing && was {
        noted.retain(|i| *i != id);
        log(&format!("teams: the faction of entity {id} is found now"));
    }
}

fn groups_tick(st: &mut PowerState, dir: &std::path::Path) {
    let now = Instant::now();
    if st.groups.last_fast.map(|t| now.duration_since(t) < Duration::from_millis(200)).unwrap_or(false) {
        return;
    }
    st.groups.last_fast = Some(now);

    let zones_path = dir.join(groups::ZONES_FILE);
    let zones_mtime = std::fs::metadata(&zones_path).and_then(|m| m.modified()).ok();
    if zones_mtime != st.groups.zones_mtime {
        st.groups.zones_mtime = zones_mtime;
        st.groups.zones = std::fs::read_to_string(&zones_path).map(|t| groups::parse_zones(&t)).unwrap_or_default();
        log(&format!(
            "zones: {} active ({} safe, {} owned bases)",
            st.groups.zones.len(),
            st.groups.zones.iter().filter(|z| z.is_safe()).count(),
            st.groups.zones.iter().filter(|z| z.locks()).count()
        ));
    }
    let teams_path = dir.join(groups::TEAMS_FILE);
    let teams_mtime = std::fs::metadata(&teams_path).and_then(|m| m.modified()).ok();
    let members_path = dir.join(groups::MEMBERS_FILE);
    let members_mtime = std::fs::metadata(&members_path).and_then(|m| m.modified()).ok();
    if teams_mtime != st.groups.teams_mtime || members_mtime != st.groups.members_mtime {
        st.groups.teams_mtime = teams_mtime;
        st.groups.members_mtime = members_mtime;
        st.groups.teams = std::fs::read_to_string(&teams_path).map(|t| groups::parse_team_defs(&t)).unwrap_or_default();
        if let Ok(text) = std::fs::read_to_string(&members_path) {
            groups::apply_members(&mut st.groups.teams, &text);
        }
        log(&format!("teams: {} defined {:?}", st.groups.teams.len(), st.groups.teams));
    }
    publish_live_teams(dir, &st.groups.teams);

    let manager = SERVER_MGR.load(Ordering::SeqCst);
    if manager == 0 {
        st.zone_ids.clear();
        return;
    }
    let players: Vec<(u32, u32)> = match TRACKED.lock() {
        Ok(list) => list.iter().map(|p| (p.id, p.number)).collect(),
        Err(_) => return,
    };
    let me = PLAYER_ID.load(Ordering::SeqCst);
    let names = super::table::names();
    let table = super::table::entries();

    struct Seen {
        id: u32,
        number: u32,
        name: Option<String>,
        team: u8,
    }
    let seen: Vec<Seen> = players
        .iter()
        .map(|(id, number)| {
            let name = names.get(id).cloned();
            let platform_id = table.iter().find(|e| e.entity() == *id).and_then(|e| e.id.clone());
            let team = groups::team_for_player(&st.groups.teams, *number, name.as_deref(), *id, platform_id.as_deref());
            Seen { id: *id, number: *number, name, team }
        })
        .collect();
    let pvp_zones: Vec<groups::Zone> = st.groups.zones.iter().filter(|z| z.is_pvp()).cloned().collect();
    let zone_mode = !LIVE.pvp() && !pvp_zones.is_empty();
    let in_pvp: Vec<bool> = seen
        .iter()
        .map(|p| {
            zone_mode
                && transform_of(manager, p.id)
                    .map(|t| groups::inside(&pvp_zones, le_i64(&t, 0) as f64 / METERS, le_i64(&t, 16) as f64 / METERS))
                    .unwrap_or(false)
        })
        .collect();
    let mut present = vec![0usize; st.groups.teams.len()];
    for (index, p) in seen.iter().enumerate() {
        if p.team != 0 && (!zone_mode || in_pvp[index]) {
            if let Some(c) = present.get_mut(p.team as usize - 1) {
                *c += 1;
            }
        }
    }
    let lent = if zone_mode { zone_team_factions(&present) } else { team_factions(&present, LIVE.pvp()) };
    super::faction::set_zone_pvp(zone_mode);
    {
        let inside: Vec<u32> = seen.iter().enumerate().filter(|(i, _)| in_pvp[*i]).map(|(_, p)| p.id).collect();
        let state = format!("pvp zones={} zone_mode={} inside={:?}", pvp_zones.len(), zone_mode, inside);
        if let Ok(mut last) = PVP_ZONE_STATE.lock() {
            if *last != state {
                log(&format!("zones: {state}"));
                super::diag::line(&format!("zones: {state}"));
                *last = state;
            }
        }
    }
    let mut mask = 0u32;
    let mut rows = Vec::new();
    let mut map: Vec<(usize, u8)> = Vec::new();
    let mut shielded: Vec<usize> = Vec::new();
    let mut dead: Vec<u32> = Vec::new();
    let mut inside_ids: Vec<u32> = Vec::new();
    let mut inside_at: Vec<(u32, i64, i64, i64)> = Vec::new();
    let mut visitors: Vec<usize> = Vec::new();
    for (seen_index, p) in seen.iter().enumerate() {
        let (id, number, team) = (p.id, p.number, p.team);
        match transform_of(manager, id) {
            Some(t) => {
                let (x, y, z) = (le_i64(&t, 0) as f64 / METERS, le_i64(&t, 8) as f64 / METERS, le_i64(&t, 16) as f64 / METERS);
                rows.push(format!(
                    "{{\"n\":{},\"x\":{:.1},\"y\":{:.1},\"z\":{:.1},\"me\":{},\"entity\":{},\"name\":{}}}",
                    number,
                    x,
                    y,
                    z,
                    id == me,
                    id,
                    serde_json::Value::String(p.name.clone().unwrap_or_default())
                ));
                let in_safe = st.groups.zones.iter().any(|zone| zone.is_safe() && groups::inside(std::slice::from_ref(zone), x, z));
                if in_safe {
                    inside_ids.push(id);
                    inside_at.push((id, le_i64(&t, 0), le_i64(&t, 8), le_i64(&t, 16)));
                    shielded.extend(faction_addresses(manager, id));
                }
                if let Some(zone) = groups::ruling_zone(&st.groups.zones, x, z) {
                    if team != zone.owner() as u8 {
                        let entry = table.iter().find(|e| e.entity() == id);
                        let admin = entry.map(|e| roster::is_admin(e.permissions)).unwrap_or(false);
                        if !admin || zone.lock_admins.unwrap_or(false) {
                            if let Some(e) = entry {
                                visitors.push(e.index);
                            }
                        }
                    }
                }
                let team_faction = if team != 0 { lent.get(team as usize - 1).copied().unwrap_or(super::faction::PLAYER) } else { super::faction::PLAYER };
                let wanted = if zone_mode && !in_pvp[seen_index] { super::faction::PEACE } else { team_faction };
                let addresses = faction_addresses(manager, id);
                note_faction(id, team != 0 && addresses.is_empty());
                for addr in addresses {
                    if team != 0 {
                        map.push((addr, team));
                    }
                    if lend_faction(addr, wanted) {
                        mask |= super::faction::mask_bit(wanted, zone_mode);
                    } else {
                        log(&format!("teams: could not set the faction of entity {id}"));
                    }
                }
            }
            None => {
                if find_record(manager, id).is_none() {
                    dead.push(id);
                }
            }
        }
    }
    let listed = format!(
        "{:?} me={} teams={:?} visitors={:?}",
        seen.iter().map(|p| (p.number, p.id, p.name.clone().unwrap_or_default(), p.team)).collect::<Vec<_>>(),
        me,
        map.len(),
        visitors
    );
    if let Ok(mut last) = LISTED.lock() {
        if *last != listed {
            log(&format!("players: listed (number, entity, name, team) {listed}"));
            *last = listed;
        }
    }
    let entered: Vec<u32> = inside_ids.iter().copied().filter(|id| !st.zone_ids.contains(id)).collect();
    if st.zone_ids != inside_ids {
        log(&format!("zones: {} player(s) inside a safe zone", inside_ids.len()));
        st.zone_ids = inside_ids;
    }
    groups::set_team_map(map);
    super::faction::set_team_mask(mask);
    groups::set_protected(shielded);
    super::table::apply_visitors(&visitors);
    if !inside_at.is_empty()
        && AURA_FAILED.load(Ordering::Relaxed) < 3
        && (!entered.is_empty() || st.groups.aura_last.map(|t| now.duration_since(t) >= AURA_PERIOD).unwrap_or(true))
    {
        st.groups.aura_last = Some(now);
        if let Ok(mut q) = AURA_QUEUE.lock() {
            q.clear();
            q.extend(inside_at.into_iter().take(8).map(|(id, x, y, z)| (x, y.saturating_add(metres(0.3)), z, entered.contains(&id))));
            AURA_PENDING.store(1, Ordering::SeqCst);
        }
    }

    if st.groups.last_slow.map(|t| now.duration_since(t) < Duration::from_millis(1000)).unwrap_or(false) {
        return;
    }
    st.groups.last_slow = Some(now);
    let _ = std::fs::write(dir.join(groups::PLAYERS_FILE), format!("[{}]", rows.join(",")));
    if let Ok(mut list) = TRACKED.lock() {
        for p in list.iter_mut() {
            if dead.contains(&p.id) {
                p.misses = p.misses.saturating_add(1);
            } else {
                p.misses = 0;
            }
        }
        list.retain(|p| p.misses < 5);
    }
}

static LIVE_TEAMS: Mutex<Vec<groups::LiveTeam>> = Mutex::new(Vec::new());
static LIVE_TEXT: Mutex<String> = Mutex::new(String::new());

/// Keeps the server's real teams ready for the chat answer and in a file the panel can read here.
fn publish_live_teams(dir: &std::path::Path, teams: &[groups::TeamDef]) {
    let live = groups::live_teams(teams);
    let text = groups::live_json(&live, true);
    if let Ok(mut last) = LIVE_TEXT.lock() {
        if *last != text {
            let tmp = dir.join("m182_teams_live.json.tmp");
            if std::fs::write(&tmp, &text).is_ok() && std::fs::rename(&tmp, dir.join(groups::LIVE_FILE)).is_ok() {
                *last = text;
            }
        }
    }
    if let Ok(mut shared) = LIVE_TEAMS.lock() {
        *shared = live;
    }
}

pub fn teams_line(seq: u32) -> String {
    let live = LIVE_TEAMS.lock().map(|l| l.clone()).unwrap_or_default();
    groups::live_line(seq, &live)
}

pub fn roster_line(seq: u32, client: u32) -> String {
    let table = super::table::entries();
    let mut ids: Vec<u32> = tracked_ids();
    for e in &table {
        if !ids.contains(&e.entity()) {
            ids.push(e.entity());
        }
    }
    ids.sort_unstable();
    let entries: Vec<(u32, String)> = ids
        .into_iter()
        .map(|id| {
            let entry = table.iter().find(|e| e.entity() == id);
            let name = entry.and_then(|e| e.name.clone()).unwrap_or_else(|| format!("ID {id}"));
            (id, roster::label_with_id(&name, entry.and_then(|e| e.id.as_deref())))
        })
        .collect();
    // Chat client 64 is entity 1, 65 entity 2, ...: the asking player is known for certain.
    let asking = (client & 0x3f) + 1;
    let own = transform_of(SERVER_MGR.load(Ordering::SeqCst), asking).map(|t| {
        (asking, [le_i64(&t, 0) as f64 / METERS, le_i64(&t, 8) as f64 / METERS, le_i64(&t, 16) as f64 / METERS])
    });
    super::roster::reply_text_with_own(seq, &entries, own)
}

fn record_trail(pos: (i64, i64, i64)) {
    static LAST: Mutex<Option<(f64, f64)>> = Mutex::new(None);
    let x = pos.0 as f64 / METERS;
    let z = pos.2 as f64 / METERS;
    let Ok(mut last) = LAST.lock() else { return };
    if let Some((lx, lz)) = *last {
        if ((x - lx).powi(2) + (z - lz).powi(2)).sqrt() < 6.0 {
            return;
        }
    }
    let Ok(dir) = std::env::current_dir() else { return };
    let path = dir.join(TRAIL_FILE);
    if std::fs::metadata(&path).map(|m| m.len() > 8_000_000).unwrap_or(false) {
        return;
    }
    let fresh = !path.exists();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        if fresh {
            let _ = writeln!(f, "x,y,z");
        }
        let _ = writeln!(f, "{:.0},{:.0},{:.0}", x, pos.1 as f64 / METERS, z);
        *last = Some((x, z));
    }
}

fn key_down(vk: i32) -> bool {
    let state = unsafe { GetAsyncKeyState(vk) };
    state as u16 & 0x8000 != 0
}

fn client_pair() -> Option<(usize, u32)> {
    if SERVER_MGR.load(Ordering::SeqCst) == 0 || LIVE.force_client() {
        return None;
    }
    let m = CLIENT_MGR.load(Ordering::SeqCst);
    let id = CLIENT_PLAYER_ID.load(Ordering::SeqCst);
    if m == 0 || id == 0 || m == SERVER_MGR.load(Ordering::SeqCst) {
        None
    } else {
        Some((m, id))
    }
}

fn find_gravity(manager: usize, id: u32) -> Option<usize> {
    let (_, _, slots) = entity_layout(manager, id)?;
    for slot in &slots {
        if slot.stride != 20 {
            continue;
        }
        let Some(b) = read_bytes(slot.addr, 20) else { continue };
        if b.len() < 20 {
            continue;
        }
        let (dx, dy, dz, value) = (f32_at(&b, 0), f32_at(&b, 4), f32_at(&b, 8), f32_at(&b, 12));
        if dy < -0.5 && dx.abs() < 0.3 && dz.abs() < 0.3 && value > 0.5 && value < 400.0 && b[16] <= 1 {
            return Some(slot.addr);
        }
    }
    None
}

const LOCO_SIZE: usize = 456;
const LOCO_STATE: usize = 89;
const LOCO_PREVIOUS: usize = 90;
const NET_LOCO_SIZE: usize = 64;
const NET_LOCO_STATE: usize = 36;
const STATE_FLYING: u8 = 3;
const STATE_COUNT: u8 = 12;

fn unit_or_zero(b: &[u8], off: usize) -> bool {
    let (x, y, z) = (f32_at(b, off), f32_at(b, off + 4), f32_at(b, off + 8));
    let len = (x * x + y * y + z * z).sqrt();
    len.is_finite() && (len < 0.001 || (0.9..1.1).contains(&len))
}

fn find_locomotion(manager: usize, id: u32) -> (Option<usize>, Option<usize>) {
    let Some((_, _, slots)) = entity_layout(manager, id) else { return (None, None) };
    let (mut loco, mut net) = (None, None);
    for slot in &slots {
        if slot.stride == LOCO_SIZE && loco.is_none() {
            if let Some(b) = read_bytes(slot.addr, LOCO_SIZE) {
                if b.len() == LOCO_SIZE
                    && b[LOCO_STATE] < STATE_COUNT
                    && b[LOCO_PREVIOUS] < STATE_COUNT
                    && b[225] <= 1
                    && unit_or_zero(&b, 12)
                {
                    loco = Some(slot.addr);
                }
            }
        } else if slot.stride == NET_LOCO_SIZE && net.is_none() {
            if let Some(b) = read_bytes(slot.addr, NET_LOCO_SIZE) {
                if b.len() == NET_LOCO_SIZE
                    && b[NET_LOCO_STATE] < STATE_COUNT
                    && unit_or_zero(&b, 0)
                    && unit_or_zero(&b, 12)
                    && unit_or_zero(&b, 24)
                    && (0.9..1.1).contains(&{
                        let (x, y, z) = (f32_at(&b, 12), f32_at(&b, 16), f32_at(&b, 20));
                        (x * x + y * y + z * z).sqrt()
                    })
                {
                    net = Some(slot.addr);
                }
            }
        }
    }
    (loco, net)
}

fn hold_flying_state(st: &mut PowerState) {
    let mut pairs: Vec<(usize, usize, u32)> = Vec::new();
    if let Some((m, id)) = world_pair() {
        pairs.push((0, m, id));
    }
    if let Some((m, id)) = client_pair() {
        pairs.push((1, m, id));
    }
    for (kind, manager, id) in pairs {
        if manager == 0 || id == 0 {
            continue;
        }
        if st.loco[kind].is_none() && st.net_loco[kind].is_none() {
            let (loco, net) = find_locomotion(manager, id);
            st.loco[kind] = loco;
            st.net_loco[kind] = net;
            if loco.is_some() || net.is_some() {
                log(&format!(
                    "fly: locomotion found ({}) state={} network={}",
                    if kind == 0 { "server" } else { "client" },
                    loco.is_some(),
                    net.is_some()
                ));
            }
        }
        if let Some(addr) = st.loco[kind] {
            if let Some(b) = read_bytes(addr + LOCO_STATE, 2) {
                if b.first() != Some(&STATE_FLYING) {
                    st.loco_overwritten[kind] += 1;
                    write_bytes(addr + LOCO_STATE, &[STATE_FLYING]);
                }
                if b.get(1) != Some(&STATE_FLYING) {
                    write_bytes(addr + LOCO_PREVIOUS, &[STATE_FLYING]);
                }
            }
        }
        if let Some(addr) = st.net_loco[kind] {
            if read_bytes(addr + NET_LOCO_STATE, 1).and_then(|b| b.first().copied()) != Some(STATE_FLYING) {
                write_bytes(addr + NET_LOCO_STATE, &[STATE_FLYING]);
            }
        }
    }
    let now = Instant::now();
    if st.loco_reported.map(|t| now.duration_since(t) >= Duration::from_secs(10)).unwrap_or(true) {
        if st.loco_reported.is_some() {
            log(&format!(
                "fly: game overwrote the locomotion state server={} client={} times in the last 10 s",
                st.loco_overwritten[0], st.loco_overwritten[1]
            ));
        }
        st.loco_reported = Some(now);
        st.loco_overwritten = [0, 0];
        st.loco = [None, None];
        st.net_loco = [None, None];
    }
}

fn set_gravity(st: &mut PowerState, active: bool) {
    let mut pairs: Vec<(usize, usize, u32)> = Vec::new();
    if let Some((m, id)) = world_pair() {
        pairs.push((0, m, id));
    }
    if let Some((m, id)) = client_pair() {
        pairs.push((1, m, id));
    }
    for (kind, manager, id) in pairs {
        if manager == 0 || id == 0 {
            continue;
        }
        if st.gravity[kind].is_none() {
            st.gravity[kind] = find_gravity(manager, id);
            if st.gravity[kind].is_none() {
                continue;
            }
        }
        let Some(addr) = st.gravity[kind] else { continue };
        if active {
            write_bytes(addr + 16, &[st.gravity_saved[kind]]);
        } else {
            if let Some(b) = read_bytes(addr + 16, 1) {
                if b.first() == Some(&1) {
                    st.gravity_saved[kind] = 1;
                }
            }
            write_bytes(addr + 16, &[0]);
        }
    }
    if !active {
        log("fly: gravity switched off");
    }
}

fn near(a: &[u8], b: &[u8], tolerance: i64) -> bool {
    (0..3).all(|i| (le_i64(a, i * 8) - le_i64(b, i * 8)).abs() < tolerance)
}

fn set_client_position(old: &[u8], pos: [Option<i64>; 3]) -> usize {
    let Some((manager, id)) = client_pair() else { return 0 };
    let Some((_, _, slots)) = entity_layout(manager, id) else { return 0 };
    let mut count = 0;
    for slot in &slots {
        if slot.stride < TRANSFORM_SIZE || slot.stride > 128 {
            continue;
        }
        let Some(b) = read_bytes(slot.addr, 24) else { continue };
        if b.len() >= 24 && near(&b, old, metres(3.0)) {
            for (axis, value) in pos.iter().enumerate() {
                if let Some(v) = value {
                    write_bytes(slot.addr + axis * 8, &v.to_le_bytes());
                }
            }
            count += 1;
        }
    }
    count
}

fn diagnostics(st: &mut PowerState, manager: usize, id: u32, client: Option<(usize, u32)>) {
    if st.diag_left == 0 {
        return;
    }
    let now = Instant::now();
    if st.diag_last.map(|t| now.duration_since(t) < Duration::from_secs(20)).unwrap_or(false) {
        return;
    }
    st.diag_last = Some(now);
    st.diag_left -= 1;
    for (label, m, i) in [("server", manager, id)].into_iter().chain(client.map(|(m, i)| ("client", m, i))) {
        let stamina = resource(m, i, 1, STAMINA_SIGNATURE).map(|(_, b)| hex(&b[8..40])).unwrap_or_default();
        let heat = resource(m, i, 4, BODY_HEAT_SIGNATURE).map(|(_, b)| hex(&b[8..40])).unwrap_or_default();
        log(&format!("diag {label}: stamina={stamina} heat={heat}"));
        if let Some(p) = find_record(m, i).and_then(placement) {
            let mut small = Vec::new();
            for index in 0..COMPONENT_SLOTS {
                if let Some((addr, 4)) = component_address(&p, index) {
                    if let Some(b) = read_bytes(addr, 4) {
                        small.push(format!("{index}:{}", hex(&b)));
                    }
                }
            }
            log(&format!("diag {label}: 4-byte components {}", small.join(" ")));
        }
    }
}

static CLIENT_GONE_MS: AtomicU32 = AtomicU32::new(12_000);

pub fn set_client_gone_ms(ms: u32) {
    CLIENT_GONE_MS.store(ms, Ordering::SeqCst);
}

pub fn clear_client_power_files(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if super::chat::client_of_powers_file(&name).is_some() && std::fs::remove_file(entry.path()).is_ok() {
            log(&format!("powers: removed {name} left from an earlier run"));
        }
    }
}

fn refresh_client_powers(st: &mut PowerState, dir: &std::path::Path) {
    let gone: Vec<u32> = st
        .client_powers
        .iter()
        .filter(|(_, c)| c.absent_since.is_some_and(|t| t.elapsed() >= Duration::from_millis(CLIENT_GONE_MS.load(Ordering::SeqCst) as u64)))
        .map(|(client, _)| *client)
        .collect();
    for client in gone {
        st.client_powers.remove(&client);
        let _ = std::fs::remove_file(dir.join(super::chat::client_powers_file(client)));
        log(&format!("powers: chat client {client} left, its powers were dropped"));
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut present: Vec<u32> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(client) = super::chat::client_of_powers_file(&name) else { continue };
        present.push(client);
        let mtime = entry.metadata().and_then(|m| m.modified()).ok();
        let known = st.client_powers.get(&client).map(|c| c.mtime);
        if known == Some(mtime) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        match serde_json::from_str::<Powers>(&text) {
            Ok(p) => {
                log(&format!("powers (chat client {client}, entity {}): god={} stamina={} fly={}", (client & 0x3f) + 1, p.god, p.stamina, p.fly));
                let slot = st.client_powers.entry(client).or_default();
                slot.powers = p;
                slot.mtime = mtime;
            }
            Err(e) => log(&format!("powers (chat client {client}): invalid file ({e})")),
        }
    }
    st.client_powers.retain(|client, c| present.contains(client) || c.heal);
}

fn client_powers_busy(st: &PowerState) -> bool {
    st.client_powers.values().any(|c| c.powers.god || c.powers.stamina || c.heal)
}

fn apply_client_powers(st: &mut PowerState, manager: usize, tick: u32) {
    let clients: Vec<u32> = st.client_powers.keys().copied().collect();
    for client in clients {
        let id = (client & 0x3f) + 1;
        let Some(c) = st.client_powers.get_mut(&client) else { continue };
        if find_record(manager, id).is_none() {
            c.absent_since.get_or_insert_with(Instant::now);
            continue;
        }
        c.absent_since = None;
        if c.heal {
            c.heal = false;
            refill_health(manager, id);
            refill_stamina(manager, id, &mut c.stamina_peak);
            refill_mana(manager, id, &mut c.mana_peak);
        }
        if c.powers.stamina {
            if tick % 4 == 0 {
                cleanse_debuffs(manager, id, &mut c.seen);
            }
            refill_stamina(manager, id, &mut c.stamina_peak);
            refill_mana(manager, id, &mut c.mana_peak);
        }
    }
}

pub fn power_tick(st: &mut PowerState, dir: &std::path::Path, tick: u32) {
    if tick % 10 == 0 {
        refresh_powers(st, dir);
        if SERVER_MGR.load(Ordering::SeqCst) != 0 {
            refresh_client_powers(st, dir);
        }
        run_command(st, dir);
        scan_request(st, dir);
    }
    let Some((manager, id)) = world_pair() else { return };
    let hosting = SERVER_MGR.load(Ordering::SeqCst) != 0;
    if hosting && tick % 8 == 0 {
        deliveries_tick();
    }
    if hosting && tick % 4 == 0 {
        groups_tick(st, dir);
    }
    if hosting && tick % 16 == 0 {
        spawners_tick(&mut st.spawners, dir);
    }
    let client = client_pair();
    let protected = st.protect_until.map(|t| Instant::now() < t).unwrap_or(false);
    let mut want: Vec<(usize, u32)> = Vec::new();
    if st.powers.god || protected {
        want.push((manager, id));
        if let Some(pair) = client {
            want.push(pair);
        }
    }
    for (client_number, c) in &st.client_powers {
        if c.powers.god {
            let entity = (client_number & 0x3f) + 1;
            if !want.contains(&(manager, entity)) {
                want.push((SERVER_MGR.load(Ordering::SeqCst), entity));
            }
        }
    }
    for zone_id in st.zone_ids.clone() {
        let server = SERVER_MGR.load(Ordering::SeqCst);
        if server != 0 && !want.contains(&(server, zone_id)) {
            want.push((server, zone_id));
            if zone_id == id {
                if let Some(pair) = client {
                    want.push(pair);
                }
            }
        }
    }
    shield_tick(st, &want);
    if hosting {
        apply_client_powers(st, SERVER_MGR.load(Ordering::SeqCst), tick);
    }
    if st.heal_once {
        refill_health(manager, id);
        if let Some((m, c)) = client {
            refill_health(m, c);
        }
        st.heal_once = false;
        refill_stamina(manager, id, &mut st.stamina_peak);
        refill_mana(manager, id, &mut st.mana_peak);
        if let Some((m, c)) = client {
            refill_stamina(m, c, &mut st.stamina_peak_c);
            refill_mana(m, c, &mut st.mana_peak_c);
        }
    }
    if st.powers.stamina {
        if tick % 4 == 0 {
            cleanse_debuffs(manager, id, &mut st.buffs_seen);
        }
        refill_stamina(manager, id, &mut st.stamina_peak);
        refill_mana(manager, id, &mut st.mana_peak);
        if let Some((m, c)) = client {
            refill_mana(m, c, &mut st.mana_peak_c);
            refill_stamina(m, c, &mut st.stamina_peak_c);
        }
        diagnostics(st, manager, id, client);
    }
    if st.powers.fly {
        if !st.gravity_off {
            st.gravity_off = true;
            set_gravity(st, false);
            st.last_fly = None;
            st.last_written = None;
            st.hover = None;
            st.loco = [None, None];
            st.net_loco = [None, None];
            st.loco_reported = None;
        }
        hold_flying_state(st);
        fly_step(st, manager, id);
    } else if st.gravity_off {
        st.gravity_off = false;
        set_gravity(st, true);
        st.last_written = None;
        log("fly: gravity restored");
    }
}

pub fn fly_step(st: &mut PowerState, manager: usize, id: u32) {
    let Some(t) = player_transform() else { return };
    let now = Instant::now();
    let dt = st.last_fly.map(|p| now.duration_since(p).as_secs_f64()).unwrap_or(0.001).clamp(0.0005, 0.05);
    st.last_fly = Some(now);
    let (x, y, z) = (le_i64(&t, 0), le_i64(&t, 8), le_i64(&t, 16));
    let speed = st.powers.fly_speed.unwrap_or(12.0).clamp(1.0, 80.0) as f64;
    let focus = game_has_focus();
    let mut target = st.hover.unwrap_or(y);
    if y > target + metres(0.5) {
        target = y;
    }
    let vertical = metres(speed.min(40.0) * dt);
    if focus && key_down(VK_SPACE) {
        target = target.saturating_add(vertical);
    }
    if focus && key_down(VK_LCONTROL) {
        target = target.saturating_sub(vertical);
    }
    st.hover = Some(target);

    let (mut nx, mut nz) = (x, z);
    if let Some((lx, _, lz)) = st.last_written {
        let dx = (x - lx) as f64 / METERS;
        let dz = (z - lz) as f64 / METERS;
        let d = (dx * dx + dz * dz).sqrt();
        let k = (speed / 7.0 - 1.0).clamp(0.0, 12.0);
        if focus && k > 0.0 && d > 0.0 && d < 2.0 {
            nx = x.saturating_add((dx * k * METERS) as i64);
            nz = z.saturating_add((dz * k * METERS) as i64);
        }
    }
    if nx != x || nz != z || target != y {
        let pos = [Some(nx), Some(target), Some(nz)];
        set_player_position(manager, id, pos);
        let copies = set_client_position(&t, pos);
        if !st.client_logged {
            st.client_logged = true;
            log(&format!("fly: client position copies updated = {copies}"));
        }
        st.last_written = Some((nx, target, nz));
    } else {
        st.last_written = Some((x, y, z));
    }
}

pub fn new_power_state() -> PowerState {
    PowerState {
        powers: Powers::default(),
        mtime: None,
        hover: None,
        protect_until: None,
        stamina_peak: 0,
        mana_peak: 0,
        heal_once: false,
        stamina_peak_c: 0,
        mana_peak_c: 0,
        last_fly: None,
        last_written: None,
        gravity: [None, None],
        gravity_saved: [1, 1],
        loco: [None, None],
        net_loco: [None, None],
        loco_overwritten: [0, 0],
        loco_reported: None,
        gravity_off: false,
        client_logged: false,
        groups: GroupState::new(),
        saved: HashMap::new(),
        zone_ids: Vec::new(),
        diag_left: 12,
        diag_last: None,
        spawners: SpawnerSet::default(),
        buffs_seen: Vec::new(),
        client_powers: HashMap::new(),
        client_scan: None,
    }
}


fn watch_collection(manager: usize, id: u32, item: u32) {
    let _ = std::thread::Builder::new().name("m182-pickup-watch".into()).spawn(move || {
        for second in 1..=6u32 {
            std::thread::sleep(Duration::from_secs(1));
            if find_record(manager, id).is_none() {
                log(&format!("delivery: item {item} was collected after {second} s (the pickup entity {id} is gone)"));
                return;
            }
        }
        log(&format!("delivery: item {item} is still on the ground after 6 s (the pickup entity {id} was not collected)"));
    });
}

fn verify_pickup(manager: usize, id: u32, g: &GiveSpec) {
    watch_collection(manager, id, g.item);
    let Some((_, _, slots)) = entity_layout(manager, id) else {
        log(&format!("delivery: entity {id} not readable"));
        return;
    };
    for slot in &slots {
        if slot.stride != PICKUP_ITEM_SIZE {
            continue;
        }
        let Some(b) = read_bytes(slot.addr, slot.stride) else { continue };
        if b.len() >= PICKUP_ITEM_SIZE && u32_at(&b, 0) == g.item {
            log(&format!(
                "delivery: item {} placed (level {} rarity {} amount {})",
                g.item,
                u32_at(&b, 20),
                b[24],
                u16::from_le_bytes([b[26], b[27]])
            ));
            return;
        }
    }
    log(&format!("delivery: item {} requested; the pickup entity {id} was created", g.item));
}

pub fn data_regions() -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut addr = 0x10000usize;
    for _ in 0..400_000 {
        let mut info = MemInfo::default();
        let n = unsafe { VirtualQuery(addr as *const c_void, &mut info, std::mem::size_of::<MemInfo>()) };
        if n == 0 {
            break;
        }
        let end = info.base.saturating_add(info.size);
        let low = info.protect & 0xff;
        if info.state == 0x1000 && matches!(low, 0x04 | 0x08 | 0x40 | 0x80) && info.protect & 0x700 == 0 && info.kind != 0x100_0000 && info.size > 0 {
            out.push((info.base, end));
        }
        if end <= addr || end >= 0x7fff_ffff_0000 {
            break;
        }
        addr = end;
    }
    out
}

pub fn readable_regions() -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut addr = 0x10000usize;
    for _ in 0..400_000 {
        let mut info = MemInfo::default();
        let n = unsafe { VirtualQuery(addr as *const c_void, &mut info, std::mem::size_of::<MemInfo>()) };
        if n == 0 {
            break;
        }
        let end = info.base.saturating_add(info.size);
        let low = info.protect & 0xff;
        let readable = matches!(low, 0x02 | 0x04 | 0x08 | 0x20 | 0x40 | 0x80);
        if info.state == 0x1000 && readable && info.protect & 0x700 == 0 && info.size > 0 {
            out.push((info.base, end));
        }
        if end <= addr || end >= 0x7fff_ffff_0000 {
            break;
        }
        addr = end;
    }
    out
}

pub fn scan_regions(regions: &[(usize, usize)], pattern: &[u8], max_hits: usize) -> Vec<usize> {
    let mut hits = Vec::new();
    if pattern.len() < 4 {
        return hits;
    }
    let first = [pattern[0], pattern[1], pattern[2], pattern[3]];
    const CHUNK: usize = 4 << 20;
    for &(start, end) in regions {
        let mut at = start;
        while at < end {
            let len = (end - at).min(CHUNK + pattern.len());
            let Some(b) = read_bytes(at, len) else {
                at += CHUNK;
                continue;
            };
            let mut i = 0usize;
            while i + pattern.len() <= b.len() {
                if b[i..i + 4] == first && &b[i..i + pattern.len()] == pattern {
                    hits.push(at + i);
                    if hits.len() >= max_hits {
                        return hits;
                    }
                }
                i += 4;
            }
            at += CHUNK;
        }
    }
    hits
}

pub const SCAN_FILE: &str = "m182_scan.request";
pub const ONLINE_FILE: &str = "m182_online.json";
static SCAN_BUSY: AtomicBool = AtomicBool::new(false);

fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
}

fn decode_after(bytes: &[u8], wide: bool) -> String {
    if wide {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).take_while(|u| *u != 0).collect();
        String::from_utf16_lossy(&units)
    } else {
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).to_string()
    }
}

fn find_reply<T>(regions: &[(usize, usize)], marker: &str, parse: impl Fn(&str) -> Option<T>) -> Option<T> {
    let narrow = marker.as_bytes().to_vec();
    let wide = utf16le(&marker);
    const CHUNK: usize = 4 << 20;
    for &(start, end) in regions {
        if end - start > (3usize << 30) {
            continue;
        }
        let mut at = start;
        while at < end {
            let len = (end - at).min(CHUNK + 2048);
            let Some(b) = read_bytes(at, len) else {
                at += CHUNK;
                continue;
            };
            for (needle, is_wide) in [(&narrow, false), (&wide, true)] {
                for hit in roster::find_all(&b, needle) {
                    let tail = &b[hit..b.len().min(hit + 2200)];
                    let text = decode_after(tail, is_wide);
                    if let Some(found) = parse(&text) {
                        return Some(found);
                    }
                }
            }
            at += CHUNK;
        }
    }
    None
}

fn find_roster(regions: &[(usize, usize)], seq: u32) -> Option<(Vec<(u32, String)>, Option<(u32, [f64; 3])>)> {
    find_reply(regions, &roster::tag(seq), |text| roster::parse_reply(text, seq).map(|list| (list, roster::parse_own(text, seq))))
}

fn scan_roster(seq: u32, dir: std::path::PathBuf) {
    let started = Instant::now();
    let mut passes = 0u32;
    let found = loop {
        passes += 1;
        let regions = data_regions();
        if let Some(found) = find_roster(&regions, seq) {
            break Some(found);
        }
        if started.elapsed() > Duration::from_secs(60) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(400));
    };
    match found {
        Some((list, own)) => {
            let players: Vec<String> = list
                .iter()
                .map(|(n, name)| format!("{{\"n\":{n},\"name\":{}}}", serde_json::Value::String(name.clone())))
                .collect();
            let me = match own {
                Some((n, p)) => format!(",\"me\":{{\"n\":{n},\"x\":{:.1},\"y\":{:.1},\"z\":{:.1}}}", p[0], p[1], p[2]),
                None => String::new(),
            };
            let text = format!("{{\"seq\":{seq},\"players\":[{}]{me}}}", players.join(","));
            let tmp = dir.join("m182_online.json.tmp");
            if std::fs::write(&tmp, text).is_ok() {
                let _ = std::fs::rename(&tmp, dir.join(ONLINE_FILE));
            }
            log(&format!("roster {seq}: {} player(s) read from the chat after {} pass(es), {} ms", list.len(), passes, started.elapsed().as_millis()));
        }
        None => log(&format!("roster {seq}: the answer never appeared in memory ({passes} passes)")),
    }
    SCAN_BUSY.store(false, Ordering::SeqCst);
}

fn find_teams(regions: &[(usize, usize)], seq: u32) -> Option<(Vec<groups::LiveTeam>, bool)> {
    find_reply(regions, &groups::live_tag(seq), |text| groups::parse_live_line(text, seq))
}

fn scan_teams(seq: u32, dir: std::path::PathBuf) {
    let started = Instant::now();
    let found = loop {
        let regions = data_regions();
        if let Some(found) = find_teams(&regions, seq) {
            break Some(found);
        }
        if started.elapsed() > Duration::from_secs(60) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(400));
    };
    match found {
        Some((live, complete)) => {
            let tmp = dir.join("m182_teams_live.json.tmp");
            if std::fs::write(&tmp, groups::live_json(&live, complete)).is_ok() {
                let _ = std::fs::rename(&tmp, dir.join(groups::LIVE_FILE));
            }
            log(&format!("teams {seq}: {} team(s) read from the chat ({}), {} ms", live.len(), if complete { "complete" } else { "cut" }, started.elapsed().as_millis()));
        }
        None => log(&format!("teams {seq}: the answer never appeared in memory")),
    }
    SCAN_BUSY.store(false, Ordering::SeqCst);
}

fn scan_request(_st: &mut PowerState, dir: &std::path::Path) {
    let path = dir.join(SCAN_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let _ = std::fs::remove_file(&path);
    let value = serde_json::from_str::<serde_json::Value>(&text).ok();
    let seq = value.as_ref().and_then(|v| v.get("seq").and_then(|s| s.as_u64())).unwrap_or(0) as u32;
    let kind = value.as_ref().and_then(|v| v.get("kind").and_then(|k| k.as_str())).unwrap_or("").to_string();
    if SCAN_BUSY.swap(true, Ordering::SeqCst) {
        log("roster: a search is already running");
        return;
    }
    let dir = dir.to_path_buf();
    if std::thread::Builder::new().name("m182-roster".into()).spawn(move || match kind.as_str() { "teams" => scan_teams(seq, dir), _ => scan_roster(seq, dir) }).is_err() {
        SCAN_BUSY.store(false, Ordering::SeqCst);
        log("roster: could not start the search");
    }
}

fn write_anywhere(addr: usize, data: &[u8]) -> bool {
    if write_bytes(addr, data) {
        return true;
    }
    let mut old = 0u32;
    unsafe {
        if VirtualProtect(addr as *mut c_void, data.len(), 0x04, &mut old) == 0 {
            return false;
        }
    }
    let ok = write_bytes(addr, data);
    let mut tmp = 0u32;
    unsafe {
        VirtualProtect(addr as *mut c_void, data.len(), old, &mut tmp);
    }
    ok
}

pub fn read_window(base: usize, len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let step = 0x1000usize;
    let mut at = 0usize;
    while at < len {
        match read_bytes(base + at, step) {
            Some(b) if !b.is_empty() => {
                let n = b.len();
                out.extend_from_slice(&b);
                if n < step {
                    break;
                }
            }
            _ => break,
        }
        at += step;
    }
    out
}

fn hex_to_bytes(text: &str) -> Vec<u8> {
    let t = text.trim();
    let mut out = Vec::with_capacity(t.len() / 2);
    let b = t.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        match (char::from(b[i]).to_digit(16), char::from(b[i + 1]).to_digit(16)) {
            (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
            _ => return Vec::new(),
        }
        i += 2;
    }
    out
}

fn run_item_stats(cmd: Command) {
    let label = cmd.label.clone().unwrap_or_default();
    let Some(guid) = cmd.guid.as_deref().and_then(super::config::parse_guid) else {
        log(&format!("item stats {label}: invalid guid"));
        return;
    };
    let item_id = cmd.item_id.unwrap_or(0);
    let Some(stats) = cmd.stats else {
        log(&format!("item stats {label}: missing data"));
        return;
    };
    if item_id == 0 {
        log(&format!("item stats {label}: missing item id"));
        return;
    }
    apply_stats_patch(item_id, &guid, &stats);
}


// ---------------------------------------------------------------------------------------------
// Players: every player entity of the world is found by looking at the entity table, not only the
// ones whose creation was seen by the hook (friends join through other paths).
// ---------------------------------------------------------------------------------------------

static DEF_CLASS: Mutex<Option<HashMap<usize, bool>>> = Mutex::new(None);
static DEF_MGR: AtomicUsize = AtomicUsize::new(0);

static DEF_SIMILAR: Mutex<Option<HashMap<(usize, usize), bool>>> = Mutex::new(None);
const PLAYER_SIMILARITY: f32 = 0.8;

pub fn reset_player_cache() {
    if let Ok(mut g) = DEF_CLASS.lock() {
        *g = None;
    }
    if let Ok(mut g) = DEF_SIMILAR.lock() {
        *g = None;
    }
}

/// Share of components two entities have in common (intersection over union).
pub fn component_similarity(a: &[usize], b: &[usize]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let left: HashSet<usize> = a.iter().copied().collect();
    let right: HashSet<usize> = b.iter().copied().collect();
    let common = left.intersection(&right).count();
    let union = left.union(&right).count();
    if union == 0 {
        0.0
    } else {
        common as f32 / union as f32
    }
}

fn component_indices(p: &Placement) -> Option<Vec<usize>> {
    layout(p).map(|slots| slots.iter().map(|s| s.index).collect())
}

/// A candidate is accepted only if it is built like the host player (same set of components, give or
/// take the few that depend on what the character is doing), so creatures that merely carry a stamina
/// and a player faction are never listed as players.
fn resembles_host(p: &Placement, id: u32) -> bool {
    let host_manager = SERVER_MGR.load(Ordering::SeqCst);
    let host_id = PLAYER_ID.load(Ordering::SeqCst);
    let Some(host) = find_record(host_manager, host_id).and_then(placement) else { return true };
    if host.def == p.def {
        return true;
    }
    if let Ok(guard) = DEF_SIMILAR.lock() {
        if let Some(known) = guard.as_ref().and_then(|m| m.get(&(p.def, host.def))) {
            return *known;
        }
    }
    let (Some(a), Some(b)) = (component_indices(&host), component_indices(p)) else { return true };
    let score = component_similarity(&a, &b);
    let verdict = score >= PLAYER_SIMILARITY;
    if let Ok(mut guard) = DEF_SIMILAR.lock() {
        guard.get_or_insert_with(HashMap::new).insert((p.def, host.def), verdict);
    }
    log(&format!(
        "players: entity {id} ({} components) shares {:.0}% of the host player's {} components: {}",
        b.len(),
        score * 100.0,
        a.len(),
        if verdict { "accepted" } else { "ignored" }
    ));
    verdict
}

/// Players created in `manager` that the creation hook saw (the host and joined players),
/// checked on every call: the entity must still exist and carry the player stamina component.
pub(super) fn tracked_players(manager: usize) -> Vec<u32> {
    if TRACKED_MGR.load(Ordering::SeqCst) != manager {
        return Vec::new();
    }
    tracked_ids()
        .into_iter()
        .filter(|&id| {
            entity_layout(manager, id)
                .map(|(_, _, slots)| {
                    slots.iter().any(|slot| {
                        slot.stride == 40 && matches!(read_bytes(slot.addr, 4), Some(b) if b.len() >= 4 && u32_at(&b, 0) == STAMINA_SIGNATURE)
                    })
                })
                .unwrap_or(false)
        })
        .collect()
}

pub fn tracked_ids() -> Vec<u32> {
    TRACKED.lock().map(|l| l.iter().map(|p| p.id).collect()).unwrap_or_default()
}

fn def_is_player(manager: usize, id: u32, p: &Placement) -> bool {
    if let Ok(guard) = DEF_CLASS.lock() {
        if let Some(known) = guard.as_ref().and_then(|m| m.get(&p.def)) {
            return *known;
        }
    }
    let verdict = match layout(p) {
        Some(slots) => slots.iter().any(|slot| {
            slot.stride == 40 && matches!(read_bytes(slot.addr, 4), Some(b) if b.len() >= 4 && u32_at(&b, 0) == STAMINA_SIGNATURE)
        }),
        None => return false,
    };
    if let Ok(mut guard) = DEF_CLASS.lock() {
        guard.get_or_insert_with(HashMap::new).insert(p.def, verdict);
    }
    if verdict {
        log(&format!("players: entity definition {:#x} (first seen on entity {id}) looks like a player", p.def));
    }
    let _ = manager;
    verdict
}

static TABLE_NOTES: Mutex<Vec<(usize, u32, &'static str)>> = Mutex::new(Vec::new());

fn table_note(index: usize, client: u32, reason: &'static str, detail: &str) {
    let Ok(mut notes) = TABLE_NOTES.lock() else { return };
    if notes.iter().any(|(i, c, r)| *i == index && *c == client && *r == reason) {
        return;
    }
    notes.retain(|(i, c, _)| !(*i == index && *c == client));
    notes.push((index, client, reason));
    log(&format!("players: connected slot {index} (client {client}) is not tracked yet: {reason}{detail}"));
}

/// Every connected player of the server's table gets tracked as soon as an entity of the right
/// kind exists for its slot, whatever the way the game created it.
pub fn track_table_players(manager: usize) -> usize {
    if manager == 0 {
        return 0;
    }
    let known = tracked_ids();
    let mut found = 0;
    for entry in super::table::entries() {
        let id = entry.entity();
        if known.contains(&id) {
            continue;
        }
        let Some(record) = find_record(manager, id) else {
            table_note(entry.index, entry.client, "its entity does not exist yet", "");
            continue;
        };
        let Some(p) = placement(record) else {
            table_note(entry.index, entry.client, "its entity has no readable layout", "");
            continue;
        };
        if transform_of(manager, id).is_none() {
            table_note(entry.index, entry.client, "its entity has no position yet", "");
            continue;
        }
        if !def_is_player(manager, id, &p) {
            table_note(entry.index, entry.client, "the entity of its slot is not a player", &format!(" (entity {id})"));
            continue;
        }
        track_player(manager, id);
        found += 1;
    }
    found
}

/// Looks through the whole entity table once and tracks every player-like entity. Returns how many
/// new players were found.
pub fn scan_players_once(manager: usize) -> usize {
    if manager == 0 {
        return 0;
    }
    if DEF_MGR.swap(manager, Ordering::SeqCst) != manager {
        reset_player_cache();
    }
    let Some(head) = read_bytes(manager + 0x148, 0x58) else { return 0 };
    let capacity = u64_at(&head, 0x10);
    if capacity == 0 || capacity > (1 << 22) || capacity & (capacity - 1) != 0 {
        return 0;
    }
    let words = (capacity as usize).div_ceil(64);
    let Some(bitmap) = read_bytes(u64_at(&head, 8) as usize, words * 8) else { return 0 };
    let Some(keys) = read_bytes(u64_at(&head, 0x28) as usize, capacity as usize * 4) else { return 0 };
    let Some(values) = read_bytes(u64_at(&head, 0x40) as usize, capacity as usize * 8) else { return 0 };
    if bitmap.len() < words * 8 || keys.len() < capacity as usize * 4 || values.len() < capacity as usize * 8 {
        return 0;
    }
    let known: HashSet<u32> = TRACKED.lock().map(|l| l.iter().map(|p| p.id).collect()).unwrap_or_default();
    let mut found = 0;
    for slot in 0..capacity as usize {
        if (u64_at(&bitmap, (slot >> 6) * 8) >> (slot & 63)) & 1 == 0 {
            continue;
        }
        let id = u32_at(&keys, slot * 4);
        if id == 0 || known.contains(&id) {
            continue;
        }
        let record = u64_at(&values, slot * 8) as usize;
        if record < 0x10000 {
            continue;
        }
        let Some(p) = placement(record) else { continue };
        if let Ok(guard) = DEF_CLASS.lock() {
            if guard.as_ref().and_then(|m| m.get(&p.def)) == Some(&false) {
                continue;
            }
        }
        if !def_is_player(manager, id, &p) {
            continue;
        }
        if !resembles_host(&p, id) {
            continue;
        }
        if faction_addresses(manager, id).is_empty() {
            continue;
        }
        track_player(manager, id);
        found += 1;
    }
    found
}

pub(super) fn server_manager() -> usize {
    SERVER_MGR.load(Ordering::SeqCst)
}

pub(super) fn host_player() -> u32 {
    PLAYER_ID.load(Ordering::SeqCst)
}

/// Entity ids of every player character in `manager` (host and connected players).
pub(super) fn player_entities(manager: usize) -> Vec<u32> {
    let mut out = Vec::new();
    let Some(head) = read_bytes(manager + 0x148, 0x58) else { return out };
    let capacity = u64_at(&head, 0x10);
    if capacity == 0 || capacity > (1 << 22) || capacity & (capacity - 1) != 0 {
        return out;
    }
    let words = (capacity as usize).div_ceil(64);
    let Some(bitmap) = read_bytes(u64_at(&head, 8) as usize, words * 8) else { return out };
    let Some(keys) = read_bytes(u64_at(&head, 0x28) as usize, capacity as usize * 4) else { return out };
    let Some(values) = read_bytes(u64_at(&head, 0x40) as usize, capacity as usize * 8) else { return out };
    if bitmap.len() < words * 8 || keys.len() < capacity as usize * 4 || values.len() < capacity as usize * 8 {
        return out;
    }
    for slot in 0..capacity as usize {
        if (u64_at(&bitmap, (slot >> 6) * 8) >> (slot & 63)) & 1 == 0 {
            continue;
        }
        let id = u32_at(&keys, slot * 4);
        let record = u64_at(&values, slot * 8) as usize;
        if id == 0 || record < 0x10000 {
            continue;
        }
        let Some(p) = placement(record) else { continue };
        if def_is_player(manager, id, &p) && !faction_addresses(manager, id).is_empty() {
            out.push(id);
        }
        if out.len() >= 32 {
            break;
        }
    }
    out
}

fn players_loop() {
    loop {
        std::thread::sleep(Duration::from_millis(2500));
        let manager = SERVER_MGR.load(Ordering::SeqCst);
        if manager == 0 || PLAYER_ID.load(Ordering::SeqCst) == 0 {
            continue;
        }
        let _ = std::panic::catch_unwind(|| track_table_players(manager));
        let _ = std::panic::catch_unwind(|| scan_players_once(manager));
    }
}

// ---------------------------------------------------------------------------------------------
// Spawners: creatures bound to a place that come back after they die (or after a restart).
// ---------------------------------------------------------------------------------------------

pub const SPAWNERS_FILE: &str = "m182_spawners.json";
pub const SPAWNER_STATUS_FILE: &str = "m182_spawner_status.json";

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
struct SpawnerDef {
    id: String,
    name: String,
    template: String,
    level: u32,
    count: u32,
    health: u32,
    damage: f32,
    at: Option<[f64; 3]>,
    radius: f32,
    respawn: u32,
    enabled: Option<bool>,
    /// A raid: the wave is created once and the spawner is gone when it has been killed.
    once: bool,
    /// What falls where each creature of this spawner dies.
    drops: Vec<DropDef>,
    /// Template of the pickup that carries a drop (the generic one when empty).
    pickup: String,
    /// Automatic raids: every `every` seconds one of these is chosen at random and appears at a
    /// random point within `area` meters of `at`.
    pool: Vec<PoolEntry>,
    every: u32,
    area: f32,
    /// Enemies of the wave being played, in the order they are created (filled by the spawner).
    parts: Vec<Member>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
struct Member {
    template: String,
    level: u32,
    count: u32,
    health: u32,
    damage: f32,
    drops: Vec<DropDef>,
}

/// One raid template: a group of enemies (each with its own amount, level and loot). The single
/// enemy fields are the older format and still work.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
struct PoolEntry {
    name: String,
    members: Vec<Member>,
    template: String,
    level: u32,
    count: u32,
    health: u32,
    damage: f32,
    drops: Vec<DropDef>,
}

impl PoolEntry {
    fn group(&self) -> Vec<Member> {
        let mut out: Vec<Member> = self.members.iter().filter(|m| !m.template.trim().is_empty()).take(MAX_MEMBERS).cloned().collect();
        if out.is_empty() && !self.template.trim().is_empty() {
            out.push(Member { template: self.template.clone(), level: self.level, count: self.count, health: self.health, damage: self.damage, drops: self.drops.clone() });
        }
        for m in out.iter_mut() {
            m.count = m.count.clamp(1, 20);
            m.level = m.level.max(1);
        }
        out
    }
}

const MAX_MEMBERS: usize = 10;
const MAX_WAVE: u32 = 60;

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
struct DropDef {
    item: u32,
    guid: String,
    level: u32,
    rarity: u32,
    amount: u32,
    /// 0-100, empty means always.
    chance: f32,
}

const GENERIC_PICKUP: &str = "c15a5c9d-4956-49ff-9d72-9f522229cf1c";
const MAX_DROPS: usize = 8;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SpawnersFile {
    spawners: Vec<SpawnerDef>,
}

struct SpawnerRt {
    def: SpawnerDef,
    ids: Vec<u32>,
    missing: HashMap<u32, u8>,
    respawn_at: Option<Instant>,
    spawned: u32,
    manager: usize,
    state: &'static str,
    was_active: bool,
    check_at: Option<Instant>,
    last: HashMap<u32, [f64; 3]>,
    done: bool,
    wave: Option<SpawnerDef>,
    next_wave: Option<Instant>,
    wave_started: Option<Instant>,
    part: usize,
    part_spawned: u32,
    owner: HashMap<u32, usize>,
    /// Creatures of waves that were left behind when the next wave came (id, their loot).
    detached: Vec<(u32, Vec<DropDef>)>,
}

impl SpawnerRt {
    fn cur(&self) -> &SpawnerDef {
        self.wave.as_ref().unwrap_or(&self.def)
    }

    /// Creatures the current wave is made of.
    fn total(&self) -> usize {
        let w = self.cur();
        if w.parts.is_empty() {
            w.count.clamp(1, 20) as usize
        } else {
            w.parts.iter().map(|m| m.count.clamp(1, 20) as usize).sum::<usize>().min(MAX_WAVE as usize)
        }
    }

    /// Loot of the creature `id`.
    fn drops_of(&self, id: u32) -> &[DropDef] {
        let w = self.cur();
        if w.parts.is_empty() {
            &w.drops
        } else {
            self.owner.get(&id).and_then(|i| w.parts.get(*i)).map(|m| m.drops.as_slice()).unwrap_or(&[])
        }
    }

    fn has_loot(&self) -> bool {
        let w = self.cur();
        !w.drops.is_empty() || w.parts.iter().any(|m| !m.drops.is_empty())
    }
}

const SPAWNER_RANGE_METERS: f64 = 150.0;

/// Seconds from `unix` to the next multiple of `every` since the epoch: every automatic raid
/// zone with the same interval is due at the same instant, whenever the server was started.
fn secs_to_boundary(unix: u64, every: u64) -> u64 {
    let every = every.max(1);
    every - unix % every
}

fn next_boundary(now: Instant, every: Duration) -> Instant {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    now + Duration::from_secs(secs_to_boundary(unix, every.as_secs()))
}

fn player_positions(manager: usize) -> Vec<(f64, f64)> {
    let mut ids: Vec<u32> = TRACKED.lock().map(|list| list.iter().map(|p| p.id).collect()).unwrap_or_default();
    if let Some((_, id)) = world_pair() {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.into_iter()
        .filter_map(|id| transform_of(manager, id))
        .map(|t| (le_i64(&t, 0) as f64 / METERS, le_i64(&t, 16) as f64 / METERS))
        .collect()
}

#[derive(Default)]
pub struct SpawnerSet {
    list: Vec<SpawnerRt>,
    mtime: Option<std::time::SystemTime>,
    last: Option<Instant>,
    pending: Option<(String, Instant)>,
    status: String,
    status_at: Option<Instant>,
    finished: Vec<String>,
    done_ids: Vec<String>,
}

fn health_now(manager: usize, id: u32) -> Option<u32> {
    let (_, _, slots) = entity_layout(manager, id)?;
    for slot in &slots {
        if slot.stride == 40 {
            if let Some(b) = read_bytes(slot.addr, 12) {
                if b.len() >= 12 && u32_at(&b, 0) == HEALTH_SIGNATURE {
                    return Some(u32_at(&b, 8));
                }
            }
        }
    }
    Some(u32::MAX)
}

/// A creature counts as gone only after it was missing, or at zero health, on three checks in a row
/// (a few seconds): right after creation or while the world streams, one bad reading is not a death.
fn creature_alive(manager: usize, id: u32, missing: &mut HashMap<u32, u8>) -> bool {
    let bad = matches!(health_now(manager, id), None | Some(0));
    if bad {
        let n = missing.entry(id).or_insert(0);
        *n += 1;
        *n < 3
    } else {
        missing.remove(&id);
        true
    }
}

fn load_spawners(set: &mut SpawnerSet, dir: &std::path::Path) {
    let path = dir.join(SPAWNERS_FILE);
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    if mtime == set.mtime {
        return;
    }
    set.mtime = mtime;
    let defs: Vec<SpawnerDef> = match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<SpawnersFile>(&text) {
            Ok(f) => f.spawners.into_iter().filter(|d| !d.id.is_empty() && !d.template.trim().is_empty() && d.enabled.unwrap_or(true)).collect(),
            Err(e) => {
                log(&format!("spawners: invalid file ({e})"));
                return;
            }
        },
        Err(_) => Vec::new(),
    };
    let mut next: Vec<SpawnerRt> = Vec::new();
    for def in defs {
        match set.list.iter().position(|r| r.def.id == def.id) {
            Some(i) => {
                let mut rt = set.list.swap_remove(i);
                if rt.def.every != def.every && !def.pool.is_empty() && !def.once {
                    rt.next_wave = Some(next_boundary(Instant::now(), Duration::from_secs(def.every.clamp(60, 604_800) as u64)));
                }
                rt.def = def;
                next.push(rt);
            }
            None => next.push(SpawnerRt { def, ids: Vec::new(), missing: HashMap::new(), respawn_at: None, spawned: 0, manager: 0, state: "preparando", was_active: false, check_at: None, last: HashMap::new(), done: false, wave: None, next_wave: None, wave_started: None, part: 0, part_spawned: 0, owner: HashMap::new(), detached: Vec::new() }),
        }
    }
    set.list = next;
    log(&format!("spawners: {} active", set.list.len()));
}

/// Takes finished raids out of the spawner file so they do not come back after a restart.
fn remove_spawner_rows(dir: &std::path::Path, ids: &[String]) {
    let path = dir.join(SPAWNERS_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    let Some(rows) = value.get_mut("spawners").and_then(|r| r.as_array_mut()) else { return };
    let before = rows.len();
    rows.retain(|r| !r.get("id").and_then(|i| i.as_str()).map(|i| ids.iter().any(|d| d == i)).unwrap_or(false));
    if rows.len() == before {
        return;
    }
    let temp = dir.join(format!("{SPAWNERS_FILE}.tmp"));
    if std::fs::write(&temp, value.to_string()).is_ok() && std::fs::rename(&temp, &path).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

static DROPS: Mutex<std::collections::VecDeque<(GiveSpec, String, [f64; 3])>> = Mutex::new(std::collections::VecDeque::new());

fn roll_percent() -> f32 {
    use std::sync::atomic::AtomicU64;
    static SEED: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos() as u64).unwrap_or(1);
    let mut x = SEED.load(Ordering::Relaxed) ^ nanos.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x1234_5678_9ABC_DEF1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    SEED.store(x, Ordering::Relaxed);
    (x % 10_000) as f32 / 100.0
}

/// The drops of a spawner fall where its creature died (each one rolls its own chance).
fn queue_drops(d: &SpawnerDef, spot: [f64; 3]) {
    queue_drop_list(&d.drops, &d.pickup, spot);
}

fn queue_drop_list(list: &[DropDef], pickup: &str, spot: [f64; 3]) {
    let pickup = if pickup.trim().is_empty() { GENERIC_PICKUP.to_string() } else { pickup.trim().to_string() };
    for drop in list.iter().take(MAX_DROPS) {
        let Some(guid) = super::config::parse_guid(&drop.guid) else { continue };
        if drop.item == 0 {
            continue;
        }
        let chance = if drop.chance <= 0.0 { 100.0 } else { drop.chance.min(100.0) };
        if chance < 100.0 && roll_percent() >= chance {
            continue;
        }
        let spec = GiveSpec { item: drop.item, level: drop.level.clamp(1, 255), rarity: drop.rarity.min(4), amount: drop.amount.clamp(1, 65535), guid };
        if let Ok(mut q) = DROPS.lock() {
            if q.len() < 200 {
                q.push_back((spec, pickup.clone(), spot));
            }
        }
    }
}

fn drops_tick() -> bool {
    let Some((spec, template, spot)) = DROPS.lock().ok().and_then(|mut q| q.pop_front()) else { return false };
    let settings = SpawnSettings {
        template,
        level: 1,
        count: 1,
        distance: 2.0,
        health: 0,
        damage: 1.0,
        key: 0,
        dump_player_key: 0,
        dump_spawned_key: 0,
        watch_key: 0,
    };
    if let Ok(mut r) = REQUEST.lock() {
        *r = Some(settings);
    }
    if let Ok(mut g) = REQUEST_GIVE.lock() {
        *g = Some(spec);
    }
    if let Ok(mut r) = REQUEST_AT.lock() {
        *r = Some(spot);
    }
    REQUEST_SENDER.store(0, Ordering::SeqCst);
    REQUEST_OBJECT.store(true, Ordering::SeqCst);
    REQUEST_FORWARD.store(false, Ordering::SeqCst);
    if let Ok(mut t) = SPAWN_TAG.lock() {
        *t = None;
    }
    PENDING.store(1, Ordering::SeqCst);
    log(&format!("drop: item {} falls at x={:.1} z={:.1}", spec.item, spot[0], spot[2]));
    true
}

fn roll_unit() -> f64 {
    roll_percent() as f64 / 100.0
}

/// One automatic raid: a random template (a group of enemies) of the pool at a random point inside the area.
fn pick_wave(def: &SpawnerDef) -> Option<SpawnerDef> {
    let center = def.at?;
    let usable: Vec<(&PoolEntry, Vec<Member>)> = def.pool.iter().map(|e| (e, e.group())).filter(|(_, g)| !g.is_empty()).collect();
    if usable.is_empty() {
        return None;
    }
    let (_, group) = &usable[((roll_unit() * usable.len() as f64) as usize).min(usable.len() - 1)];
    let reach = def.area.clamp(0.0, 600.0) as f64;
    let angle = roll_unit() * std::f64::consts::TAU;
    let dist = roll_unit().sqrt() * reach;
    let at = [center[0] + dist * angle.cos(), center[1] + 3.0, center[2] + dist * angle.sin()];
    let total: u32 = group.iter().map(|m| m.count).sum();
    let first = group[0].clone();
    Some(SpawnerDef {
        id: def.id.clone(),
        name: def.name.clone(),
        template: first.template.clone(),
        level: first.level,
        count: total.clamp(1, MAX_WAVE),
        health: first.health,
        damage: first.damage,
        at: Some(at),
        radius: if total > 1 { 6.0 } else { 3.0 },
        respawn: 0,
        enabled: None,
        once: true,
        drops: Vec::new(),
        pickup: def.pickup.clone(),
        pool: Vec::new(),
        every: 0,
        area: 0.0,
        parts: group.clone(),
    })
}

fn issue_spawn(rt: &SpawnerRt, need: u32) {
    let d = rt.cur();
    let m = d.parts.get(rt.part);
    let (template, level, health, damage) = match m {
        Some(m) => (m.template.as_str(), m.level, m.health, m.damage),
        None => (d.template.as_str(), d.level, d.health, d.damage),
    };
    let settings = SpawnSettings {
        template: template.trim().to_string(),
        level: level.clamp(1, 1000),
        count: need.clamp(1, 20),
        distance: if d.radius > 0.0 { d.radius.clamp(0.5, 60.0) } else { 3.0 },
        health: health.min(2_000_000_000),
        damage: damage.clamp(0.0, 1000.0).max(if damage == 0.0 { 1.0 } else { 0.0 }),
        key: 0,
        dump_player_key: 0,
        dump_spawned_key: 0,
        watch_key: 0,
    };
    if let Ok(mut r) = REQUEST.lock() {
        *r = Some(settings);
    }
    if let Ok(mut g) = REQUEST_GIVE.lock() {
        *g = None;
    }
    if let Ok(mut r) = REQUEST_AT.lock() {
        *r = d.at.filter(|p| p.iter().all(|v| v.is_finite() && v.abs() < 1.0e6));
    }
    REQUEST_OBJECT.store(false, Ordering::SeqCst);
    REQUEST_FORWARD.store(false, Ordering::SeqCst);
    if let Ok(mut t) = SPAWN_TAG.lock() {
        *t = Some(d.id.clone());
    }
    PENDING.store(1, Ordering::SeqCst);
}

fn spawners_tick(set: &mut SpawnerSet, dir: &std::path::Path) {
    let now = Instant::now();
    if set.last.map(|t| now.duration_since(t) < Duration::from_secs(1)).unwrap_or(false) {
        return;
    }
    set.last = Some(now);
    load_spawners(set, dir);

    let created: Vec<(String, u32)> = CREATED.lock().map(|mut c| c.drain(..).collect()).unwrap_or_default();
    for (tag, id) in created {
        if let Some(rt) = set.list.iter_mut().find(|r| r.def.id == tag) {
            rt.ids.push(id);
            rt.spawned += 1;
            rt.check_at = Some(now + Duration::from_secs(3));
            let part = rt.part;
            rt.owner.insert(id, part);
            rt.part_spawned += 1;
        }
    }
    for rt in set.list.iter_mut() {
        let more = rt.cur().parts.get(rt.part).map(|m| m.count.clamp(1, 20)).map(|c| rt.part_spawned >= c && rt.part + 1 < rt.cur().parts.len()).unwrap_or(false);
        if more {
            rt.part += 1;
            rt.part_spawned = 0;
        }
    }
    if let Some((tag, since)) = set.pending.clone() {
        let finished = PENDING.load(Ordering::SeqCst) == 0 && SPAWN_TAG.lock().map(|t| t.is_none()).unwrap_or(true);
        if finished {
            set.pending = None;
        } else if since.elapsed() > Duration::from_secs(12) {
            PENDING.store(0, Ordering::SeqCst);
            if let Ok(mut r) = REQUEST.lock() {
                *r = None;
            }
            if let Ok(mut r) = REQUEST_AT.lock() {
                *r = None;
            }
            if let Ok(mut t) = SPAWN_TAG.lock() {
                *t = None;
            }
            log(&format!("spawners: {tag} could not be created in time, will retry"));
            set.pending = None;
        }
    }

    let manager = SERVER_MGR.load(Ordering::SeqCst);
    let ready_world = manager != 0 && player_transform().is_some();
    let positions = if manager != 0 { player_positions(manager) } else { Vec::new() };
    let busy = set.pending.is_some() || PENDING.load(Ordering::SeqCst) != 0 || REQUEST.lock().map(|r| r.is_some()).unwrap_or(true);
    let mut issued: Option<(String, u32)> = None;
    for rt in set.list.iter_mut() {
        if manager == 0 {
            rt.state = "esperando el mundo";
            continue;
        }
        if rt.manager != manager {
            if rt.manager != 0 {
                rt.respawn_at = Some(now + Duration::from_secs(10));
                log(&format!("spawners: world changed, {} will be created again", rt.def.id));
            }
            rt.manager = manager;
            rt.ids.clear();
            rt.missing.clear();
            rt.last.clear();
            rt.wave = None;
            rt.spawned = 0;
            rt.part = 0;
            rt.part_spawned = 0;
            rt.owner.clear();
            rt.detached.clear();
        }
        if rt.done {
            rt.state = "completado";
            continue;
        }
        let active = match rt.def.at {
            Some(p) if !positions.is_empty() => positions.iter().any(|(x, z)| ((x - p[0]).powi(2) + (z - p[2]).powi(2)).sqrt() <= SPAWNER_RANGE_METERS),
            _ => true,
        };
        if !active {
            rt.state = "sin jugadores cerca";
            rt.was_active = false;
            continue;
        }
        let woke = !rt.was_active;
        rt.was_active = true;
        let before = rt.ids.len();
        let ids_before = rt.ids.clone();
        let mut missing = std::mem::take(&mut rt.missing);
        rt.ids.retain(|id| creature_alive(manager, *id, &mut missing));
        rt.missing = missing;
        for dead in ids_before.iter().filter(|id| !rt.ids.contains(id)) {
            if let Some(spot) = rt.last.remove(dead) {
                if !woke {
                    queue_drop_list(rt.drops_of(*dead), &rt.cur().pickup, spot);
                }
            }
            rt.owner.remove(dead);
        }
        if rt.has_loot() {
            for id in rt.ids.iter() {
                if let Some(tr) = transform_of(manager, *id) {
                    rt.last.insert(*id, [le_i64(&tr, 0) as f64 / METERS, le_i64(&tr, 8) as f64 / METERS, le_i64(&tr, 16) as f64 / METERS]);
                }
            }
        }
        if !rt.detached.is_empty() {
            let mut missing = std::mem::take(&mut rt.missing);
            let old = std::mem::take(&mut rt.detached);
            for (id, drops) in old {
                if creature_alive(manager, id, &mut missing) {
                    if !drops.is_empty() {
                        if let Some(tr) = transform_of(manager, id) {
                            rt.last.insert(id, [le_i64(&tr, 0) as f64 / METERS, le_i64(&tr, 8) as f64 / METERS, le_i64(&tr, 16) as f64 / METERS]);
                        }
                    }
                    rt.detached.push((id, drops));
                } else if let Some(spot) = rt.last.remove(&id) {
                    if !woke {
                        queue_drop_list(&drops, &rt.cur().pickup, spot);
                    }
                }
            }
            rt.missing = missing;
        }
        let respawn = Duration::from_secs(rt.def.respawn.clamp(1, 86_400) as u64);
        if rt.ids.len() < before && rt.respawn_at.is_none() {
            if woke {
                rt.respawn_at = Some(now);
                log(&format!("spawners: {} lost {} creature(s) while nobody was near, they come back now", rt.def.id, before - rt.ids.len()));
            } else {
                rt.respawn_at = Some(now + respawn);
                log(&format!("spawners: {} lost a creature, it comes back in {} s", rt.def.id, respawn.as_secs()));
            }
        }
        if let Some(t) = rt.check_at {
            if now >= t {
                rt.check_at = None;
                for id in rt.ids.iter().take(3) {
                    match transform_of(manager, *id) {
                        Some(tr) => {
                            let (x, y, z) = (le_i64(&tr, 0) as f64 / METERS, le_i64(&tr, 8) as f64 / METERS, le_i64(&tr, 16) as f64 / METERS);
                            let nearest = positions.iter().map(|(px, pz)| ((px - x).powi(2) + (pz - z).powi(2)).sqrt()).fold(f64::MAX, f64::min);
                            log(&format!("spawners: {} creature {id} is at x={x:.1} y={y:.1} z={z:.1}, {nearest:.0} m from the nearest player", rt.def.id));
                        }
                        None => log(&format!("spawners: {} creature {id} position could not be read", rt.def.id)),
                    }
                }
            }
        }
        let pooled = !rt.def.pool.is_empty();
        if pooled && !rt.def.once && rt.wave.is_some() && rt.next_wave.map(|d| now >= d).unwrap_or(false) {
            // The next wave is due: it comes even if the players have not cleared the last one.
            let ids = std::mem::take(&mut rt.ids);
            let left: Vec<(u32, Vec<DropDef>)> = ids.iter().map(|id| (*id, rt.drops_of(*id).to_vec())).collect();
            log(&format!("spawners: {} next wave is due, {} creature(s) of the last one stay", rt.def.id, left.len()));
            rt.detached.extend(left);
            rt.wave = None;
            rt.spawned = 0;
            rt.part = 0;
            rt.part_spawned = 0;
            rt.owner.clear();
            rt.check_at = None;
            rt.respawn_at = None;
        }
        if pooled && rt.wave.is_none() {
            let every = Duration::from_secs(rt.def.every.clamp(60, 604_800) as u64);
            let first = if rt.def.once { now } else { next_boundary(now, every) };
            let due = *rt.next_wave.get_or_insert(first);
            if now < due {
                rt.state = "wave_wait";
                continue;
            }
            if busy || issued.is_some() || !ready_world {
                continue;
            }
            match pick_wave(&rt.def) {
                Some(w) => {
                    log(&format!("spawners: {} starts a wave of {} at x={:.1} z={:.1}", rt.def.id, w.parts.iter().map(|m| m.count).sum::<u32>().max(w.count), w.at.map(|a| a[0]).unwrap_or(0.0), w.at.map(|a| a[2]).unwrap_or(0.0)));
                    rt.wave = Some(w);
                    rt.spawned = 0;
                    rt.part = 0;
                    rt.part_spawned = 0;
                    rt.owner.clear();
                    rt.respawn_at = None;
                    rt.wave_started = Some(now);
                    if !rt.def.once {
                        rt.next_wave = Some(next_boundary(now, every));
                    }
                }
                None => {
                    rt.next_wave = Some(now + every);
                    continue;
                }
            }
        }
        let once = rt.def.once || pooled;
        let want = rt.total();
        if once && rt.spawned as usize >= want {
            if rt.ids.is_empty() && rt.check_at.is_none() && set.pending.is_none() {
                if pooled && !rt.def.once {
                    rt.wave = None;
                    rt.spawned = 0;
                    rt.state = "wave_wait";
                    log(&format!("spawners: wave of {} was wiped out, the next one comes later", rt.def.id));
                } else {
                    rt.done = true;
                    rt.state = "completado";
                    set.finished.push(rt.def.id.clone());
                    log(&format!("spawners: raid {} was wiped out, it is removed", rt.def.id));
                }
            } else {
                rt.state = "vivo";
            }
            continue;
        }
        if rt.ids.len() >= want {
            rt.respawn_at = None;
            rt.state = "vivo";
            continue;
        }
        let due = if once { now } else { *rt.respawn_at.get_or_insert(now) };
        if now < due {
            rt.state = "esperando para reaparecer";
            continue;
        }
        rt.state = "creando";
        if busy || issued.is_some() || !ready_world {
            continue;
        }
        let need = if once {
            match rt.cur().parts.get(rt.part) {
                Some(m) => m.count.clamp(1, 20).saturating_sub(rt.part_spawned),
                None => want.saturating_sub(rt.spawned as usize) as u32,
            }
        } else {
            (want - rt.ids.len()) as u32
        };
        if need == 0 {
            continue;
        }
        issue_spawn(rt, need);
        rt.respawn_at = Some(now + Duration::from_secs(30));
        issued = Some((rt.def.id.clone(), need));
    }
    if !set.finished.is_empty() {
        let gone: Vec<String> = set.finished.clone();
        remove_spawner_rows(dir, &gone);
        set.mtime = std::fs::metadata(dir.join(SPAWNERS_FILE)).and_then(|m| m.modified()).ok();
        set.list.retain(|r| !gone.contains(&r.def.id));
        set.done_ids.extend(gone);
        let extra = set.done_ids.len().saturating_sub(50);
        set.done_ids.drain(..extra);
        set.finished.clear();
    }
    if let Some((tag, need)) = issued {
        set.pending = Some((tag.clone(), now));
        log(&format!("spawners: creating {need} for {tag}"));
    }

    // status for the panel
    let rows: Vec<String> = set
        .list
        .iter()
        .map(|rt| {
            let when = if rt.def.pool.is_empty() { rt.respawn_at } else { rt.next_wave.filter(|_| rt.wave.is_none()) };
            let next = when.map(|t| t.saturating_duration_since(now).as_secs().to_string()).unwrap_or_else(|| "null".to_string());
            format!(
                "{{\"id\":{},\"alive\":{},\"want\":{},\"state\":\"{}\",\"next\":{},\"spawned\":{}}}",
                serde_json::to_string(&rt.def.id).unwrap_or_else(|_| "\"\"".into()),
                rt.ids.len(),
                rt.total(),
                rt.state,
                if rt.ids.len() >= rt.total() && rt.def.pool.is_empty() { "null".to_string() } else { next },
                rt.spawned
            )
        })
        .collect();
    let done: Vec<String> = set.done_ids.iter().map(|d| serde_json::to_string(d).unwrap_or_else(|_| "\"\"".into())).collect();
    let text = format!("{{\"spawners\":[{}],\"done\":[{}]}}", rows.join(","), done.join(","));
    if text != set.status && set.status_at.map(|t| now.duration_since(t) >= Duration::from_secs(2)).unwrap_or(true) {
        set.status = text.clone();
        set.status_at = Some(now);
        let _ = std::fs::write(dir.join(SPAWNER_STATUS_FILE), text);
    }
}

fn powers_loop() {
    let Ok(dir) = std::env::current_dir() else { return };
    unsafe {
        timeBeginPeriod(1);
    }
    let mut st = new_power_state();
    clear_client_power_files(&dir);
    let mut tick = 0u32;
    loop {
        let busy = st.powers.god
            || st.powers.stamina
            || st.powers.fly
            || st.heal_once
            || !st.saved.is_empty()
            || !st.zone_ids.is_empty()
            || !st.groups.zones.is_empty()
            || client_powers_busy(&st)
            || st.protect_until.map(|t| Instant::now() < t).unwrap_or(false);
        std::thread::sleep(Duration::from_millis(if busy { 1 } else { 25 }));
        tick = tick.wrapping_add(1);
        power_tick(&mut st, &dir, tick);
    }
}

#[cfg(test)]
mod raid_tests {
    use super::*;

    fn def(chance: f32) -> SpawnerDef {
        serde_json::from_str(&format!(
            r#"{{"id":"r1","template":"333ea93f-04f6-4bd2-b003-1582429dfb66","once":true,
               "drops":[{{"item":77,"guid":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee","amount":3,"rarity":2,"level":10,"chance":{chance}}},
                        {{"item":0,"guid":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"}},{{"item":5,"guid":"bad"}}]}}"#
        ))
        .unwrap()
    }

    #[test]
    fn a_dead_creature_drops_only_valid_items_where_it_fell() {
        DROPS.lock().unwrap().clear();
        let d = def(100.0);
        assert!(d.once && d.drops.len() == 3);
        queue_drops(&d, [10.0, 5.0, -3.0]);
        let q = DROPS.lock().unwrap().clone();
        assert_eq!(q.len(), 1);
        assert_eq!((q[0].0.item, q[0].0.amount, q[0].0.rarity, q[0].0.level), (77, 3, 2, 10));
        assert_eq!(q[0].1, GENERIC_PICKUP);
        assert_eq!(q[0].2, [10.0, 5.0, -3.0]);
        DROPS.lock().unwrap().clear();
    }

    #[test]
    fn chance_decides_how_often_it_drops() {
        DROPS.lock().unwrap().clear();
        let d = def(30.0);
        for _ in 0..400 {
            queue_drops(&d, [0.0; 3]);
            std::thread::sleep(Duration::from_micros(150));
        }
        let n = DROPS.lock().unwrap().len();
        DROPS.lock().unwrap().clear();
        assert!(n > 40 && n < 200, "{n} of 400 at 30%");
    }

    #[test]
    fn finished_raids_leave_the_file() {
        let dir = std::env::temp_dir().join(format!("m182_raid_native_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SPAWNERS_FILE), r#"{"spawners":[{"id":"a"},{"id":"b"},{"id":"c"}]}"#).unwrap();
        remove_spawner_rows(&dir, &["a".to_string(), "c".to_string()]);
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join(SPAWNERS_FILE)).unwrap()).unwrap();
        assert_eq!(v["spawners"].as_array().unwrap().len(), 1);
        assert_eq!(v["spawners"][0]["id"], "b");
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn automatic_raids_pick_a_template_and_a_point_inside_the_area() {
        let def: SpawnerDef = serde_json::from_str(r#"{"id":"z1","at":[100.0,20.0,-50.0],"area":40.0,"every":3600,
            "pickup":"c15a5c9d-4956-49ff-9d72-9f522229cf1c",
            "pool":[{"template":"aaaaaaaa-0000-0000-0000-000000000001","level":10,"count":2},
                    {"template":"aaaaaaaa-0000-0000-0000-000000000002","level":20,"count":1,"drops":[{"item":7,"guid":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"}]},
                    {"template":""}]}"#).unwrap();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let w = pick_wave(&def).unwrap();
            let at = w.at.unwrap();
            assert!(((at[0] - 100.0).powi(2) + (at[2] + 50.0).powi(2)).sqrt() <= 40.0001);
            assert!(w.once && w.pool.is_empty() && !w.template.is_empty());
            seen.insert(w.template.clone());
            std::thread::sleep(Duration::from_micros(100));
        }
        assert_eq!(seen.len(), 2);
        assert!(pick_wave(&SpawnerDef { at: None, ..def.clone() }).is_none());
        assert!(pick_wave(&SpawnerDef { pool: Vec::new(), ..def }).is_none());
    }

    #[test]
    fn every_zone_is_due_at_the_same_moment() {
        assert_eq!(secs_to_boundary(0, 1800), 1800);
        assert_eq!(secs_to_boundary(1799, 1800), 1);
        assert_eq!(secs_to_boundary(1800, 1800), 1800);
        for unix in [1_761_500_000u64, 1_761_500_333] {
            assert_eq!((unix + secs_to_boundary(unix, 1800)) % 1800, 0);
        }
        assert_eq!(secs_to_boundary(5, 0), 1);
    }

    #[test]
    fn a_wave_mixes_enemies_and_each_keeps_its_own_loot() {
        let def: SpawnerDef = serde_json::from_str(r#"{"id":"z1","at":[0.0,0.0,0.0],"area":10.0,"every":60,
            "pool":[{"name":"mix","members":[
                {"template":"aaaaaaaa-0000-0000-0000-000000000001","level":40,"count":5},
                {"template":"aaaaaaaa-0000-0000-0000-000000000002","level":45,"count":3,"drops":[{"item":7,"guid":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"}]},
                {"template":"aaaaaaaa-0000-0000-0000-000000000003","level":50,"count":1}]}]}"#).unwrap();
        let w = pick_wave(&def).unwrap();
        assert_eq!(w.parts.len(), 3);
        assert_eq!(w.count, 9);
        let mut rt = SpawnerRt { def: def.clone(), ids: Vec::new(), missing: HashMap::new(), respawn_at: None, spawned: 0, manager: 0, state: "x", was_active: false, check_at: None, last: HashMap::new(), done: false, wave: Some(w), next_wave: None, wave_started: None, part: 0, part_spawned: 0, owner: HashMap::new(), detached: Vec::new() };
        assert_eq!(rt.total(), 9);
        rt.owner.insert(11, 0);
        rt.owner.insert(12, 1);
        assert!(rt.drops_of(11).is_empty());
        assert_eq!(rt.drops_of(12).len(), 1);
        assert!(rt.has_loot());
        // the legacy single-enemy format still makes a one-enemy group
        let old: PoolEntry = serde_json::from_str(r#"{"template":"aaaaaaaa-0000-0000-0000-000000000001","level":3,"count":50}"#).unwrap();
        assert_eq!(old.group().len(), 1);
        assert_eq!(old.group()[0].count, 20);
    }

}
