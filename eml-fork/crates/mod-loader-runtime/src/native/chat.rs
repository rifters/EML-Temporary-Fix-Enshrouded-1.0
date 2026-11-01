//! Chat command channel: lets an admin drive the native layer from the in-game chat,
//! which is the only way to reach a rented dedicated server that offers no console.
//!
//! A message such as `/m182 <key> set m182_zones.json {...}` is swallowed (other players
//! never see it) and turned into the same files the desktop panel writes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const PREFIXES: [&str; 2] = ["/m182", "!m182"];
pub const MIN_KEY_LEN: usize = 6;

/// Files a chat command may write; each one is polled by the native layer.
pub const FILES: [&str; 8] = [
    "m182_spawn.request",
    "m182_command.request",
    "m182_powers.json",
    "m182_zones.json",
    "m182_teams.json",
    "m182_spawners.json",
    "m182_data.json",
    "m182_spawner_op.json",
];

pub const SPAWNER_OP_FILE: &str = "m182_spawner_op.json";
const SPAWNERS_FILE: &str = "m182_spawners.json";
const MAX_SPAWNERS: usize = 1000;

pub const DATA_FILE: &str = "m182_data.json";
const DATA_TARGET: [&str; 4] = ["mods", "zz_m182_item_stats", "src", "m182_data.lua"];
const DATA_LIMIT: usize = 80 * 1024;

const NATIVE_CONFIG: &str = "m182_native.json";
const MAX_BUFFER: usize = 96 * 1024;
const BUFFER_LIFE: Duration = Duration::from_secs(120);
const MAX_FAILURES: usize = 8;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Not ours: the game handles the message as usual.
    Pass,
    /// Ours: hide it from the chat. The text is for the log (never contains the key).
    Swallowed(String),
    /// Ours: the message is replaced by the list of connected players (sequence number echoed).
    Roster(u32),
    /// Ours: the message is replaced by the server's real teams (sequence number echoed).
    Teams(u32),
}

#[derive(Default)]
pub struct Session {
    buffers: HashMap<String, (String, Instant)>,
    failures: Vec<Instant>,
}

/// Chat client (plus one) that wrote the request waiting to be executed; 0 when none.
pub static REQUESTER: AtomicU32 = AtomicU32::new(0);

fn note_requester(file: &str, client: u32) {
    if file == "m182_spawn.request" || file == "m182_command.request" {
        REQUESTER.store(client.wrapping_add(1), Ordering::SeqCst);
    }
}

pub const POWERS_FILE: &str = "m182_powers.json";
pub const CLIENT_POWERS_PREFIX: &str = "m182_powers_";

/// Powers belong to the player that sent them, so each chat client gets a file of its own.
pub fn client_powers_file(client: u32) -> String {
    format!("{CLIENT_POWERS_PREFIX}{client}.json")
}

pub fn client_of_powers_file(name: &str) -> Option<u32> {
    name.strip_prefix(CLIENT_POWERS_PREFIX)?.strip_suffix(".json")?.parse().ok()
}

fn destination(name: &str, client: u32) -> String {
    if name == POWERS_FILE { client_powers_file(client) } else { name.to_string() }
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

static OTHER_SEEN: AtomicU32 = AtomicU32::new(0);

/// Logs the first few ordinary messages (their first characters only) so a silent
/// channel can be told apart from messages arriving in an unexpected shape.
pub fn note_other(message: &str, client: u32) {
    if OTHER_SEEN.fetch_add(1, Ordering::Relaxed) < 6 {
        let head: String = message.chars().take(6).collect();
        #[cfg(windows)]
        super::spawner::log(&format!("chat: client {client} sent an ordinary message ({} chars) starting \"{head}\"", message.chars().count()));
    }
}

pub fn has_prefix(text: &str) -> bool {
    let t = text.trim_start();
    PREFIXES.iter().any(|p| {
        t.len() >= p.len()
            && t.is_char_boundary(p.len())
            && t[..p.len()].eq_ignore_ascii_case(p)
            && t[p.len()..].chars().next().is_none_or(char::is_whitespace)
    })
}

pub fn valid_key(key: &str) -> bool {
    key.len() >= MIN_KEY_LEN && !key.chars().any(char::is_whitespace)
}

pub fn handle(text: &str, key: &str, client: u32, dir: &Path) -> Verdict {
    let Ok(mut guard) = SESSION.lock() else { return Verdict::Pass };
    let session = guard.get_or_insert_with(Session::default);
    handle_with(session, Instant::now(), text, key, client, dir)
}

fn next_token(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], s[i..].trim_start()),
        None => (s, ""),
    }
}

fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

pub fn handle_with(session: &mut Session, now: Instant, text: &str, key: &str, client: u32, dir: &Path) -> Verdict {
    if !valid_key(key) || !has_prefix(text) {
        return Verdict::Pass;
    }
    let t = text.trim_start();
    let (_, rest) = next_token(t);
    let (given, rest) = next_token(rest);

    session.failures.retain(|at| now.duration_since(*at) < FAILURE_WINDOW);
    if session.failures.len() >= MAX_FAILURES {
        return Verdict::Swallowed(format!("chat: client {client} ignored (too many wrong keys)"));
    }
    if !same(given, key) {
        session.failures.push(now);
        return Verdict::Swallowed(format!("chat: client {client} used a wrong key"));
    }

    session.buffers.retain(|_, (_, at)| now.duration_since(*at) < BUFFER_LIFE);

    let (command, rest) = next_token(rest);
    let lowered = command.to_ascii_lowercase();
    let (command, rest) = match lowered.as_str() {
        "setx" | "putx" | "addx" | "endx" => {
            let (file, hex) = next_token(rest);
            match unhex(hex) {
                Ok(body) => (lowered[..3].to_string(), format!("{file} {body}")),
                Err(why) => return Verdict::Swallowed(format!("chat: client {client} rejected: {why}")),
            }
        }
        _ => (lowered.clone(), rest.to_string()),
    };
    let rest = rest.as_str();
    if command == "players" {
        let (seq, _) = next_token(rest);
        return Verdict::Roster(seq.parse().unwrap_or(0));
    }
    if command == "teams" {
        let (seq, _) = next_token(rest);
        return Verdict::Teams(seq.parse().unwrap_or(0));
    }
    let outcome = match command.as_str() {
        "ping" => Ok("ping".to_string()),
        "mark" => {
            super::diag::mark(rest);
            Ok("mark".to_string())
        }
        "set" => {
            let (file, body) = next_token(rest);
            let written = write_json(dir, file, body, client);
            if written.is_ok() {
                note_requester(file, client);
            }
            written
        }
        "put" => {
            let (file, chunk) = next_token(rest);
            match allowed(file) {
                Some(name) if chunk.len() <= MAX_BUFFER => {
                    session.buffers.insert(name.to_string(), (chunk.to_string(), now));
                    Ok(format!("put {name} ({} bytes buffered)", chunk.len()))
                }
                Some(_) => Err("chunk too large".to_string()),
                None => Err(format!("file \"{file}\" is not allowed")),
            }
        }
        "add" => {
            let (file, chunk) = next_token(rest);
            match allowed(file) {
                Some(name) => match session.buffers.get_mut(name) {
                    Some((buffer, at)) if buffer.len() + chunk.len() <= MAX_BUFFER => {
                        buffer.push_str(chunk);
                        *at = now;
                        Ok(format!("add {name} ({} bytes buffered)", buffer.len()))
                    }
                    Some(_) => Err("buffer too large".to_string()),
                    None => Err(format!("nothing buffered for {name}; send put first")),
                },
                None => Err(format!("file \"{file}\" is not allowed")),
            }
        }
        "end" => {
            let (file, _) = next_token(rest);
            match allowed(file) {
                Some(name) => match session.buffers.remove(name) {
                    Some((buffer, _)) => {
                        let written = write_json(dir, name, &buffer, client);
                        if written.is_ok() {
                            note_requester(name, client);
                        }
                        written
                    }
                    None => Err(format!("nothing buffered for {name}")),
                },
                None => Err(format!("file \"{file}\" is not allowed")),
            }
        }
        "setz" => {
            let (file, packed) = next_token(rest);
            match unpack(packed) {
                Ok(body) => {
                    let written = write_json(dir, file, &body, client);
                    if written.is_ok() {
                        note_requester(file, client);
                    }
                    written
                }
                Err(why) => Err(why),
            }
        }
        "putz" => {
            let (file, chunk) = next_token(rest);
            match allowed(file) {
                Some(name) if chunk.len() <= MAX_BUFFER => {
                    session.buffers.insert(name.to_string(), (chunk.to_string(), now));
                    Ok(format!("putz {name} ({} bytes buffered)", chunk.len()))
                }
                Some(_) => Err("chunk too large".to_string()),
                None => Err(format!("file \"{file}\" is not allowed")),
            }
        }
        "addz" => {
            let (file, chunk) = next_token(rest);
            match allowed(file) {
                Some(name) => match session.buffers.get_mut(name) {
                    Some((buffer, at)) if buffer.len() + chunk.len() <= MAX_BUFFER => {
                        buffer.push_str(chunk);
                        *at = now;
                        Ok(format!("addz {name} ({} bytes buffered)", buffer.len()))
                    }
                    Some(_) => Err("buffer too large".to_string()),
                    None => Err(format!("nothing buffered for {name}; send putz first")),
                },
                None => Err(format!("file \"{file}\" is not allowed")),
            }
        }
        "endz" => {
            let (file, _) = next_token(rest);
            match allowed(file) {
                Some(name) => match session.buffers.remove(name) {
                    Some((buffer, _)) => match unpack(&buffer) {
                        Ok(body) => {
                            let written = write_json(dir, name, &body, client);
                            if written.is_ok() {
                                note_requester(name, client);
                            }
                            written
                        }
                        Err(why) => Err(why),
                    },
                    None => Err(format!("nothing buffered for {name}")),
                },
                None => Err(format!("file \"{file}\" is not allowed")),
            }
        }
        "pvp" => {
            let (value, _) = next_token(rest);
            match value.to_ascii_lowercase().as_str() {
                "on" | "true" | "1" => set_pvp(dir, true),
                "off" | "false" | "0" => set_pvp(dir, false),
                _ => Err("pvp needs on or off".to_string()),
            }
        }
        "friendlyfire" => {
            let (value, _) = next_token(rest);
            match value.to_ascii_lowercase().as_str() {
                "on" | "true" | "1" => set_flag(dir, "pvp_friendly_fire", "friendlyfire", true),
                "off" | "false" | "0" => set_flag(dir, "pvp_friendly_fire", "friendlyfire", false),
                _ => Err("friendlyfire needs on or off".to_string()),
            }
        }
        "" => Err("missing command".to_string()),
        other => Err(format!("unknown command \"{other}\"")),
    };

    Verdict::Swallowed(match outcome {
        Ok(done) => format!("chat: client {client} ok: {done}"),
        Err(why) => format!("chat: client {client} rejected: {why}"),
    })
}

const MAX_UNPACKED: usize = 512 * 1024;

fn unb64(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4 + 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes().filter(|b| !b.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return Err("packed payload has an invalid character".to_string()),
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// Inverse of the panel's `compose_z`: base64 (url alphabet) of a deflate stream, holding JSON text.
fn unpack(text: &str) -> Result<String, String> {
    let raw = unb64(text)?;
    let bytes = miniz_oxide::inflate::decompress_to_vec_with_limit(&raw, MAX_UNPACKED).map_err(|_| "packed payload is damaged".to_string())?;
    String::from_utf8(bytes).map_err(|_| "packed payload is not text".to_string())
}

fn unhex(text: &str) -> Result<String, String> {
    let digits: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.len() % 2 != 0 {
        return Err("hex payload has an odd length".to_string());
    }
    let mut bytes = Vec::with_capacity(digits.len() / 2);
    for pair in digits.chunks(2) {
        let hi = (pair[0] as char).to_digit(16);
        let lo = (pair[1] as char).to_digit(16);
        match (hi, lo) {
            (Some(h), Some(l)) => bytes.push((h * 16 + l) as u8),
            _ => return Err("hex payload has a non-hex character".to_string()),
        }
    }
    String::from_utf8(bytes).map_err(|_| "hex payload is not text".to_string())
}

fn allowed(file: &str) -> Option<&'static str> {
    FILES.iter().copied().find(|f| f.eq_ignore_ascii_case(file))
}

fn write_json(dir: &Path, file: &str, body: &str, client: u32) -> Result<String, String> {
    let name = allowed(file).ok_or_else(|| format!("file \"{file}\" is not allowed"))?;
    if body.trim().is_empty() {
        return Err("empty content".to_string());
    }
    let value = serde_json::from_str::<serde_json::Value>(body).map_err(|e| format!("invalid JSON for {name}: {e}"))?;
    if name == DATA_FILE {
        return write_data(dir, &value);
    }
    if name == SPAWNER_OP_FILE {
        return apply_spawner_op(dir, &value);
    }
    let target = destination(name, client);
    store(dir, &target, body)?;
    Ok(format!("wrote {target} ({} bytes)", body.len()))
}

fn spawner_field<'a>(row: &'a serde_json::Value, short: &str, long: &str) -> Option<&'a serde_json::Value> {
    row.get(short).or_else(|| row.get(long))
}

fn expand_drops(list: &[serde_json::Value]) -> Vec<serde_json::Value> {
    use serde_json::{json, Value};
    let mut drops = Vec::new();
        for d in list.iter().take(8) {
            // wire: [item, guid, amount, rarity, level, chance]; long: an object
            let (item, guid, amount, rarity, level, chance) = match d {
                Value::Array(a) if a.len() >= 2 => (
                    a[0].as_u64(),
                    a[1].as_str().map(str::to_string),
                    a.get(2).and_then(Value::as_u64).unwrap_or(1),
                    a.get(3).and_then(Value::as_u64).unwrap_or(0),
                    a.get(4).and_then(Value::as_u64).unwrap_or(1),
                    a.get(5).and_then(Value::as_f64).unwrap_or(100.0),
                ),
                Value::Object(o) => (
                    o.get("item").and_then(Value::as_u64),
                    o.get("guid").and_then(Value::as_str).map(str::to_string),
                    o.get("amount").and_then(Value::as_u64).unwrap_or(1),
                    o.get("rarity").and_then(Value::as_u64).unwrap_or(0),
                    o.get("level").and_then(Value::as_u64).unwrap_or(1),
                    o.get("chance").and_then(Value::as_f64).unwrap_or(100.0),
                ),
                _ => continue,
            };
            let (Some(item), Some(guid)) = (item, guid) else { continue };
            let hex: String = guid.chars().filter(|c| *c != '-').collect();
            if item == 0 || item > u32::MAX as u64 || hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            let guid = format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]).to_ascii_lowercase();
            drops.push(json!({ "item": item, "guid": guid, "amount": amount.clamp(1, 65535), "rarity": rarity.min(4), "level": level.clamp(1, 255), "chance": chance.clamp(0.0, 100.0) }));
        }
    drops
}

/// One enemy of a raid template: template id, level, amount, health, damage and loot.
fn expand_member(e: &serde_json::Value) -> Option<serde_json::Value> {
    use serde_json::{json, Value};
    let raw = spawner_field(e, "t", "template").and_then(Value::as_str)?;
    let hex: String = raw.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let template = format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]).to_ascii_lowercase();
    let mut entry = serde_json::Map::new();
    entry.insert("template".into(), json!(template));
    for (short, long) in [("l", "level"), ("c", "count"), ("h", "health"), ("d", "damage")] {
        if let Some(v) = spawner_field(e, short, long).filter(|v| v.is_number()) {
            entry.insert(long.into(), v.clone());
        }
    }
    if let Some(l) = spawner_field(e, "k", "drops").and_then(Value::as_array) {
        let d = expand_drops(l);
        if !d.is_empty() {
            entry.insert("drops".into(), Value::Array(d));
        }
    }
    Some(Value::Object(entry))
}

/// One spawner as the panel sends it (short keys, template without dashes) turned into the row
/// the spawner file holds.
fn expand_spawner(row: &serde_json::Value) -> Result<serde_json::Value, String> {
    use serde_json::{json, Map, Value};
    let id = spawner_field(row, "i", "id").and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= 40).ok_or("spawner without id")?;
    let raw = spawner_field(row, "t", "template").and_then(Value::as_str).ok_or("spawner without template")?;
    let hex: String = raw.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("spawner {id}: bad template"));
    }
    let template = format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]).to_ascii_lowercase();
    let at = spawner_field(row, "p", "at").and_then(Value::as_array).filter(|a| a.len() == 3 && a.iter().all(|n| n.as_f64().is_some_and(f64::is_finite))).ok_or_else(|| format!("spawner {id}: bad position"))?;
    let mut out = Map::new();
    out.insert("id".into(), json!(id));
    out.insert("template".into(), json!(template));
    out.insert("at".into(), json!(at));
    for (short, long) in [("l", "level"), ("c", "count"), ("h", "health"), ("d", "damage"), ("r", "radius"), ("w", "respawn")] {
        if let Some(v) = spawner_field(row, short, long).filter(|v| v.is_number()) {
            out.insert(long.into(), v.clone());
        }
    }
    if let Some(v) = spawner_field(row, "o", "once") {
        if v.as_bool().unwrap_or_else(|| v.as_u64().unwrap_or(0) != 0) {
            out.insert("once".into(), json!(true));
        }
    }
    if let Some(raw) = spawner_field(row, "q", "pickup").and_then(Value::as_str) {
        let hex: String = raw.chars().filter(|c| *c != '-').collect();
        if hex.len() == 32 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            out.insert("pickup".into(), json!(format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]).to_ascii_lowercase()));
        }
    }
    if let Some(list) = spawner_field(row, "k", "drops").and_then(Value::as_array) {
        let drops = expand_drops(list);
        if !drops.is_empty() {
            out.insert("drops".into(), Value::Array(drops));
        }
    }
    if let Some(v) = spawner_field(row, "e", "every").filter(|v| v.is_number()) {
        out.insert("every".into(), v.clone());
    }
    if let Some(v) = spawner_field(row, "a", "area").filter(|v| v.is_number()) {
        out.insert("area".into(), v.clone());
    }
    if let Some(list) = spawner_field(row, "m", "pool").and_then(Value::as_array) {
        let mut pool = Vec::new();
        for e in list.iter().take(10) {
            let mut entry = serde_json::Map::new();
            let mut members = Vec::new();
            if let Some(group) = spawner_field(e, "u", "members").and_then(Value::as_array) {
                for m in group.iter().take(10) {
                    if let Some(member) = expand_member(m) {
                        members.push(member);
                    }
                }
            }
            if members.is_empty() {
                match expand_member(e) {
                    Some(member) => members.push(member),
                    None => continue,
                }
            }
            entry.insert("members".into(), Value::Array(members));
            pool.push(Value::Object(entry));
        }
        if !pool.is_empty() {
            out.insert("pool".into(), Value::Array(pool));
        }
    }
    Ok(Value::Object(out))
}

/// Changes the spawner list without resending it: `{"add":{..}}`, `{"rm":"id"}` or
/// `{"set":[..]}` (replace everything), applied in the order set, add, del.
fn apply_spawner_op(dir: &Path, op: &serde_json::Value) -> Result<String, String> {
    use serde_json::{json, Value};
    let path = dir.join(SPAWNERS_FILE);
    let mut rows: Vec<Value> = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("spawners").and_then(Value::as_array).cloned())
            .ok_or_else(|| format!("{SPAWNERS_FILE} is not readable; send the whole list instead"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("cannot read {SPAWNERS_FILE}: {e}")),
    };
    let id_of = |r: &Value| r.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let many = |v: &Value| -> Vec<Value> { v.as_array().cloned().unwrap_or_else(|| vec![v.clone()]) };
    let (mut added, mut removed) = (0usize, 0usize);
    if let Some(set) = op.get("set") {
        rows.clear();
        for row in many(set) {
            rows.push(expand_spawner(&row)?);
            added += 1;
        }
    }
    if let Some(add) = op.get("add") {
        for row in many(add) {
            let row = expand_spawner(&row)?;
            let id = id_of(&row);
            match rows.iter().position(|r| id_of(r) == id) {
                Some(i) => rows[i] = row,
                None => rows.push(row),
            }
            added += 1;
        }
    }
    if let Some(del) = op.get("rm").or_else(|| op.get("del")) {
        for id in many(del).iter().filter_map(Value::as_str) {
            let before = rows.len();
            rows.retain(|r| id_of(r) != id);
            removed += before - rows.len();
        }
    }
    if rows.len() > MAX_SPAWNERS {
        return Err(format!("too many spawners ({})", rows.len()));
    }
    let text = json!({ "spawners": rows }).to_string();
    store(dir, SPAWNERS_FILE, &text)?;
    Ok(format!("{SPAWNERS_FILE}: {} spawner(s) now ({added} added or replaced, {removed} removed)", rows.len()))
}

fn write_data(dir: &Path, value: &serde_json::Value) -> Result<String, String> {
    let lua = value.get("lua").and_then(|v| v.as_str()).ok_or_else(|| "m182_data.json needs a \"lua\" text".to_string())?;
    if lua.len() > DATA_LIMIT {
        return Err(format!("m182_data.json is too large ({} bytes)", lua.len()));
    }
    if !lua.trim_start().starts_with("return") || lua.contains('\0') {
        return Err("m182_data.json does not hold a data table".to_string());
    }
    let mut target = dir.to_path_buf();
    for part in &DATA_TARGET[..3] {
        target.push(part);
    }
    std::fs::create_dir_all(&target).map_err(|e| format!("cannot create the mod folder: {e}"))?;
    let name = DATA_TARGET[3];
    store(&target, name, lua)?;
    Ok(format!("wrote {} ({} bytes); it applies when the server starts", DATA_TARGET.join("/"), lua.len()))
}

fn store(dir: &Path, name: &str, body: &str) -> Result<(), String> {
    let temp = dir.join(format!("{name}.chat.tmp"));
    std::fs::write(&temp, body).map_err(|e| format!("cannot write {name}: {e}"))?;
    std::fs::rename(&temp, dir.join(name)).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("cannot replace {name}: {e}")
    })
}

fn set_pvp(dir: &Path, on: bool) -> Result<String, String> {
    set_flag(dir, "pvp", "pvp", on)
}

fn set_flag(dir: &Path, key: &str, label: &str, on: bool) -> Result<String, String> {
    let path = dir.join(NATIVE_CONFIG);
    let mut value = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text).map_err(|e| format!("{NATIVE_CONFIG} is invalid: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(format!("cannot read {NATIVE_CONFIG}: {e}")),
    };
    let Some(map) = value.as_object_mut() else { return Err(format!("{NATIVE_CONFIG} is not an object")) };
    map.insert(key.to_string(), serde_json::Value::Bool(on));
    let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    store(dir, NATIVE_CONFIG, &text)?;
    Ok(format!("{label} {}", if on { "on" } else { "off" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("m182_chat_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run(session: &mut Session, dir: &Path, text: &str) -> Verdict {
        handle_with(session, Instant::now(), text, "secret-key", 7, dir)
    }

    #[test]
    fn hex_commands_carry_json_without_special_characters() {
        let dir = scratch("hex");
        let mut s = Session::default();
        let hex = |t: &str| t.bytes().map(|b| format!("{b:02x}")).collect::<String>();
        let body = "{\"a\":[1,2,3]}";
        let v = run(&mut s, &dir, &format!("!m182 secret-key setx m182_zones.json {}", hex(body)));
        assert!(matches!(v, Verdict::Swallowed(ref l) if l.contains("ok")));
        assert_eq!(std::fs::read_to_string(dir.join("m182_zones.json")).unwrap(), body);
        let (a, b) = body.split_at(5);
        run(&mut s, &dir, &format!("!m182 secret-key putx m182_teams.json {}", hex(a)));
        run(&mut s, &dir, &format!("!m182 secret-key addx m182_teams.json {}", hex(b)));
        run(&mut s, &dir, "!m182 secret-key endx m182_teams.json");
        assert_eq!(std::fs::read_to_string(dir.join("m182_teams.json")).unwrap(), body);
        let Verdict::Swallowed(line) = run(&mut s, &dir, "!m182 secret-key setx m182_powers.json zz") else { panic!() };
        assert!(line.contains("rejected"));
    }

    #[test]
    fn spawner_changes_arrive_one_at_a_time() {
        let dir = scratch("ops");
        let mut s = Session::default();
        let send = |s: &mut Session, body: &str| {
            let hex: String = body.bytes().map(|b| format!("{b:02x}")).collect();
            run(s, &dir, &format!("!m182 secret-key setx m182_spawner_op.json {hex}"))
        };
        let list = |dir: &Path| -> serde_json::Value { serde_json::from_str(&std::fs::read_to_string(dir.join("m182_spawners.json")).unwrap()).unwrap() };
        let a = r#"{"add":{"i":"s1","t":"333ea93f04f64bd2b0031582429dfb66","p":[10.5,20.0,30.25],"l":20,"c":5,"h":1800,"d":1.1,"r":4.0,"w":300}}"#;
        assert!(matches!(send(&mut s, a), Verdict::Swallowed(ref l) if l.contains("1 spawner(s) now")));
        let v = list(&dir);
        let row = &v["spawners"][0];
        assert_eq!(row["id"], "s1");
        assert_eq!(row["template"], "333ea93f-04f6-4bd2-b003-1582429dfb66");
        assert_eq!(row["at"], serde_json::json!([10.5, 20.0, 30.25]));
        assert_eq!((row["level"].as_u64(), row["count"].as_u64(), row["health"].as_u64(), row["respawn"].as_u64()), (Some(20), Some(5), Some(1800), Some(300)));
        assert_eq!(row["radius"].as_f64(), Some(4.0));
        send(&mut s, r#"{"add":{"i":"s2","t":"c3db66d6-89a5-4a6c-a8e3-d9a1f6009812","p":[1,2,3]}}"#);
        send(&mut s, r#"{"add":{"i":"s1","t":"333ea93f04f64bd2b0031582429dfb66","p":[11,20,30],"l":21}}"#);
        let v = list(&dir);
        assert_eq!(v["spawners"].as_array().unwrap().len(), 2);
        assert_eq!(v["spawners"][0]["level"], 21);
        assert!(matches!(send(&mut s, r#"{"del":"s1"}"#), Verdict::Swallowed(ref l) if l.contains("1 removed")));
        assert_eq!(list(&dir)["spawners"].as_array().unwrap().len(), 1);
        send(&mut s, r#"{"set":[{"i":"a","t":"333ea93f04f64bd2b0031582429dfb66","p":[0,0,0]},{"i":"b","t":"333ea93f04f64bd2b0031582429dfb66","p":[0,0,0]}]}"#);
        send(&mut s, r#"{"add":[{"i":"c","t":"333ea93f04f64bd2b0031582429dfb66","p":[0,0,0]}],"del":["a"]}"#);
        let ids: Vec<String> = list(&dir)["spawners"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
        assert_eq!(ids, ["b", "c"]);
        let Verdict::Swallowed(line) = send(&mut s, r#"{"add":{"i":"x","t":"nothex","p":[0,0,0]}}"#) else { panic!() };
        assert!(line.contains("rejected") && line.contains("bad template"));
        std::fs::write(dir.join("m182_spawners.json"), "not json").unwrap();
        let Verdict::Swallowed(line) = send(&mut s, r#"{"del":"b"}"#) else { panic!() };
        assert!(line.contains("rejected"), "{line}");
        assert_eq!(std::fs::read_to_string(dir.join("m182_spawners.json")).unwrap(), "not json");
    }

    #[test]
    fn ordinary_chat_is_left_alone() {
        let dir = scratch("plain");
        let mut s = Session::default();
        assert_eq!(run(&mut s, &dir, "hola a todos"), Verdict::Pass);
        assert_eq!(run(&mut s, &dir, "/m1823 secret-key ping"), Verdict::Pass);
        assert_eq!(run(&mut s, &dir, "/other secret-key ping"), Verdict::Pass);
    }

    #[test]
    fn nothing_is_intercepted_without_a_usable_key() {
        let dir = scratch("nokey");
        let mut s = Session::default();
        assert_eq!(handle_with(&mut s, Instant::now(), "/m182 x ping", "", 1, &dir), Verdict::Pass);
        assert_eq!(handle_with(&mut s, Instant::now(), "/m182 abc ping", "abc", 1, &dir), Verdict::Pass);
    }

    #[test]
    fn wrong_key_is_hidden_and_never_echoed() {
        let dir = scratch("wrong");
        let mut s = Session::default();
        let Verdict::Swallowed(line) = run(&mut s, &dir, "/m182 guess-key set m182_zones.json {}") else { panic!() };
        assert!(line.contains("wrong key"));
        assert!(!line.contains("guess-key"));
        assert!(!dir.join("m182_zones.json").exists());
    }

    #[test]
    fn repeated_wrong_keys_lock_the_channel_for_a_while() {
        let dir = scratch("lock");
        let mut s = Session::default();
        let start = Instant::now();
        for _ in 0..MAX_FAILURES {
            handle_with(&mut s, start, "/m182 nope-nope ping", "secret-key", 3, &dir);
        }
        let Verdict::Swallowed(line) = handle_with(&mut s, start, "/m182 secret-key set m182_zones.json {}", "secret-key", 3, &dir) else { panic!() };
        assert!(line.contains("too many"));
        assert!(!dir.join("m182_zones.json").exists());
        let later = start + FAILURE_WINDOW + Duration::from_secs(1);
        let Verdict::Swallowed(line) = handle_with(&mut s, later, "/m182 secret-key set m182_zones.json {\"a\":1}", "secret-key", 3, &dir) else { panic!() };
        assert!(line.contains("wrote"), "{line}");
    }

    #[test]
    fn set_writes_valid_json_to_allowed_files_only() {
        let dir = scratch("set");
        let mut s = Session::default();
        let Verdict::Swallowed(line) = run(&mut s, &dir, "!M182 secret-key SET m182_zones.json {\"zones\": []}") else { panic!() };
        assert!(line.contains("wrote m182_zones.json"), "{line}");
        assert_eq!(std::fs::read_to_string(dir.join("m182_zones.json")).unwrap(), "{\"zones\": []}");

        let Verdict::Swallowed(line) = run(&mut s, &dir, "/m182 secret-key set evil.dll {}") else { panic!() };
        assert!(line.contains("not allowed"));
        let Verdict::Swallowed(line) = run(&mut s, &dir, "/m182 secret-key set ../m182_zones.json {}") else { panic!() };
        assert!(line.contains("not allowed"));
        let Verdict::Swallowed(line) = run(&mut s, &dir, "/m182 secret-key set m182_teams.json {broken") else { panic!() };
        assert!(line.contains("invalid JSON"));
        assert!(!dir.join("m182_teams.json").exists());
        assert!(!dir.join("evil.dll").exists());
    }

    #[test]
    fn long_content_is_assembled_from_parts() {
        let dir = scratch("parts");
        let mut s = Session::default();
        run(&mut s, &dir, "/m182 secret-key put m182_teams.json {\"teams\": [[1,2],");
        let Verdict::Swallowed(line) = run(&mut s, &dir, "/m182 secret-key end m182_teams.json") else { panic!() };
        assert!(line.contains("invalid JSON"), "{line}");
        assert!(!dir.join("m182_teams.json").exists());

        run(&mut s, &dir, "/m182 secret-key put m182_teams.json {\"teams\": [[1,2],");
        run(&mut s, &dir, "/m182 secret-key add m182_teams.json [3,4]]}");
        let Verdict::Swallowed(line) = run(&mut s, &dir, "/m182 secret-key end m182_teams.json") else { panic!() };
        assert!(line.contains("wrote"), "{line}");
        let text = std::fs::read_to_string(dir.join("m182_teams.json")).unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&text).unwrap()["teams"][1][1], 4);
        let Verdict::Swallowed(line) = run(&mut s, &dir, "/m182 secret-key end m182_teams.json") else { panic!() };
        assert!(line.contains("nothing buffered"));
    }

    #[test]
    fn buffers_expire() {
        let dir = scratch("expire");
        let mut s = Session::default();
        let t0 = Instant::now();
        handle_with(&mut s, t0, "/m182 secret-key put m182_zones.json {", "secret-key", 1, &dir);
        let Verdict::Swallowed(line) = handle_with(&mut s, t0 + BUFFER_LIFE + Duration::from_secs(1), "/m182 secret-key add m182_zones.json }", "secret-key", 1, &dir) else { panic!() };
        assert!(line.contains("nothing buffered"), "{line}");
    }

    #[test]
    fn pvp_edits_only_that_field_of_the_native_config() {
        let dir = scratch("pvp");
        std::fs::write(dir.join(NATIVE_CONFIG), "{\"spawner\": true, \"pvp\": false, \"spawn_level\": 30}").unwrap();
        let mut s = Session::default();
        let Verdict::Swallowed(line) = run(&mut s, &dir, "/m182 secret-key pvp on") else { panic!() };
        assert!(line.contains("pvp on"), "{line}");
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join(NATIVE_CONFIG)).unwrap()).unwrap();
        assert_eq!(v["pvp"], true);
        assert_eq!(v["spawner"], true);
        assert_eq!(v["spawn_level"], 30);
        let Verdict::Swallowed(line) = run(&mut s, &dir, "/m182 secret-key pvp maybe") else { panic!() };
        assert!(line.contains("on or off"));
    }

    #[test]
    fn friendly_fire_edits_only_its_own_field() {
        let dir = scratch("friendly_fire");
        std::fs::write(dir.join(NATIVE_CONFIG), "{\"pvp\": true, \"spawn_level\": 30}").unwrap();
        let mut s = Session::default();
        let Verdict::Swallowed(line) = run(&mut s, &dir, "/m182 secret-key friendlyfire on") else { panic!() };
        assert!(line.contains("friendlyfire on"), "{line}");
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join(NATIVE_CONFIG)).unwrap()).unwrap();
        assert_eq!(v["pvp_friendly_fire"], true);
        assert_eq!(v["pvp"], true);
        assert_eq!(v["spawn_level"], 30);
        let Verdict::Swallowed(line) = run(&mut s, &dir, "/m182 secret-key friendlyfire maybe") else { panic!() };
        assert!(line.contains("on or off"));
    }

    #[test]
    fn ping_and_unknown_commands_are_answered_in_the_log() {
        let dir = scratch("misc");
        let mut s = Session::default();
        assert_eq!(run(&mut s, &dir, "/m182 secret-key ping"), Verdict::Swallowed("chat: client 7 ok: ping".into()));
        let Verdict::Swallowed(line) = run(&mut s, &dir, "/m182 secret-key dance") else { panic!() };
        assert!(line.contains("unknown command"));
        let Verdict::Swallowed(line) = run(&mut s, &dir, "/m182 secret-key") else { panic!() };
        assert!(line.contains("missing command"));
    }
    #[test]
    fn raids_and_drops_survive_the_wire() {
        let row = serde_json::json!({"i":"r1","t":"333ea93f04f64bd2b0031582429dfb66","p":[1,2,3],"o":1,"c":4,
            "k":[[77,"aaaaaaaabbbbccccddddeeeeeeeeeeee",3,2,10,25.0],[0,"aaaaaaaabbbbccccddddeeeeeeeeeeee"],[5,"bad"]],
            "q":"c15a5c9d495649ff9d729f522229cf1c"});
        let out = expand_spawner(&row).unwrap();
        assert_eq!(out["once"], true);
        assert_eq!(out["pickup"], "c15a5c9d-4956-49ff-9d72-9f522229cf1c");
        let drops = out["drops"].as_array().unwrap();
        assert_eq!(drops.len(), 1);
        assert_eq!(drops[0]["item"], 77);
        assert_eq!(drops[0]["guid"], "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
        assert_eq!(drops[0]["amount"], 3);
        assert_eq!(drops[0]["chance"], 25.0);
        let plain = expand_spawner(&serde_json::json!({"i":"a","t":"333ea93f04f64bd2b0031582429dfb66","p":[0,0,0]})).unwrap();
        assert!(plain.get("once").is_none() && plain.get("drops").is_none());
    }

    #[test]
    fn automatic_raid_pools_survive_the_wire() {
        let row = serde_json::json!({"i":"z","t":"333ea93f04f64bd2b0031582429dfb66","p":[1,2,3],"e":3600,"a":40,
            "m":[{"t":"333ea93f04f64bd2b0031582429dfb66","l":10,"c":2,"h":0,"d":1.5,"k":[[7,"aaaaaaaabbbbccccddddeeeeeeeeeeee",1,0,1,50]]},{"t":"bad"}]});
        let out = expand_spawner(&row).unwrap();
        assert_eq!(out["every"], 3600);
        assert_eq!(out["area"], 40);
        let pool = out["pool"].as_array().unwrap();
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0]["members"][0]["template"], "333ea93f-04f6-4bd2-b003-1582429dfb66");
        assert_eq!(pool[0]["members"][0]["count"], 2);
        assert_eq!(pool[0]["members"][0]["drops"][0]["item"], 7);
    }

    #[test]
    fn a_template_can_mix_several_enemies() {
        let row = serde_json::json!({"i":"z","t":"333ea93f04f64bd2b0031582429dfb66","p":[1,2,3],"e":60,"a":40,
            "m":[{"u":[{"t":"333ea93f04f64bd2b0031582429dfb66","l":40,"c":5},
                       {"t":"642098d4fc0948e2811e782d438bf551","l":45,"c":3,"k":[[9,"aaaaaaaabbbbccccddddeeeeeeeeeeee",1,0,1,30]]},
                       {"t":"nope","c":9}]}]});
        let out = expand_spawner(&row).unwrap();
        let members = out["pool"][0]["members"].as_array().unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0]["level"], 40);
        assert_eq!(members[1]["count"], 3);
        assert_eq!(members[1]["drops"][0]["chance"], 30.0);
    }


    #[test]
    fn packed_commands_from_the_panel_are_unpacked() {
        let dir = scratch("packed");
        let mut s = Session::default();
        let line = "!m182 secret-key setz m182_zones.json lcqxCYAwEEDRVeTXEaJ2V7qGWBx4hYURDIIkZChncDGzQqrXvEw6g0VkyQQ9DGHWaN334niQwTsS0o_VS7f9rnXyxTXttfw";
        assert!(matches!(run(&mut s, &dir, line), Verdict::Swallowed(ref l) if l.contains("ok")));
        let body = std::fs::read_to_string(dir.join("m182_zones.json")).unwrap();
        assert_eq!(body, r#"{"zones":[{"name":"Base ñ","x":10,"z":-20,"radius":30},{"name":"Base ñ","x":10,"z":-20,"radius":30}]}"#);
        let packed = line.rsplit(' ').next().unwrap();
        let (a, b) = packed.split_at(40);
        run(&mut s, &dir, &format!("!m182 secret-key putz m182_teams.json {a}"));
        run(&mut s, &dir, &format!("!m182 secret-key addz m182_teams.json {b}"));
        run(&mut s, &dir, "!m182 secret-key endz m182_teams.json");
        assert_eq!(std::fs::read_to_string(dir.join("m182_teams.json")).unwrap(), body);
        let Verdict::Swallowed(bad) = run(&mut s, &dir, "!m182 secret-key setz m182_powers.json AAAA") else { panic!() };
        assert!(bad.contains("rejected"));
    }
}
