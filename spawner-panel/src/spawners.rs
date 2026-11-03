//! Persistent creature spawners: a creature tied to a saved place that comes back by itself
//! (every few minutes after it dies, and after every restart of the game).

use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::write_atomic;

pub const SPAWNERS_FILE: &str = "m182_spawners.json";
pub const OP_FILE: &str = "m182_spawner_op.json";
pub const STATUS_FILE: &str = "m182_spawner_status.json";
pub const DEFAULT_RESPAWN_MINUTES: u32 = 5;

#[derive(Clone, Debug, PartialEq)]
pub struct Spawner {
    pub id: String,
    pub creature: String,
    pub template: String,
    pub place: String,
    pub at: [f64; 3],
    pub level: u32,
    pub count: u32,
    pub health: u32,
    pub damage: f32,
    pub radius: f32,
    pub respawn: u32,
    /// A raid or event: created once, gone when its creatures are dead.
    pub once: bool,
    /// Items that fall where each creature dies.
    pub drops: Vec<Drop>,
    /// Pickup template that carries the drops.
    pub pickup: String,
    /// Automatic raids: every `every` seconds one of the templates appears at a random point
    /// within `area` meters of `at`.
    pub pool: Vec<RaidTemplate>,
    pub every: u32,
    pub area: f32,
}

pub const MAX_TEMPLATES: usize = 10;
pub const TEMPLATES_FILE: &str = "m182_raid_templates.json";

/// One kind of enemy inside a raid template, with its own amount, level and loot.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub creature: String,
    pub template: String,
    pub level: u32,
    pub count: u32,
    pub health: u32,
    pub damage: f32,
    pub drops: Vec<Drop>,
}

/// A raid template: a group of enemies that appear together.
#[derive(Clone, Debug, PartialEq)]
pub struct RaidTemplate {
    pub name: String,
    pub members: Vec<Member>,
    /// Used by the automatic raids.
    pub active: bool,
}

pub const MAX_MEMBERS: usize = 10;

impl RaidTemplate {
    pub fn total(&self) -> u32 {
        self.members.iter().map(|m| m.count).sum()
    }

    /// "5 × Renegado nv.40 + 3 × Lobo nv.45"
    pub fn summary(&self) -> String {
        self.members.iter().map(|m| format!("{} × {} nv.{}", m.count, m.creature, m.level)).collect::<Vec<_>>().join(" + ")
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Drop {
    pub item: u32,
    pub guid: String,
    pub name: String,
    pub amount: u32,
    pub rarity: u32,
    pub level: u32,
    /// 1-100
    pub chance: u32,
}

pub const MAX_DROPS: usize = 8;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Live {
    pub alive: u32,
    pub want: u32,
    pub state: String,
    pub next: Option<u64>,
    pub spawned: u32,
}

pub fn new_id() -> String {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("s{ms}")
}

pub fn to_json(list: &[Spawner]) -> Value {
    let rows: Vec<Value> = list
        .iter()
        .map(|s| {
            json!({
                "id": s.id,
                "name": s.creature,
                "template": s.template,
                "place": s.place,
                "at": s.at,
                "level": s.level,
                "count": s.count,
                "health": s.health,
                "damage": s.damage,
                "radius": s.radius,
                "respawn": s.respawn,
                "once": s.once,
                "pickup": s.pickup,
                "drops": drops_json(&s.drops),
                "every": s.every,
                "area": s.area,
                "pool": templates_json(&s.pool),
            })
        })
        .collect();
    json!({ "spawners": rows })
}

fn drops_json(list: &[Drop]) -> Vec<Value> {
    list.iter().map(|d| json!({"item": d.item, "guid": d.guid, "name": d.name, "amount": d.amount, "rarity": d.rarity, "level": d.level, "chance": d.chance})).collect()
}

fn parse_drops(v: Option<&Value>) -> Vec<Drop> {
    v.and_then(|d| d.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|d| {
                    Some(Drop {
                        item: d.get("item")?.as_u64()? as u32,
                        guid: d.get("guid")?.as_str()?.to_string(),
                        name: d.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        amount: d.get("amount").and_then(|x| x.as_u64()).unwrap_or(1).clamp(1, 65535) as u32,
                        rarity: d.get("rarity").and_then(|x| x.as_u64()).unwrap_or(0).min(4) as u32,
                        level: d.get("level").and_then(|x| x.as_u64()).unwrap_or(1).clamp(1, 255) as u32,
                        chance: d.get("chance").and_then(|x| x.as_u64()).unwrap_or(100).clamp(1, 100) as u32,
                    })
                })
                .take(MAX_DROPS)
                .collect()
        })
        .unwrap_or_default()
}

fn member_json(m: &Member) -> Value {
    json!({"creature": m.creature, "template": m.template, "level": m.level, "count": m.count, "health": m.health, "damage": m.damage, "drops": drops_json(&m.drops)})
}

fn templates_json(list: &[RaidTemplate]) -> Vec<Value> {
    list.iter().map(|t| json!({"name": t.name, "active": t.active, "members": t.members.iter().map(member_json).collect::<Vec<_>>()})).collect()
}

fn parse_member(t: &Value) -> Option<Member> {
    Some(Member {
        creature: t.get("creature").or_else(|| t.get("name")).and_then(|x| x.as_str()).unwrap_or("").to_string(),
        template: t.get("template")?.as_str()?.to_string(),
        level: t.get("level").and_then(|x| x.as_u64()).unwrap_or(1).clamp(1, 1000) as u32,
        count: t.get("count").and_then(|x| x.as_u64()).unwrap_or(1).clamp(1, 20) as u32,
        health: t.get("health").and_then(|x| x.as_u64()).unwrap_or(0).min(2_000_000_000) as u32,
        damage: t.get("damage").and_then(|x| x.as_f64()).unwrap_or(1.0).clamp(0.0, 1000.0) as f32,
        drops: parse_drops(t.get("drops")),
    })
}

fn parse_templates(v: Option<&Value>) -> Vec<RaidTemplate> {
    v.and_then(|d| d.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|t| {
                    let mut members: Vec<Member> = t.get("members").and_then(|m| m.as_array()).map(|m| m.iter().filter_map(parse_member).take(MAX_MEMBERS).collect()).unwrap_or_default();
                    if members.is_empty() {
                        // older files: one enemy per template
                        members.extend(parse_member(t));
                    }
                    if members.is_empty() {
                        return None;
                    }
                    let name = t.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    Some(RaidTemplate { name, members, active: t.get("active").and_then(|x| x.as_bool()).unwrap_or(true) })
                })
                .take(MAX_TEMPLATES)
                .collect()
        })
        .unwrap_or_default()
}

pub fn load_templates(dir: &Path) -> Vec<RaidTemplate> {
    std::fs::read_to_string(dir.join(TEMPLATES_FILE)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| parse_templates(v.get("templates"))).unwrap_or_default()
}

pub fn save_templates(dir: &Path, list: &[RaidTemplate]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&json!({ "templates": templates_json(list) })).map_err(|e| e.to_string())?;
    write_atomic(dir, TEMPLATES_FILE, "m182_raid_templates.json.tmp", &text)
}

fn round(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// What the server needs of one spawner, as short as it can be (it goes through the game chat).
fn wire_row(s: &Spawner) -> Value {
    let mut row = wire_base(s);
    if s.once {
        row["o"] = json!(1);
    }
    let wire_drops = |list: &[Drop]| json!(list.iter().take(MAX_DROPS).map(|d| json!([d.item, d.guid.replace('-', ""), d.amount, d.rarity, d.level, d.chance])).collect::<Vec<_>>());
    if !s.drops.is_empty() {
        row["k"] = wire_drops(&s.drops);
    }
    if (!s.drops.is_empty() || s.pool.iter().any(|t| t.members.iter().any(|m| !m.drops.is_empty()))) && !s.pickup.is_empty() {
        row["q"] = json!(s.pickup.replace('-', ""));
    }
    if !s.pool.is_empty() {
        row["e"] = json!(s.every);
        row["a"] = json!((f64::from(s.area) * 10.0).round() / 10.0);
        row["m"] = json!(s
            .pool
            .iter()
            .filter(|t| t.active || s.once)
            .take(MAX_TEMPLATES)
            .map(|t| {
                json!({"u": t.members.iter().take(MAX_MEMBERS).map(|m| {
                    let mut e = json!({"t": m.template.replace('-', ""), "l": m.level, "c": m.count, "h": m.health, "d": (f64::from(m.damage) * 100.0).round() / 100.0});
                    if !m.drops.is_empty() {
                        e["k"] = wire_drops(&m.drops);
                    }
                    e
                }).collect::<Vec<_>>()})
            })
            .collect::<Vec<_>>());
    }
    row
}

fn wire_base(s: &Spawner) -> Value {
    json!({
        "i": s.id,
        "t": s.template.replace('-', ""),
        "p": [round(s.at[0]), round(s.at[1]), round(s.at[2])],
        "l": s.level,
        "c": s.count,
        "h": s.health,
        "d": (f64::from(s.damage) * 100.0).round() / 100.0,
        "r": (f64::from(s.radius) * 10.0).round() / 10.0,
        "w": s.respawn,
    })
}

/// The server learns about one new spawner without being sent the whole list again.
pub fn op_add(s: &Spawner) -> String {
    json!({ "add": wire_row(s) }).to_string()
}

/// Several spawners at once (a raid on many places) in one message.
pub fn op_add_many(list: &[Spawner]) -> String {
    json!({ "add": list.iter().map(wire_row).collect::<Vec<_>>() }).to_string()
}

pub fn op_delete(id: &str) -> String {
    json!({ "rm": id }).to_string()
}

pub fn op_delete_many(ids: &[String]) -> String {
    json!({ "rm": ids }).to_string()
}

/// Whole list for the server: the first message replaces what it has, the rest add to it.
pub fn op_replace_all(list: &[Spawner], per_message: usize) -> Vec<String> {
    let rows: Vec<Value> = list.iter().map(wire_row).collect();
    if rows.is_empty() {
        return vec![json!({ "set": [] }).to_string()];
    }
    rows.chunks(per_message.max(1)).enumerate().map(|(i, part)| json!({ if i == 0 { "set" } else { "add" }: part }).to_string()).collect()
}

/// Saves the list next to the game and, when the panel drives a server, tells it only what changed.
pub fn save_change(dir: &Path, list: &[Spawner], op: &str) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&to_json(list)).map_err(|e| e.to_string())?;
    crate::write_local(dir, SPAWNERS_FILE, "m182_spawners.json.tmp", &text)?;
    if crate::remote::route(SPAWNERS_FILE) != crate::remote::Route::Local {
        crate::remote::submit(OP_FILE, op)?;
    }
    Ok(())
}

pub fn parse(text: &str) -> Vec<Spawner> {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let Some(rows) = v.get("spawners").and_then(|r| r.as_array()) else { return Vec::new() };
    rows.iter()
        .filter_map(|r| {
            let at = r.get("at")?.as_array()?;
            if at.len() != 3 {
                return None;
            }
            let num = |i: usize| at[i].as_f64().filter(|x| x.is_finite());
            Some(Spawner {
                id: r.get("id")?.as_str()?.to_string(),
                creature: r.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                template: r.get("template")?.as_str()?.to_string(),
                place: r.get("place").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                at: [num(0)?, num(1)?, num(2)?],
                level: r.get("level").and_then(|x| x.as_u64()).unwrap_or(1).clamp(1, 1000) as u32,
                count: r.get("count").and_then(|x| x.as_u64()).unwrap_or(1).clamp(1, 20) as u32,
                health: r.get("health").and_then(|x| x.as_u64()).unwrap_or(0).min(2_000_000_000) as u32,
                damage: r.get("damage").and_then(|x| x.as_f64()).unwrap_or(1.0).clamp(0.0, 1000.0) as f32,
                radius: r.get("radius").and_then(|x| x.as_f64()).unwrap_or(3.0).clamp(0.5, 60.0) as f32,
                respawn: r.get("respawn").and_then(|x| x.as_u64()).unwrap_or(300).clamp(10, 86_400) as u32,
                once: r.get("once").and_then(|x| x.as_bool()).unwrap_or(false),
                pickup: r.get("pickup").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                drops: parse_drops(r.get("drops")),
                every: r.get("every").and_then(|x| x.as_u64()).unwrap_or(0).min(604_800) as u32,
                area: r.get("area").and_then(|x| x.as_f64()).unwrap_or(0.0).clamp(0.0, 600.0) as f32,
                pool: parse_templates(r.get("pool")),
            })
        })
        .collect()
}

pub fn load(dir: &Path) -> Vec<Spawner> {
    std::fs::read_to_string(dir.join(SPAWNERS_FILE)).map(|t| parse(&t)).unwrap_or_default()
}

pub fn save(dir: &Path, list: &[Spawner]) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&to_json(list)).map_err(|e| e.to_string())?;
    write_atomic(dir, SPAWNERS_FILE, "m182_spawners.json.tmp", &text)
}

pub fn parse_status(text: &str) -> HashMap<String, Live> {
    let mut out = HashMap::new();
    let Ok(v) = serde_json::from_str::<Value>(text) else { return out };
    let Some(rows) = v.get("spawners").and_then(|r| r.as_array()) else { return out };
    for r in rows {
        let Some(id) = r.get("id").and_then(|x| x.as_str()) else { continue };
        out.insert(
            id.to_string(),
            Live {
                alive: r.get("alive").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                want: r.get("want").and_then(|x| x.as_u64()).unwrap_or(1) as u32,
                state: r.get("state").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                next: r.get("next").and_then(|x| x.as_u64()),
                spawned: r.get("spawned").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            },
        );
    }
    out
}

/// Raids the server finished (all their creatures died).
pub fn parse_done(text: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    v.get("done").and_then(|d| d.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// A raid older than this is dropped from the list even if the panel never heard it finished.
pub const RAID_LIFETIME_MS: u128 = 3 * 3600 * 1000;

pub fn created_ms(id: &str) -> Option<u128> {
    id.strip_prefix('s').and_then(|n| n.parse().ok())
}

pub fn expired(s: &Spawner, now_ms: u128) -> bool {
    s.once && created_ms(&s.id).map(|c| now_ms.saturating_sub(c) > RAID_LIFETIME_MS).unwrap_or(false)
}

pub fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

pub fn load_status(dir: &Path) -> HashMap<String, Live> {
    std::fs::read_to_string(dir.join(STATUS_FILE)).map(|t| parse_status(&t)).unwrap_or_default()
}

pub fn clock(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

pub const RANGE_METERS: f64 = 150.0;

/// How far this spawner is from the player and whether that is close enough for it to be active.
pub fn distance_note(player: Option<[f64; 3]>, at: [f64; 3]) -> Option<String> {
    let p = player?;
    let d = ((p[0] - at[0]).powi(2) + (p[2] - at[2]).powi(2)).sqrt();
    let shown = d.round() as i64;
    Some(if d <= RANGE_METERS { format!("A {shown} m de ti: dentro del alcance") } else { format!("A {shown} m de ti: se activa a menos de 150 m") })
}

/// One short line for the list: how many are alive and when the next one comes.
pub fn describe(live: Option<&Live>) -> String {
    let Some(l) = live else { return "Esperando al juego…".to_string() };
    match l.state.as_str() {
        "vivo" => format!("Vivo ({}/{})", l.alive, l.want),
        "esperando para reaparecer" => match l.next {
            Some(n) => format!("Muerto: reaparece en {}", clock(n)),
            None => "Reaparecerá pronto".to_string(),
        },
        "wave_wait" => match l.next {
            Some(n) => format!("Próxima oleada en {}", clock(n)),
            None => "Oleada en curso".to_string(),
        },
        "creando" => "Apareciendo…".to_string(),
        "esperando el mundo" => "Esperando a que cargue el mundo".to_string(),
        "sin jugadores cerca" => "En reposo: se activa al acercarte".to_string(),
        _ => "Preparando…".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Spawner {
        Spawner {
            id: "s1".into(),
            creature: "Troll".into(),
            template: "642098d4-fc09-48e2-811e-782d438bf551".into(),
            place: "Cueva".into(),
            at: [10.5, 20.0, -3.0],
            level: 100,
            count: 2,
            health: 5000,
            damage: 1.5,
            radius: 4.0,
            respawn: 300,
            once: false,
            drops: Vec::new(),
            pickup: String::new(),
            pool: Vec::new(),
            every: 0,
            area: 0.0,
        }
    }

    #[test]
    fn spawners_roundtrip_through_the_file_the_dll_reads() {
        let dir = std::env::temp_dir().join(format!("m182_spawners_panel_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        save(&dir, &[sample()]).unwrap();
        let text = std::fs::read_to_string(dir.join(SPAWNERS_FILE)).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["spawners"][0]["template"], "642098d4-fc09-48e2-811e-782d438bf551");
        assert_eq!(v["spawners"][0]["at"], json!([10.5, 20.0, -3.0]));
        assert_eq!(v["spawners"][0]["respawn"], 300);
        assert_eq!(load(&dir), vec![sample()]);
        save(&dir, &[]).unwrap();
        assert!(load(&dir).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn distance_tells_when_a_spawner_is_in_range() {
        assert_eq!(distance_note(None, [0.0, 0.0, 0.0]), None);
        assert_eq!(distance_note(Some([0.0, 5.0, 0.0]), [30.0, 9.0, 40.0]).unwrap(), "A 50 m de ti: dentro del alcance");
        assert_eq!(distance_note(Some([0.0, 5.0, 0.0]), [300.0, 9.0, 400.0]).unwrap(), "A 500 m de ti: se activa a menos de 150 m");
    }

    #[test]
    fn a_new_spawner_costs_only_a_couple_of_chat_lines() {
        let mut s = sample();
        s.id = "s1792524456177".into();
        s.template = "333ea93f-04f6-4bd2-b003-1582429dfb66".into();
        s.at = [3722.65, 842.38, 1367.64];
        s.count = 5;
        s.level = 20;
        s.health = 1800;
        s.damage = 1.1;
        s.radius = 4.0;
        s.respawn = 300;
        let op = op_add(&s);
        assert!(!op.contains("Troll") && !op.contains("Cueva"), "{op}");
        let lines = crate::remote_proto::compose("secret-key", OP_FILE, &op, crate::remote_proto::DEFAULT_LIMIT);
        assert!(lines.len() <= 5, "{} lines for {op}", lines.len());
        assert!(crate::remote_proto::compose("secret-key", OP_FILE, &op_delete("s1792524456177"), crate::remote_proto::DEFAULT_LIMIT).len() <= 2);
        let many: Vec<Spawner> = (0..50).map(|i| Spawner { id: format!("s{i}"), ..s.clone() }).collect();
        let all = op_replace_all(&many, 4);
        assert_eq!(all.len(), 13);
        assert!(all[0].starts_with(r#"{"set":"#) && all[1].starts_with(r#"{"add":"#));
        assert_eq!(op_replace_all(&[], 4), vec![r#"{"set":[]}"#.to_string()]);
    }

    #[test]
    fn status_text_is_readable() {
        let st = parse_status(r#"{"spawners":[{"id":"a","alive":1,"want":1,"state":"vivo","next":null,"spawned":3},{"id":"b","alive":0,"want":1,"state":"esperando para reaparecer","next":200,"spawned":1}]}"#);
        assert_eq!(describe(st.get("a")), "Vivo (1/1)");
        assert_eq!(describe(st.get("b")), "Muerto: reaparece en 3:20");
        assert_eq!(describe(st.get("c")), "Esperando al juego…");
        let far = Live { state: "sin jugadores cerca".into(), alive: 0, want: 1, next: None, spawned: 0 };
        assert_eq!(describe(Some(&far)), "En reposo: se activa al acercarte");
        assert!(parse_status("nonsense").is_empty());
    }

    #[test]
    fn ids_are_unique_enough() {
        assert!(new_id().starts_with('s'));
    }
    #[test]
    fn a_raid_with_loot_goes_to_the_server_and_back() {
        let mut s = sample();
        s.id = "s1".into();
        s.once = true;
        s.pickup = "c15a5c9d-4956-49ff-9d72-9f522229cf1c".into();
        s.drops = vec![Drop { item: 77, guid: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(), name: "Espada".into(), amount: 2, rarity: 3, level: 40, chance: 25 }];
        let wire = wire_row(&s);
        assert_eq!(wire["o"], 1);
        assert_eq!(wire["k"][0], json!([77, "aaaaaaaabbbbccccddddeeeeeeeeeeee", 2, 3, 40, 25]));
        assert_eq!(wire["q"], "c15a5c9d495649ff9d729f522229cf1c");
        let dir = std::env::temp_dir().join(format!("m182_raid_panel_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        save(&dir, &[s.clone()]).unwrap();
        assert_eq!(load(&dir), vec![s.clone()]);
        let many = op_add_many(&[s.clone(), Spawner { id: "s2".into(), ..s.clone() }]);
        assert!(many.starts_with(r#"{"add":["#));
        assert_eq!(parse_done(r#"{"spawners":[],"done":["a","b"]}"#), vec!["a".to_string(), "b".to_string()]);
        assert!(parse_done("junk").is_empty());
        let old = Spawner { id: "s1000".into(), ..s };
        assert!(expired(&old, 1000 + RAID_LIFETIME_MS + 1));
        assert!(!expired(&old, 1000 + 60_000));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn several_ids_go_in_one_delete() {
        let op = op_delete_many(&["s1".to_string(), "s2".to_string()]);
        let v: Value = serde_json::from_str(&op).unwrap();
        assert_eq!(v["rm"], json!(["s1", "s2"]));
    }

    #[test]
    fn automatic_raids_roundtrip_and_fit_the_wire() {
        let member = |c: u32| Member {
            creature: "Troll".into(),
            template: "642098d4-fc09-48e2-811e-782d438bf551".into(),
            level: 60,
            count: c,
            health: 0,
            damage: 1.5,
            drops: vec![Drop { item: 77, guid: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(), name: "Espada".into(), amount: 1, rarity: 3, level: 40, chance: 50 }],
        };
        let tpl = |n: &str, c: u32| RaidTemplate { name: n.into(), members: vec![member(c), member(c + 1)], active: true };
        let mut s = sample();
        s.pool = (0..10).map(|i| tpl(&format!("T{i}"), i + 1)).collect();
        s.every = 3600;
        s.area = 40.0;
        s.pickup = "c15a5c9d-4956-49ff-9d72-9f522229cf1c".into();
        let wire = wire_row(&s);
        assert_eq!(wire["e"], 3600);
        assert_eq!(wire["m"].as_array().unwrap().len(), 10);
        assert_eq!(wire["m"][1]["u"][0]["c"], 2);
        assert_eq!(wire["m"][1]["u"][1]["c"], 3);
        assert_eq!(s.pool[0].summary(), "1 × Troll nv.60 + 2 × Troll nv.60");
        assert_eq!(s.pool[0].total(), 3);
        let dir = std::env::temp_dir().join(format!("m182_pool_panel_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        save(&dir, &[s.clone()]).unwrap();
        assert_eq!(load(&dir), vec![s.clone()]);
        save_templates(&dir, &s.pool).unwrap();
        assert_eq!(load_templates(&dir), s.pool);
        let lines = crate::remote_proto::compose("secret-key", OP_FILE, &op_add(&s), crate::remote_proto::DEFAULT_LIMIT);
        assert!(lines.len() <= 80, "{} lines", lines.len());
        let _ = std::fs::remove_dir_all(dir);
    }

}
