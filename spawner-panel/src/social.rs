use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, Vec2};
use serde_json::{json, Value};

use crate::session::{LogWatch, Member};
use crate::theme::*;
use crate::{write_atomic, App};

pub const ZONES_FILE: &str = "m182_zones.json";
pub const TEAMS_FILE: &str = "m182_teams.json";
pub const LIVE_FILE: &str = "m182_teams_live.json";
pub const PLAYERS_FILE: &str = "m182_players.json";

#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    pub name: String,
    pub x: f64,
    pub z: f64,
    pub radius: f64,
    pub enabled: bool,
    /// Nobody loses health inside.
    pub safe: bool,
    /// Team that owns the place (1-based position in the teams list); 0 = nobody.
    pub team: usize,
    /// Everybody outside the owning team is a visitor: no chests, no building.
    pub lock: bool,
    /// Also applies to administrators (to try it out).
    pub lock_admins: bool,
    /// Players can always hurt each other inside, even when PvP is off elsewhere.
    pub pvp: bool,
}

impl Zone {
    pub fn new(name: String, x: f64, z: f64, radius: f64) -> Self {
        Self { name, x, z, radius, enabled: true, safe: true, team: 0, lock: false, lock_admins: false, pvp: false }
    }
}

/// Keeps the owners of the zones right after a team was deleted.
pub fn team_removed(zones: &mut [Zone], removed: usize) {
    for z in zones {
        if z.team == removed + 1 {
            z.team = 0;
            z.lock = false;
        } else if z.team > removed + 1 {
            z.team -= 1;
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub key: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Team {
    pub name: String,
    pub members: Vec<Person>,
    /// Keys of the people who may invite and remove players by chat.
    pub leaders: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Seen {
    pub member: Member,
    pub online: Option<Online>,
}

impl Seen {
    pub fn person(&self) -> Person {
        Person { key: self.member.key(), name: self.member.name.clone() }
    }
}

/// Matches the players the game reports in its log (name and platform id) with the characters the
/// native layer sees in the world: the host first, then the others in the order they appeared.
pub fn pair_players(log: &[Member], online: &[Online]) -> Vec<Seen> {
    if log.is_empty() {
        return online
            .iter()
            .map(|p| Seen {
                member: Member { slot: p.number, name: if p.me { format!("Tú (ID {})", p.number) } else { format!("ID {}", p.number) }, platform: String::new(), id: String::new() },
                online: Some(p.clone()),
            })
            .collect();
    }
    let host_online = online.iter().find(|p| p.me).or_else(|| online.first());
    let mut others = online.iter().filter(|p| Some(p.number) != host_online.map(|h| h.number));
    log.iter()
        .map(|m| {
            let online = if m.slot == 0 { host_online.cloned() } else { others.next().cloned() };
            Seen { member: m.clone(), online }
        })
        .collect()
}

pub fn team_numbers(teams: &[Team], seen: &[Seen]) -> Vec<Vec<u32>> {
    teams
        .iter()
        .map(|t| {
            let mut numbers: Vec<u32> = seen
                .iter()
                .filter(|s| t.members.iter().any(|m| m.key == s.member.key()))
                .filter_map(|s| s.online.as_ref().map(|o| o.number))
                .collect();
            numbers.sort_unstable();
            numbers
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Online {
    pub number: u32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub me: bool,
}

impl Online {
    pub fn located(&self) -> bool {
        self.x.is_finite() && self.z.is_finite()
    }
}

pub fn player_label(p: &Online) -> String {
    if p.me {
        format!("Tú (ID {})", p.number)
    } else {
        format!("ID {}", p.number)
    }
}

/// The list a server answered with: `{"seq":N,"players":[{"n":1,"name":"Ana"}]}` for request `seq`.
pub fn parse_roster(text: &str, seq: u32) -> Option<Vec<(u32, String)>> {
    let v: Value = serde_json::from_str(text).ok()?;
    if v.get("seq")?.as_u64()? != seq as u64 {
        return None;
    }
    let mut out: Vec<(u32, String)> = v
        .get("players")?
        .as_array()?
        .iter()
        .filter_map(|p| Some((p.get("n")?.as_u64()? as u32, p.get("name")?.as_str()?.trim().to_string())))
        .filter(|(_, name)| !name.is_empty())
        .collect();
    out.sort_by_key(|p| p.0);
    Some(out)
}

/// The player that asked and where the server sees it: `"me":{"n":5,"x":..,"y":..,"z":..}`.
pub fn parse_roster_me(text: &str, seq: u32) -> Option<(u32, [f64; 3])> {
    let v: Value = serde_json::from_str(text).ok()?;
    if v.get("seq")?.as_u64()? != seq as u64 {
        return None;
    }
    let me = v.get("me")?;
    let p = [me.get("x")?.as_f64()?, me.get("y")?.as_f64()?, me.get("z")?.as_f64()?];
    if p.iter().any(|c| !c.is_finite()) {
        return None;
    }
    Some((me.get("n")?.as_u64()? as u32, p))
}

pub fn roster_seen(list: &[(u32, String)]) -> Vec<Seen> {
    list.iter()
        .map(|(n, label)| {
            let (name, id) = match label.split_once('~') {
                Some((name, id)) => (name.trim(), id.trim()),
                None => (label.trim(), ""),
            };
            let (platform, id) = id.split_once(':').map(|(p, d)| (p.to_string(), d.to_string())).unwrap_or_default();
            Seen {
                member: Member { slot: *n, name: if name.is_empty() { format!("ID {n}") } else { name.to_string() }, platform, id },
                online: Some(Online { number: *n, x: f64::NAN, y: f64::NAN, z: f64::NAN, me: false }),
            }
        })
        .collect()
}

/// Entity number of this account in the server's player list: by Steam id when the list has it,
/// otherwise by name when exactly one player has that name.
pub fn own_entity(list: &[(u32, String)], me: &crate::steam::Account) -> Option<u32> {
    let wanted = format!("steamid:{}", me.id.trim()).to_lowercase();
    let parts = |label: &str| -> (String, String) {
        match label.split_once('~') {
            Some((name, id)) => (name.trim().to_lowercase(), id.trim().to_lowercase()),
            None => (label.trim().to_lowercase(), String::new()),
        }
    };
    if !me.id.trim().is_empty() {
        let by_id: Vec<u32> = list.iter().filter(|(_, label)| parts(label).1 == wanted).map(|(n, _)| *n).collect();
        if let [n] = by_id.as_slice() {
            return Some(*n);
        }
    }
    let name = me.name.trim().to_lowercase();
    if name.is_empty() {
        return None;
    }
    let by_name: Vec<u32> = list.iter().filter(|(_, label)| parts(label).0 == name).map(|(n, _)| *n).collect();
    match by_name.as_slice() {
        [n] => Some(*n),
        _ => None,
    }
}

pub const MANUAL_PLAYERS: u32 = 8;

pub fn manual_players() -> Vec<Seen> {
    (1..=MANUAL_PLAYERS)
        .map(|n| Seen {
            member: Member { slot: n, name: format!("ID {n}"), platform: "slot".to_string(), id: n.to_string() },
            online: Some(Online { number: n, x: 0.0, y: 0.0, z: 0.0, me: false }),
        })
        .collect()
}

pub struct SocialState {
    /// Counter per person of the panel's own membership edits (lowercase name and key).
    pub touched: std::collections::BTreeMap<String, u64>,
    pub manual: bool,
    pub zones: Vec<Zone>,
    pub teams: Vec<Team>,
    pub online: Vec<Online>,
    pub seen: Vec<Seen>,
    pub watch: LogWatch,
    pub written: Vec<Vec<u32>>,
    pub checked: Instant,
    pub zone_name: String,
    pub zone_radius: f64,
    pub team_name: String,
    pub notice: String,
    pub roster: Vec<(u32, String)>,
    pub roster_seq: u32,
    pub roster_asked: Option<Instant>,
    /// This account's entity and position as the server saw them when it answered.
    pub server_me: Option<(u32, [f64; 3])>,
    pub extra: Vec<String>,
    pub extra_input: String,
    pub me: Option<crate::steam::Account>,
    /// Last text of the server's real teams that was applied here.
    pub live_seen: String,
    pub live_note: String,
    /// When the panel last sent its teams: the server's answer is not trusted right after.
    pub last_push: Option<Instant>,
    pub teams_seq: u32,
    pub teams_asked: Option<Instant>,
    pub teams_last: Option<Instant>,
}

impl Default for SocialState {
    fn default() -> Self {
        Self {
            touched: Default::default(),
            manual: false,
            zones: Vec::new(),
            teams: Vec::new(),
            online: Vec::new(),
            seen: Vec::new(),
            watch: LogWatch::new(),
            written: Vec::new(),
            checked: Instant::now(),
            zone_name: String::new(),
            zone_radius: 40.0,
            team_name: String::new(),
            notice: String::new(),
            roster: Vec::new(),
            roster_seq: 0,
            roster_asked: None,
            server_me: None,
            extra: Vec::new(),
            extra_input: String::new(),
            me: None,
            live_seen: String::new(),
            live_note: String::new(),
            last_push: None,
            teams_seq: 0,
            teams_asked: None,
            teams_last: None,
        }
    }
}

/// The server's real teams (`m182_teams_live.json`) put over the panel's list: for every team of
/// the same name the people and leaders become the server's. Returns whether anything changed;
/// `None` when the answer was cut short and must not be used.
pub fn adopt_live(teams: &mut [Team], seen: &[Seen], text: &str) -> Option<bool> {
    let v: Value = serde_json::from_str(text).ok()?;
    if !v.get("complete").and_then(|c| c.as_bool()).unwrap_or(false) {
        return None;
    }
    let list = v.get("teams")?.as_array()?;
    let mut known: Vec<Person> = seen.iter().map(|s| s.person()).collect();
    for t in teams.iter() {
        known.extend(t.members.iter().cloned());
    }
    let strings = |v: &Value, key: &str| -> Vec<String> {
        v.get(key).and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
    };
    let mut changed = false;
    for live in list {
        let name = live.get("name").and_then(|n| n.as_str()).unwrap_or("").trim().to_lowercase();
        let Some(team) = teams.iter_mut().find(|t| !name.is_empty() && t.name.trim().to_lowercase() == name) else { continue };
        let mut members: Vec<Person> = Vec::new();
        for n in strings(live, "members") {
            let person = known.iter().find(|p| p.name.trim().eq_ignore_ascii_case(n.trim())).cloned().unwrap_or(Person { key: format!("name:{n}"), name: n.clone() });
            if !members.iter().any(|m| m.key == person.key) {
                members.push(person);
            }
        }
        let wanted = strings(live, "leaders");
        let leaders: Vec<String> = members
            .iter()
            .filter(|m| wanted.iter().any(|l| l.eq_ignore_ascii_case(&m.key) || l.eq_ignore_ascii_case(&format!("name:{}", m.name))))
            .map(|m| m.key.clone())
            .collect();
        let keys = |l: &[Person]| -> Vec<String> {
            let mut k: Vec<String> = l.iter().map(|p| p.key.clone()).collect();
            k.sort();
            k
        };
        let (mut old_l, mut new_l) = (team.leaders.clone(), leaders.clone());
        old_l.sort();
        new_l.sort();
        if keys(&team.members) != keys(&members) || old_l != new_l {
            changed = true;
        }
        team.members = members;
        team.leaders = leaders;
    }
    Some(changed)
}

pub fn parse_zones(text: &str) -> Vec<Zone> {
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let Some(Value::Array(list)) = root.get("zones") else { return Vec::new() };
    list.iter()
        .filter_map(|v| {
            Some(Zone {
                name: v.get("name")?.as_str()?.to_string(),
                x: v.get("x")?.as_f64()?,
                z: v.get("z")?.as_f64()?,
                radius: v.get("radius")?.as_f64()?,
                enabled: v.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true),
                safe: v.get("safe").and_then(|e| e.as_bool()).unwrap_or(true),
                team: v.get("team").and_then(|e| e.as_u64()).unwrap_or(0) as usize,
                lock: v.get("lock").and_then(|e| e.as_bool()).unwrap_or(false),
                lock_admins: v.get("lock_admins").and_then(|e| e.as_bool()).unwrap_or(false),
                pvp: v.get("pvp").and_then(|e| e.as_bool()).unwrap_or(false),
            })
        })
        .collect()
}

pub fn zones_json(zones: &[Zone]) -> Value {
    json!({
        "zones": zones.iter().map(|z| json!({"name": z.name, "x": z.x, "z": z.z, "radius": z.radius, "enabled": z.enabled, "safe": z.safe, "team": z.team, "lock": z.lock && z.team != 0, "lock_admins": z.lock_admins, "pvp": z.pvp})).collect::<Vec<_>>()
    })
}

pub fn parse_teams(text: &str) -> Vec<Team> {
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let Some(Value::Array(list)) = root.get("teams") else { return Vec::new() };
    list.iter()
        .filter_map(|v| {
            let members = v
                .get("people")
                .and_then(|p| p.as_array())
                .map(|people| {
                    people
                        .iter()
                        .filter_map(|p| Some(Person { key: p.get("key")?.as_str()?.to_string(), name: p.get("name")?.as_str()?.to_string() }))
                        .collect()
                })
                .unwrap_or_default();
            let leaders = v
                .get("leaders")
                .and_then(|l| l.as_array())
                .map(|l| l.iter().filter_map(|k| k.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            Some(Team { name: v.get("name")?.as_str()?.to_string(), members, leaders })
        })
        .collect()
}

pub fn parse_touched(text: &str) -> std::collections::BTreeMap<String, u64> {
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(text) else { return Default::default() };
    root.get("touched")
        .and_then(|t| t.as_object())
        .map(|t| t.iter().filter_map(|(k, v)| Some((k.clone(), v.as_u64()?))).collect())
        .unwrap_or_default()
}

pub fn teams_json(teams: &[Team], numbers: &[Vec<u32>], touched: &std::collections::BTreeMap<String, u64>) -> Value {
    json!({
        "touched": touched,
        "teams": teams.iter().enumerate().map(|(i, t)| json!({
            "name": t.name,
            "members": numbers.get(i).cloned().unwrap_or_default(),
            "people": t.members.iter().map(|p| json!({"key": p.key, "name": p.name})).collect::<Vec<_>>(),
            "leaders": t.leaders.iter().filter(|k| t.members.iter().any(|m| &m.key == *k)).collect::<Vec<_>>(),
        })).collect::<Vec<_>>()
    })
}

pub fn parse_online(text: &str) -> Vec<Online> {
    let Ok(Value::Array(list)) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let mut out: Vec<Online> = list
        .iter()
        .filter_map(|v| {
            Some(Online {
                number: v.get("n")?.as_u64()? as u32,
                x: v.get("x")?.as_f64()?,
                y: v.get("y")?.as_f64()?,
                z: v.get("z")?.as_f64()?,
                me: v.get("me").and_then(|m| m.as_bool()).unwrap_or(false),
            })
        })
        .collect();
    out.sort_by_key(|p| p.number);
    out
}

pub fn zone_at<'a>(zones: &'a [Zone], x: f64, z: f64) -> Option<&'a Zone> {
    zones.iter().find(|zone| zone.enabled && (x - zone.x).powi(2) + (z - zone.z).powi(2) <= zone.radius * zone.radius)
}

pub fn set_member(teams: &mut [Team], team: usize, person: &Person, member: bool) {
    for (i, t) in teams.iter_mut().enumerate() {
        t.members.retain(|m| m.key != person.key);
        t.leaders.retain(|k| *k != person.key);
        if member && i == team {
            t.members.push(person.clone());
        }
    }
}

impl SocialState {
    /// The admin edited this person in the panel: the server ignores older changes made by chat.
    pub fn touch(&mut self, person: &Person) {
        let next = self.touched.values().copied().max().unwrap_or(0) + 1;
        self.touched.insert(person.name.trim().to_lowercase(), next);
        self.touched.insert(person.key.trim().to_lowercase(), next);
        if self.touched.len() > 400 {
            let mut by_age: Vec<(String, u64)> = self.touched.iter().map(|(k, v)| (k.clone(), *v)).collect();
            by_age.sort_by_key(|(_, v)| *v);
            for (k, _) in by_age.into_iter().take(self.touched.len() - 400) {
                self.touched.remove(&k);
            }
        }
    }

    pub fn load(dir: &Path) -> Self {
        let mut s = Self::default();
        if let Ok(text) = std::fs::read_to_string(dir.join(ZONES_FILE)) {
            s.zones = parse_zones(&text);
        }
        if let Ok(text) = std::fs::read_to_string(dir.join(TEAMS_FILE)) {
            s.teams = parse_teams(&text);
            s.touched = parse_touched(&text);
        }
        s.me = crate::steam::local_account(dir);
        s.refresh(dir);
        s
    }

    pub fn refresh(&mut self, dir: &Path) {
        self.checked = Instant::now();
        self.online = std::fs::read_to_string(dir.join(PLAYERS_FILE)).map(|t| parse_online(&t)).unwrap_or_default();
        self.watch.refresh(dir);
        self.seen = pair_players(&self.watch.members, &self.online);
        if crate::remote::active() {
            self.read_roster(dir);
            if !self.roster.is_empty() {
                self.seen = roster_seen(&self.roster);
                if let Some(me) = &self.me {
                    for s in self.seen.iter_mut().filter(|s| s.member.id.is_empty() && s.member.name.trim().eq_ignore_ascii_case(me.name.trim())) {
                        s.member.platform = "steamid".to_string();
                        s.member.id = me.id.clone();
                    }
                }
            }
        }
        self.manual = crate::remote::active() && self.seen.is_empty();
        if self.manual {
            self.seen = manual_players();
            if let Some(me) = &self.me {
                let member = Member { slot: 0, name: me.name.clone(), platform: "steamid".to_string(), id: me.id.clone() };
                self.seen.insert(0, Seen { member, online: None });
            }
        }
    }

    fn read_roster(&mut self, dir: &Path) {
        if self.roster_asked.is_none() {
            return;
        }
        let Ok(text) = std::fs::read_to_string(dir.join(crate::remote::ONLINE_FILE)) else { return };
        if let Some(list) = parse_roster(&text, self.roster_seq) {
            self.roster = list;
            self.roster_asked = None;
            self.server_me = parse_roster_me(&text, self.roster_seq);
            let entity = self.server_me.map(|m| m.0).or_else(|| self.me.as_ref().and_then(|me| own_entity(&self.roster, me)));
            if let Some(n) = entity {
                let text = json!({"entity": n}).to_string();
                if write_atomic(dir, "m182_self.json", "m182_self.json.tmp", &text).is_ok() {
                    crate::remote::note(&format!("your character in the server's player list is entity {n}"));
                }
            }
            if let (Some((_, server)), Some(game)) = (self.server_me, crate::read_player(dir)) {
                let apart = ((server[0] - game[0]).powi(2) + (server[2] - game[2]).powi(2)).sqrt();
                crate::remote::note(&format!(
                    "position check: the server sees you at x={:.1} z={:.1}, your game says x={:.1} z={:.1} ({:.0} m apart)",
                    server[0], server[2], game[0], game[2], apart
                ));
            }
            self.notice = format!("{} jugador(es) conectado(s) al servidor.", self.roster.len());
        }
    }

    /// Asks the server who is connected: the answer comes back through the chat and this game's
    /// native layer copies it into `m182_online.json`.
    pub fn request_roster(&mut self, dir: &Path) {
        let seq = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(1) % 1_000_000) as u32 + 1;
        self.roster_seq = seq;
        self.server_me = None;
        let _ = std::fs::remove_file(dir.join(crate::remote::ONLINE_FILE));
        let scan = json!({"seq": seq}).to_string();
        self.notice = match write_atomic(dir, crate::remote::SCAN_FILE, "m182_scan.request.tmp", &scan) {
            Ok(()) => {
                crate::remote::submit_players(seq);
                self.roster_asked = Some(Instant::now());
                "Preguntando al servidor quién está conectado…".to_string()
            }
            Err(e) => format!("Error: {e}"),
        };
    }

    pub fn roster_failed(&mut self) -> bool {
        match self.roster_asked {
            Some(at) if at.elapsed() > Duration::from_secs(75) => {
                self.roster_asked = None;
                true
            }
            _ => false,
        }
    }

    pub fn save_zones(&mut self, dir: &Path) {
        self.notice = match write_atomic(dir, ZONES_FILE, "m182_zones.json.tmp", &zones_json(&self.zones).to_string()) {
            Ok(()) => "Guardado. El juego lo aplica en un par de segundos.".to_string(),
            Err(e) => format!("Error: {e}"),
        };
    }

    pub fn save_teams(&mut self, dir: &Path) {
        let numbers = team_numbers(&self.teams, &self.seen);
        let result = write_atomic(dir, TEAMS_FILE, "m182_teams.json.tmp", &teams_json(&self.teams, &numbers, &self.touched).to_string());
        if result.is_ok() {
            self.written = numbers;
            self.last_push = Some(Instant::now());
            self.teams_asked = None;
            self.teams_last = None;
            let _ = std::fs::remove_file(dir.join(crate::social::LIVE_FILE));
        }
        self.notice = match result {
            Ok(()) => "Guardado. El juego lo aplica en un par de segundos.".to_string(),
            Err(e) => format!("Error: {e}"),
        };
    }

    /// Keeps the panel's teams equal to the server's: reads what the server published (in a local
    /// game its own file, against a server the answer to a chat question asked every few minutes).
    pub fn live_tick(&mut self, dir: &Path, remote: bool, connected: bool, queued: usize) {
        if queued > 0 || self.last_push.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
            return;
        }
        let path = dir.join(LIVE_FILE);
        let fresh = |dir: &Path| {
            let live = std::fs::metadata(dir.join(LIVE_FILE)).and_then(|m| m.modified()).ok();
            let mine = std::fs::metadata(dir.join(TEAMS_FILE)).and_then(|m| m.modified()).ok();
            match (live, mine) {
                (Some(l), Some(m)) => l >= m,
                (Some(_), None) => true,
                _ => false,
            }
        };
        if remote {
            if self.teams_asked.is_some_and(|at| at.elapsed() > Duration::from_secs(75)) {
                self.teams_asked = None;
            }
        }
        let usable = if remote { self.teams_asked.is_some() } else { true };
        if usable && fresh(dir) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if text != self.live_seen {
                    match adopt_live(&mut self.teams, &self.seen, &text) {
                        Some(changed) => {
                            self.live_seen = text;
                            self.teams_asked = None;
                            self.written = team_numbers(&self.teams, &self.seen);
                            self.live_note = "Equipos sincronizados con el servidor.".to_string();
                            if changed {
                                self.notice = "Los equipos cambiaron en el servidor y ya se muestran aquí.".to_string();
                            }
                        }
                        None => self.live_note = "La lista de equipos del servidor llegó incompleta; se ignora.".to_string(),
                    }
                } else if remote {
                    self.teams_asked = None;
                }
            }
        }
        if remote && connected && self.roster_asked.is_none() && self.teams_asked.is_none() && self.teams_last.map_or(true, |t| t.elapsed() >= Duration::from_secs(180)) {
            let seq = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(1) % 1_000_000) as u32 + 1;
            self.teams_seq = seq;
            let _ = std::fs::remove_file(&path);
            let scan = json!({"seq": seq, "kind": "teams"}).to_string();
            if write_atomic(dir, crate::remote::SCAN_FILE, "m182_scan.request.tmp", &scan).is_ok() {
                crate::remote::submit_teams(seq);
                self.teams_asked = Some(Instant::now());
            }
            self.teams_last = Some(Instant::now());
        }
    }

    pub fn resync_teams(&mut self, dir: &Path) {
        if self.teams.is_empty() {
            return;
        }
        if team_numbers(&self.teams, &self.seen) != self.written {
            let notice = self.notice.clone();
            self.save_teams(dir);
            self.notice = notice;
        }
    }
}

/// What a "where I stand" button is waiting for.
#[derive(Clone, Debug, PartialEq)]
pub enum Here {
    Zone,
    Place(String),
}

impl App {
    /// Takes the position for a zone or a saved place. Against a server the server tells where
    /// this player is standing (it knows for certain); in a local game the game's own position is used.
    pub fn ask_here(&mut self, what: Here) {
        if crate::remote::active() {
            let dir = self.dir.clone();
            self.social.request_roster(&dir);
            if self.social.roster_asked.is_some() {
                self.here = Some((what, Instant::now()));
                self.social.notice = "Pidiendo tu posición al servidor…".to_string();
            }
        } else if let Some(p) = self.player {
            self.place_here(what, p);
        }
    }

    fn place_here(&mut self, what: Here, p: [f64; 3]) {
        match what {
            Here::Zone => self.add_zone(p[0], p[2]),
            Here::Place(name) => {
                self.places.push(crate::Place { name, x: p[0], y: p[1], z: p[2] });
                self.save_places();
            }
        }
    }

    fn finish_here(&mut self) {
        let Some((what, since)) = self.here.clone() else { return };
        if self.social.roster_asked.is_none() {
            self.here = None;
            match (self.social.server_me, self.player) {
                (Some((_, p)), _) => {
                    self.place_here(what, p);
                    self.social.notice = "Posición tomada del servidor.".to_string();
                }
                (None, Some(p)) => {
                    self.place_here(what, p);
                    self.social.notice = "El servidor no mandó tu posición; usé la de tu juego.".to_string();
                }
                (None, None) => self.social.notice = "No pude saber tu posición. Inténtalo otra vez.".to_string(),
            }
        } else if since.elapsed() > Duration::from_secs(30) {
            self.here = None;
            self.social.notice = "El servidor no contestó. Inténtalo otra vez.".to_string();
        }
    }

    pub fn social_tick(&mut self) {
        if self.social.checked.elapsed() >= Duration::from_millis(1000) {
            let dir = self.dir.clone();
            self.social.refresh(&dir);
            let queued = crate::remote::status().queued;
            self.social.live_tick(&dir, crate::remote::active(), crate::remote::connected(), queued);
            self.social.resync_teams(&dir);
            self.finish_here();
        }
    }

    fn add_zone(&mut self, x: f64, z: f64) {
        let name = if self.social.zone_name.trim().is_empty() {
            format!("Zona {}", self.social.zones.len() + 1)
        } else {
            self.social.zone_name.trim().to_string()
        };
        self.social.zones.push(Zone::new(name, x, z, self.social.zone_radius));
        self.social.zone_name.clear();
        let dir = self.dir.clone();
        self.social.save_zones(&dir);
    }

    pub fn zones_tab(&mut self, ui: &mut egui::Ui) {
        heading(ui, "Zonas y bases", 22.0);
        divider(ui);
        match self.player {
            Some(p) => {
                ui.label(format!("Tu posición:  X {:.0}   Z {:.0}", p[0], p[2]));
                if let Some(zone) = zone_at(&self.social.zones, p[0], p[2]) {
                    ui.label(RichText::new(format!("Estás dentro de «{}»", zone.name)).color(GOOD));
                }
            }
            None => {
                ui.label(RichText::new("Sin posición.").color(MUTED));
            }
        }
        label_title(ui, "NUEVA ZONA");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.social.zone_name).hint_text("Nombre").desired_width(220.0));
            ui.label("Radio (m)");
            ui.add(egui::Slider::new(&mut self.social.zone_radius, 5.0..=500.0).fixed_decimals(0));
        });
        ui.horizontal_wrapped(|ui| {
            let ready = (self.player.is_some() || crate::remote::active()) && self.here.is_none();
            if button(ui, "Crear donde estoy", Vec2::new(190.0, 32.0), ready, true) {
                self.ask_here(Here::Zone);
            }
            if !self.places.is_empty() {
                let mut picked = None;
                egui::ComboBox::from_id_salt("zone_place").selected_text("Crear en un lugar guardado…").show_ui(ui, |ui| {
                    for (i, p) in self.places.iter().enumerate() {
                        if ui.selectable_label(false, &p.name).clicked() {
                            picked = Some(i);
                        }
                    }
                });
                if let Some(i) = picked {
                    let (x, z) = (self.places[i].x, self.places[i].z);
                    if self.social.zone_name.trim().is_empty() {
                        self.social.zone_name = self.places[i].name.clone();
                    }
                    self.add_zone(x, z);
                }
            }
        });
        divider(ui);
        let mut changed = false;
        let mut remove = None;
        if self.pvp && self.social.zones.iter().any(|z| z.pvp) {
            ui.label(RichText::new("El PvP está activado en todo el mundo (pestaña PvP). Apágalo ahí para que las zonas de PvP siempre activo se noten.").color(GOLD));
        }
        let team_names: Vec<String> = self.social.teams.iter().map(|t| t.name.clone()).collect();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (i, z) in self.social.zones.iter_mut().enumerate() {
                ui.group(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        changed |= ui.checkbox(&mut z.enabled, "").changed();
                        ui.label(RichText::new(&z.name).font(title_font(17.0)).color(PARCHMENT));
                        ui.label(RichText::new(format!("X {:.0}  Z {:.0}", z.x, z.z)).color(MUTED));
                        ui.label("Radio");
                        changed |= ui.add(egui::DragValue::new(&mut z.radius).speed(1.0).range(5.0..=2000.0).fixed_decimals(0)).changed();
                        if button(ui, "Borrar", Vec2::new(80.0, 26.0), true, false) {
                            remove = Some(i);
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Combate en la zona");
                        let mode = if z.pvp { 2 } else if z.safe { 1 } else { 0 };
                        let names = ["Según la pestaña PvP", "Zona segura (nadie pierde vida)", "PvP siempre activo"];
                        let mut picked = mode;
                        egui::ComboBox::from_id_salt(("zone_combat", i)).selected_text(names[mode]).show_ui(ui, |ui| {
                            for (n, name) in names.iter().enumerate() {
                                ui.selectable_value(&mut picked, n, *name);
                            }
                        });
                        if picked != mode {
                            z.safe = picked == 1;
                            z.pvp = picked == 2;
                            changed = true;
                        }
                        ui.label("Equipo dueño");
                        let current = if z.team == 0 { "Nadie".to_string() } else { team_names.get(z.team - 1).cloned().unwrap_or_else(|| "—".to_string()) };
                        egui::ComboBox::from_id_salt(("zone_team", i)).selected_text(current).show_ui(ui, |ui| {
                            if ui.selectable_value(&mut z.team, 0, "Nadie").changed() {
                                z.lock = false;
                                changed = true;
                            }
                            for (t, name) in team_names.iter().enumerate() {
                                changed |= ui.selectable_value(&mut z.team, t + 1, name).changed();
                            }
                        });
                    });
                    if z.team != 0 {
                        ui.horizontal_wrapped(|ui| {
                            changed |= ui.checkbox(&mut z.lock, "Base protegida").changed();
                            if z.lock {
                                changed |= ui.checkbox(&mut z.lock_admins, "También a los administradores (para probarlo)").changed();
                            }
                        });
                    }
                });
            }
            if self.social.zones.is_empty() {
                ui.label(RichText::new("Sin zonas.").color(MUTED));
            }
        });
        if let Some(i) = remove {
            self.social.zones.remove(i);
            changed = true;
        }
        if changed {
            let dir = self.dir.clone();
            self.social.save_zones(&dir);
        }
        if !self.social.notice.is_empty() {
            ui.label(RichText::new(&self.social.notice).color(GOOD));
        }
    }

    pub fn teams_tab(&mut self, ui: &mut egui::Ui) {
        heading(ui, "Equipos", 22.0);
        divider(ui);
        let state = if self.pvp { "activado" } else { "desactivado" };
        ui.label(RichText::new(format!("PvP: {state}. Se cambia en la pestaña PvP.")).color(MUTED));
        divider(ui);
        label_title(ui, "JUGADORES CONECTADOS");
        if self.social.seen.is_empty() {
            ui.label(RichText::new("Nadie conectado.").color(MUTED));
        }
        if crate::remote::active() {
            let dir = self.dir.clone();
            ui.horizontal_wrapped(|ui| {
                let waiting = self.social.roster_asked.is_some();
                if button(ui, "Buscar jugadores del servidor", Vec2::new(280.0, 32.0), crate::remote::connected() && !waiting, true) {
                    self.social.request_roster(&dir);
                }
                if waiting {
                    ui.label(RichText::new("esperando la respuesta del servidor…").color(GOLD_DIM));
                }
            });
            if self.social.roster_failed() {
                self.social.notice = "El servidor no respondió.".to_string();
            }
            if self.social.manual {
                if let Some(me) = &self.social.me {
                    ui.label(RichText::new(format!("{}  (tú)  ·  Steam {}", me.name, me.id)).font(title_font(17.0)).color(GOLD));
                }
                ui.label(RichText::new("Pulsa «Buscar jugadores del servidor» para ver a los demás.").color(MUTED));
            }
        }
        let manual = self.social.manual;
        for s in self.social.seen.iter().filter(|_| !manual) {
            let team = self.social.teams.iter().find(|t| t.members.iter().any(|m| m.key == s.member.key())).map(|t| t.name.clone()).unwrap_or_else(|| "sin equipo".to_string());
            let place = match &s.online {
                Some(o) if o.located() => format!("X {:.0}  Z {:.0}", o.x, o.z),
                Some(_) => String::new(),
                None => "cargando…".to_string(),
            };
            let host = s.online.as_ref().map(|o| o.me).unwrap_or(s.member.slot == 0);
            let name = if host { format!("{}  (tú)", s.member.name) } else { s.member.name.clone() };
            ui.horizontal_wrapped(|ui| {
                let label = RichText::new(name).font(title_font(17.0));
                ui.label(if host { label.color(GOLD) } else { label.color(PARCHMENT) });
                let id = s.member.id_label();
                if !id.is_empty() {
                    ui.label(RichText::new(id).color(MUTED));
                }
                ui.label(RichText::new(place).color(MUTED));
                ui.label(team);
            });
        }
        divider(ui);

        let seen: Vec<Seen> = self.social.seen.clone();
        let mut candidates: Vec<Person> = seen.iter().map(|s| s.person()).collect();
        for name in &self.social.extra {
            let person = Person { key: format!("name:{name}"), name: name.clone() };
            if !candidates.iter().any(|c| c.key == person.key) {
                candidates.push(person);
            }
        }
        for team in &self.social.teams {
            for m in &team.members {
                if !candidates.iter().any(|c| c.key == m.key) {
                    candidates.push(m.clone());
                }
            }
        }
        let mut changed = false;
        let mut remove = None;
        let mut reapply: Option<usize> = None;
        let mut membership: Vec<(usize, Person, bool)> = Vec::new();
        let columns = ((ui.available_width() / 430.0).floor() as usize).clamp(1, 3);
        let live_note = self.social.live_note.clone();
        egui::ScrollArea::vertical().auto_shrink([false, true]).max_height((ui.available_height() - 120.0).max(240.0)).show(ui, |ui| {
            let count = self.social.teams.len();
            ui.columns(columns, |cols| {
                for (c, col) in cols.iter_mut().enumerate() {
                    for i in (c..count).step_by(columns) {
                        let width = col.available_width();
                        let team = &mut self.social.teams[i];
                        egui::Frame::group(col.style()).inner_margin(egui::Margin::same(10.0)).show(col, |ui| {
                            ui.set_width(width - 24.0);
                            changed |= ui.add(egui::TextEdit::singleline(&mut team.name).font(egui::TextStyle::Heading).desired_width(f32::INFINITY)).changed();
                            ui.horizontal_wrapped(|ui| {
                                if button(ui, "Reaplicar a todos", Vec2::new(170.0, 28.0), !team.members.is_empty(), false) {
                                    reapply = Some(i);
                                }
                                if button(ui, "Borrar equipo", Vec2::new(140.0, 28.0), true, false) {
                                    remove = Some(i);
                                }
                            });
                            ui.add_space(4.0);
                            if team.members.is_empty() {
                                ui.label(RichText::new("Sin miembros.").color(MUTED).size(16.0));
                            }
                            for person in team.members.clone() {
                                let online = seen.iter().any(|s| s.member.key() == person.key);
                                let lead = team.leaders.contains(&person.key);
                                ui.horizontal(|ui| {
                                    let star = RichText::new("★").size(20.0).color(if lead { GOLD } else { MUTED });
                                    if ui
                                        .add(egui::SelectableLabel::new(lead, star))
                                        .on_hover_text("Líder: puede invitar y expulsar desde el chat del juego (!team invite nombre). Puede haber varios.")
                                        .clicked()
                                    {
                                        team.leaders.retain(|k| *k != person.key);
                                        if !lead {
                                            team.leaders.push(person.key.clone());
                                        }
                                        changed = true;
                                    }
                                    let label = if online { person.name.clone() } else { format!("{} (desconectado)", person.name) };
                                    ui.add(egui::Label::new(RichText::new(label).size(17.0).color(if online { PARCHMENT } else { MUTED })).truncate());
                                    if ui.add(egui::Button::new(RichText::new("✕").size(16.0))).on_hover_text("Sacar del equipo").clicked() {
                                        membership.push((i, person.clone(), false));
                                    }
                                });
                            }
                            let free: Vec<&Person> = candidates.iter().filter(|c| !team.members.iter().any(|m| m.key == c.key)).collect();
                            if !free.is_empty() {
                                ui.add_space(4.0);
                                egui::ComboBox::from_id_salt(("team_add", i)).selected_text(RichText::new("Añadir jugador…").size(16.0)).width(width - 40.0).show_ui(ui, |ui| {
                                    for person in free {
                                        let online = seen.iter().any(|s| s.member.key() == person.key);
                                        let label = if online { person.name.clone() } else { format!("{} (desconectado)", person.name) };
                                        if ui.selectable_label(false, RichText::new(label).size(16.0)).clicked() {
                                            membership.push((i, person.clone(), true));
                                        }
                                    }
                                });
                            }
                        });
                        col.add_space(10.0);
                    }
                }
            });
        });
        if !live_note.is_empty() {
            ui.label(RichText::new(live_note).color(MUTED).size(14.0));
        }
        for (team, person, on) in membership {
            self.social.touch(&person);
            set_member(&mut self.social.teams, team, &person, on);
            changed = true;
        }
        if let Some(i) = reapply {
            let people: Vec<Person> = self.social.teams.get(i).map(|t| t.members.clone()).unwrap_or_default();
            for person in &people {
                self.social.touch(person);
            }
            changed = true;
        }
        if let Some(i) = remove {
            self.social.teams.remove(i);
            team_removed(&mut self.social.zones, i);
            let dir = self.dir.clone();
            self.social.save_zones(&dir);
            changed = true;
        }
        if crate::remote::active() {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.social.extra_input).hint_text("Nombre exacto de un jugador").desired_width(260.0));
                if button(ui, "Añadir jugador", Vec2::new(160.0, 30.0), !self.social.extra_input.trim().is_empty(), false) {
                    let name = self.social.extra_input.trim().to_string();
                    if !self.social.extra.contains(&name) {
                        self.social.extra.push(name);
                    }
                    self.social.extra_input.clear();
                }
            });
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.social.team_name).hint_text("Nombre del equipo").desired_width(220.0));
            if button(ui, "Crear equipo", Vec2::new(150.0, 30.0), true, true) {
                let name = if self.social.team_name.trim().is_empty() {
                    format!("Equipo {}", self.social.teams.len() + 1)
                } else {
                    self.social.team_name.trim().to_string()
                };
                self.social.teams.push(Team { name, members: Vec::new(), leaders: Vec::new() });
                self.social.team_name.clear();
                changed = true;
            }
        });
        if changed {
            let dir = self.dir.clone();
            self.social.save_teams(&dir);
        }
        if !self.social.notice.is_empty() {
            ui.label(RichText::new(&self.social.notice).color(GOOD));
        }
        if !self.notice.is_empty() {
            ui.label(RichText::new(&self.notice).color(GOOD));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaders_are_saved_only_while_they_are_members() {
        let a = Person { key: "steamid:1".into(), name: "Ana".into() };
        let mut teams = vec![Team { name: "Rojo".into(), members: vec![a.clone()], leaders: vec!["steamid:1".into()] }];
        let json = teams_json(&teams, &[vec![]], &Default::default());
        assert_eq!(json["teams"][0]["leaders"][0], "steamid:1");
        assert_eq!(parse_teams(&json.to_string())[0].leaders, vec!["steamid:1".to_string()]);
        set_member(&mut teams, 0, &a, false);
        assert!(teams[0].leaders.is_empty());
        assert_eq!(teams_json(&teams, &[vec![]], &Default::default())["teams"][0]["leaders"], json!([]));
    }

    #[test]
    fn zones_roundtrip_and_lookup() {
        let mut off = Zone::new("Off".into(), 0.0, 0.0, 10.0);
        off.enabled = false;
        let mut base = Zone::new("Base".into(), 100.0, -50.0, 30.0);
        base.safe = false;
        base.team = 2;
        base.lock = true;
        base.lock_admins = true;
        let zones = vec![base, off];
        let back = parse_zones(&zones_json(&zones).to_string());
        assert_eq!(back, zones);
        assert_eq!(zone_at(&back, 110.0, -40.0).map(|z| z.name.as_str()), Some("Base"));
        assert!(zone_at(&back, 0.0, 0.0).is_none());
        assert!(parse_zones("basura").is_empty());
    }

    fn person(key: &str, name: &str) -> Person {
        Person { key: key.into(), name: name.into() }
    }

    #[test]
    fn teams_are_exclusive_and_roundtrip() {
        let a = person("steamid:1", "A");
        let b = person("steamid:2", "B");
        let c = person("name:C", "C");
        let mut teams = vec![Team { name: "A".into(), members: vec![a.clone()], leaders: Vec::new() }, Team { name: "B".into(), members: vec![], leaders: Vec::new() }];
        set_member(&mut teams, 1, &a, true);
        assert!(teams[0].members.is_empty());
        assert_eq!(teams[1].members, vec![a.clone()]);
        set_member(&mut teams, 1, &c, true);
        set_member(&mut teams, 1, &b, true);
        assert_eq!(teams[1].members.len(), 3);
        set_member(&mut teams, 1, &b, false);
        assert_eq!(teams[1].members, vec![a, c]);
        assert_eq!(parse_teams(&teams_json(&teams, &[vec![], vec![1, 2]], &Default::default()).to_string()), teams);
    }

    #[test]
    fn server_teams_replace_panel_membership_and_leaders() {
        let ana = person("steamid:1", "Ana");
        let mut teams = vec![
            Team { name: "Red".into(), members: vec![ana.clone(), person("name:Luis", "Luis")], leaders: vec!["steamid:1".into()] },
            Team { name: "Blue".into(), members: vec![], leaders: vec![] },
        ];
        let live = r#"{"complete":true,"teams":[{"name":"red","members":["Luis"],"leaders":["name:luis"]},{"name":"Blue","members":["Ana","Zed"],"leaders":["steamid:1"]}]}"#;
        assert_eq!(adopt_live(&mut teams, &[], live), Some(true));
        assert_eq!(teams[0].members, vec![person("name:Luis", "Luis")]);
        assert_eq!(teams[0].leaders, vec!["name:Luis".to_string()]);
        assert_eq!(teams[1].members, vec![ana, person("name:Zed", "Zed")]);
        assert_eq!(teams[1].leaders, vec!["steamid:1".to_string()]);
        assert_eq!(adopt_live(&mut teams, &[], live), Some(false));
        assert_eq!(adopt_live(&mut teams, &[], r#"{"complete":false,"teams":[]}"#), None);
        assert_eq!(teams[1].members.len(), 2);
    }

    #[test]
    fn log_players_are_paired_with_world_characters() {
        let log = vec![
            Member { slot: 0, name: "moradoox".into(), platform: String::new(), id: String::new() },
            Member { slot: 1, name: "Nacht".into(), platform: "steamid".into(), id: "7656".into() },
        ];
        let online = vec![
            Online { number: 1, x: 1.0, y: 0.0, z: 2.0, me: true },
            Online { number: 2, x: 5.0, y: 0.0, z: 6.0, me: false },
        ];
        let seen = pair_players(&log, &online);
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].online.as_ref().unwrap().number, 1);
        assert_eq!(seen[1].online.as_ref().unwrap().number, 2);
        // a friend whose character has not loaded yet is listed without a position
        let loading = pair_players(&log, &online[..1]);
        assert_eq!(loading.len(), 2);
        assert!(loading[1].online.is_none());
        // a stray extra entity never becomes a listed player
        let extra = vec![online[0].clone(), online[1].clone(), Online { number: 3, x: 0.0, y: 0.0, z: 0.0, me: false }];
        assert_eq!(pair_players(&log[..1], &extra).len(), 1);
        let teams = vec![Team { name: "Rojo".into(), members: vec![seen[0].person(), seen[1].person()], leaders: Vec::new() }];
        assert_eq!(team_numbers(&teams, &seen), vec![vec![1, 2]]);
        assert_eq!(team_numbers(&teams, &loading), vec![vec![1]]);
        // without a readable log the characters of the world are listed by number
        let fallback = pair_players(&[], &online);
        assert_eq!(fallback.len(), 2);
        assert_eq!(fallback[0].member.name, "Tú (ID 1)");
    }
    #[test]
    fn the_server_tells_who_asked_and_where() {
        let text = r#"{"seq":7,"players":[{"n":1,"name":"Ana"}],"me":{"n":5,"x":2995.1,"y":895.0,"z":1607.7}}"#;
        assert_eq!(parse_roster_me(text, 7), Some((5, [2995.1, 895.0, 1607.7])));
        assert_eq!(parse_roster_me(text, 8), None);
        assert_eq!(parse_roster_me(r#"{"seq":7,"players":[]}"#, 7), None);
    }

    #[test]
    fn the_own_character_is_found_in_the_server_list() {
        let me = crate::steam::Account { name: "moradoox".into(), id: "76561199474170255".into() };
        let list = vec![(1, "Heindallr~steamid:76561190000000001".to_string()), (5, "moradoox~steamid:76561199474170255".to_string())];
        assert_eq!(own_entity(&list, &me), Some(5));
        let renamed = vec![(1, "Heindallr~steamid:76561190000000001".to_string()), (4, "Otro~steamid:76561199474170255".to_string())];
        assert_eq!(own_entity(&renamed, &me), Some(4));
        let by_name = vec![(1, "Heindallr".to_string()), (3, "MORADOOX".to_string())];
        assert_eq!(own_entity(&by_name, &me), Some(3));
        let twins = vec![(2, "moradoox".to_string()), (3, "moradoox".to_string())];
        assert_eq!(own_entity(&twins, &me), None);
        assert_eq!(own_entity(&[(1, "Heindallr".to_string())], &me), None);
    }

    #[test]
    fn alone_on_a_server_the_player_is_listed_by_the_steam_name() {
        let mut state = SocialState::default();
        state.me = Some(crate::steam::Account { name: "moradoox".into(), id: "76561199474170255".into() });
        state.manual = true;
        state.seen = manual_players();
        let member = Member { slot: 0, name: "moradoox".into(), platform: "steamid".into(), id: "76561199474170255".into() };
        state.seen.insert(0, Seen { member, online: None });
        assert_eq!(state.seen[0].member.key(), "steamid:76561199474170255");
        assert_eq!(state.seen[0].member.id_label(), "Steam 76561199474170255");
        let teams = vec![Team { name: "Casa".into(), members: vec![state.seen[0].person()], leaders: Vec::new() }];
        assert_eq!(team_numbers(&teams, &state.seen), vec![Vec::<u32>::new()]);
        let json = teams_json(&teams, &team_numbers(&teams, &state.seen), &Default::default());
        assert_eq!(json["teams"][0]["people"][0]["name"], "moradoox");
    }

    #[test]
    fn manual_players_give_team_numbers_without_a_player_list() {
        let seen = manual_players();
        assert_eq!(seen.len(), MANUAL_PLAYERS as usize);
        let teams = vec![Team { name: "Rojo".into(), members: vec![seen[0].person(), seen[2].person()], leaders: Vec::new() }];
        assert_eq!(team_numbers(&teams, &seen), vec![vec![1, 3]]);
    }

    #[test]
    fn the_server_roster_becomes_the_player_list_and_team_keys_are_names() {
        let text = r#"{"seq":42,"players":[{"n":2,"name":"Ana"},{"n":1,"name":" Luis "},{"n":3,"name":""}]}"#;
        assert!(parse_roster(text, 41).is_none(), "an answer to another question is ignored");
        let list = parse_roster(text, 42).unwrap();
        assert_eq!(list, vec![(1, "Luis".to_string()), (2, "Ana".to_string())]);
        let seen = roster_seen(&list);
        assert_eq!(seen[1].member.key(), "name:Ana");
        assert!(!seen[0].online.as_ref().unwrap().located());
        let teams = vec![Team { name: "Casa".into(), members: vec![seen[1].person()], leaders: Vec::new() }];
        assert_eq!(team_numbers(&teams, &seen), vec![vec![2]]);
        let json = teams_json(&teams, &team_numbers(&teams, &seen), &Default::default());
        assert_eq!(json["teams"][0]["people"][0]["name"], "Ana");
        assert!(parse_roster("[", 1).is_none());
    }

    #[test]
    fn players_without_a_readable_name_are_listed_by_their_id() {
        let text = r#"{"seq":5,"players":[{"n":3,"name":"ID 3~steamid:76561199474170255"},{"n":1,"name":"Ana~steamid:76561190000000001"},{"n":2,"name":"ID 2"}]}"#;
        let list = parse_roster(text, 5).unwrap();
        let seen = roster_seen(&list);
        assert_eq!(seen[0].member.name, "Ana");
        assert_eq!(seen[0].member.id_label(), "Steam 76561190000000001");
        assert_eq!(seen[1].member.name, "ID 2");
        assert_eq!(seen[1].member.key(), "name:ID 2");
        assert_eq!(seen[2].member.name, "ID 3");
        assert_eq!(seen[2].member.key(), "steamid:76561199474170255");
        let manual = manual_players();
        assert_eq!(manual[2].member.name, "ID 3");
        assert_eq!(manual[2].member.id_label(), "");
    }

    #[test]
    fn pvp_zone_flag_is_saved_and_read_back() {
        let mut arena = Zone::new("Arena".into(), 5.0, 6.0, 30.0);
        arena.pvp = true;
        arena.safe = false;
        let text = zones_json(&[arena, Zone::new("Town".into(), 0.0, 0.0, 20.0)]).to_string();
        let back = parse_zones(&text);
        assert!(back[0].pvp && !back[0].safe);
        assert!(!back[1].pvp && back[1].safe);
    }

    #[test]
    fn deleting_a_team_releases_or_shifts_the_bases_it_owned() {
        let mut zones = vec![Zone::new("A".into(), 0.0, 0.0, 10.0), Zone::new("B".into(), 0.0, 0.0, 10.0), Zone::new("C".into(), 0.0, 0.0, 10.0)];
        zones[0].team = 1;
        zones[0].lock = true;
        zones[1].team = 2;
        zones[1].lock = true;
        zones[2].team = 3;
        team_removed(&mut zones, 0);
        assert_eq!((zones[0].team, zones[0].lock), (0, false));
        assert_eq!((zones[1].team, zones[2].team), (1, 2));
        assert!(zones[1].lock);
        let json = zones_json(&[Zone { team: 0, lock: true, ..Zone::new("X".into(), 1.0, 2.0, 5.0) }]);
        assert_eq!(json["zones"][0]["lock"], false, "a base without an owner never locks");
    }

    #[test]
    fn online_players_file() {
        let list = parse_online(r#"[{"n":2,"x":1.5,"y":3.0,"z":-4.0},{"n":1,"x":10.0,"y":0.0,"z":5.0}]"#);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].number, 1);
        let me = parse_online(r#"[{"n":3,"x":0,"y":0,"z":0,"me":true}]"#);
        assert!(me[0].me);
        assert_eq!(player_label(&me[0]), "Tú (ID 3)");
        assert!(parse_online("[").is_empty());
    }

    #[test]
    fn state_saves_files_the_dll_reads() {
        let dir = std::env::temp_dir().join(format!("m182_social_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = SocialState::default();
        s.zones.push(Zone::new("Torre".into(), 5.0, 6.0, 20.0));
        s.teams.push(Team { name: "Rojo".into(), members: vec![Person { key: "name:Nacht".into(), name: "Nacht".into() }], leaders: Vec::new() });
        s.save_zones(&dir);
        s.save_teams(&dir);
        let loaded = SocialState::load(&dir);
        assert_eq!(loaded.zones, s.zones);
        assert_eq!(loaded.teams, s.teams);
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(TEAMS_FILE)).unwrap()).unwrap();
        assert_eq!(raw["teams"][0]["members"], json!([]));
        assert_eq!(raw["teams"][0]["people"][0]["name"], json!("Nacht"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn touching_a_person_is_saved_and_counts_up() {
        let mut state = SocialState::default();
        let pedro = Person { key: "name:Pedro".into(), name: "Pedro".into() };
        state.touch(&pedro);
        state.touch(&pedro);
        assert_eq!(state.touched["pedro"], 2);
        assert_eq!(state.touched["name:pedro"], 2);
        let text = teams_json(&[], &[], &state.touched).to_string();
        assert_eq!(parse_touched(&text), state.touched);
    }
}
