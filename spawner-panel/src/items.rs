use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, Vec2};
use serde_json::{json, Value};

use crate::theme::*;
use crate::{powers, write_atomic, App};

pub const STATS_FILE: &str = "m182_item_stats.json";
pub const MOD_DIR: &str = "zz_m182_item_stats";
pub const DATA_FILE: &str = "m182_data.lua";
pub const DATA_MESSAGE_FILE: &str = "m182_data.json";

pub const DMG_KEYS: [&str; 15] = [
    "speedMod",
    "dmgMod",
    "physicalCutDamage",
    "physicalPierceDamage",
    "physicalBluntDamage",
    "magicalFireDamage",
    "magicalIceDamage",
    "magicalFogDamage",
    "magicalLightningDamage",
    "healing",
    "poisonDamage",
    "woodDamage",
    "stoneDamage",
    "metalDamage",
    "explosionDamage",
];

pub const DMG_LABELS: [&str; 15] = [
    "Velocidad (×)",
    "Daño total (×)",
    "Corte",
    "Perforación",
    "Contundente",
    "Fuego",
    "Hielo",
    "Niebla",
    "Rayo",
    "Curación",
    "Veneno",
    "Madera",
    "Piedra",
    "Metal",
    "Explosión",
];

pub const ARM_KEYS: [&str; 9] = ["physical", "blunt", "pierce", "cut", "magical", "fire", "ice", "fog", "lightning"];
pub const ARM_LABELS: [&str; 9] = ["Física", "Contundente", "Perforación", "Corte", "Mágica", "Fuego", "Hielo", "Niebla", "Rayo"];

pub const RARITIES: [&str; 5] = ["Común", "Poco común", "Rara", "Épica", "Legendaria"];
pub const RARITY_NAMES: [&str; 5] = ["Common", "Uncommon", "Rare", "Epic", "Legendary"];

pub const BALANCE_INTEGER: &[&str] = &[
    "durabilityLossCap",
    "playerBaseHealth",
    "playerHealthPerAP",
    "playerBaseMana",
    "playerManaPerAP",
    "playerBaseStamina",
    "playerStaminaPerAP",
    "itemLevelCap",
    "playerLevelCap",
    "hookShotCostBase",
    "maximumAnimalsInWorld",
    "stackFactor",
    "altarFactor",
];

pub fn balance_is_integer(key: &str) -> bool {
    BALANCE_INTEGER.contains(&key)
}

pub fn balance_normalize(key: &str, value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    if balance_is_integer(key) {
        value.round().clamp(0.0, 1_000_000.0)
    } else {
        value.clamp(-1_000_000.0, 1_000_000.0)
    }
}

pub const BALANCE_LABELS: &[(&str, &str)] = &[
    ("durabilityStart", "Durabilidad base de las armas y armaduras"),
    ("durabilityGrowth", "Durabilidad extra por nivel de objeto"),
    ("durabilityLossCap", "Límite de pérdida de durabilidad"),
    ("rarityDurabilityMod.common", "Durabilidad ×  común"),
    ("rarityDurabilityMod.uncommon", "Durabilidad ×  poco común"),
    ("rarityDurabilityMod.rare", "Durabilidad ×  rara"),
    ("rarityDurabilityMod.epic", "Durabilidad ×  épica"),
    ("rarityDurabilityMod.legendary", "Durabilidad ×  legendaria"),
    ("playerBaseHealth", "Vida base del jugador"),
    ("playerHealthPerAP", "Vida por punto de atributo"),
    ("playerBaseMana", "Maná base del jugador"),
    ("playerManaPerAP", "Maná por punto de atributo"),
    ("playerBaseStamina", "Stamina base del jugador"),
    ("playerStaminaPerAP", "Stamina por punto de atributo"),
    ("critBonus", "Bonus de crítico"),
    ("baseCritChance", "Probabilidad base de crítico"),
    ("damageMod2Handed", "Multiplicador de daño a dos manos"),
    ("powerCreepWeapon", "Escalado de poder del arma"),
    ("itemLevelCap", "Nivel máximo de objeto"),
    ("playerLevelCap", "Nivel máximo del jugador"),
    ("stackFactor", "Pilas de objetos: multiplicador (1 = normal)"),
    ("altarFactor", "Altares de llama permitidos: multiplicador (1 = normal)"),
];

#[derive(Clone, Debug, Default)]
pub struct Item {
    pub id: u32,
    pub guid: String,
    pub debug: String,
    pub name: String,
    pub category: String,
    pub rarity: String,
    pub stack: u32,
    pub slot: String,
    pub level_min: u32,
    pub level_max: u32,
    pub give_level: u32,
    pub dmg: [f32; 15],
    pub dmg_set: bool,
    pub arm: [f32; 9],
    pub arm_set: bool,
    pub group: String,
}

impl Item {
    pub fn rarity_index(&self) -> u32 {
        RARITY_NAMES.iter().position(|r| *r == self.rarity).unwrap_or(0) as u32
    }

    pub fn deliverable(&self) -> bool {
        self.id != 0
    }

}

fn saved_notice() -> String {
    if crate::remote::active() {
        "Guardado y enviado al servidor. Reinícialo para aplicarlo.".to_string()
    } else {
        "Guardado. Se aplica al reiniciar.".to_string()
    }
}

pub fn is_internal_item(debug: &str, category: &str) -> bool {
    let n = debug.to_lowercase();
    category == "Customization"
        || n.starts_with('_')
        || n.starts_with("z_")
        || n.starts_with("8klore")
        || n.starts_with("ability_")
        || n.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
        || ["deprecated", "norecipe", "_old", "debug", "_test", "test_", "dummy", "placeholder", "_wip", "unused"].iter().any(|w| n.contains(w))
}

pub fn group_of(category: &str, slot: &str) -> &'static str {
    match category {
        "Weapons" => match slot {
            "RangedWeapon" => "Armas a distancia",
            "Shield" => "Escudos",
            _ => "Armas",
        },
        "Equipment" => {
            if slot.starts_with("Armour") {
                "Armaduras"
            } else if slot.starts_with("Vanity") {
                "Ropa cosmética"
            } else if slot.starts_with("Ring") {
                "Anillos"
            } else if slot == "Shield" {
                "Escudos"
            } else if slot == "RangedWeapon" || slot == "MainHand" {
                "Armas"
            } else {
                "Equipo y accesorios"
            }
        }
        "Tools" | "BuildTools" => "Herramientas",
        "Instrument" => "Instrumentos",
        "FishingRod" => "Pesca",
        "Consumables" => "Comida y pociones",
        "Ammunition" => "Munición",
        "Materials" => "Materiales",
        "WeaponGem" => "Gemas de arma",
        "Blueprints" => "Planos",
        "Currency" | "Collectibles" => "Coleccionables",
        "AnimalFood" | "PetFood" => "Comida de animales",
        "ColorPalette" => "Tintes",
        "Customization" => "Personalización",
        _ => "Otros",
    }
}

fn num_u(s: &str) -> u32 {
    s.trim().parse::<f64>().map(|v| v.max(0.0) as u32).unwrap_or(0)
}

fn num_f(s: &str) -> f32 {
    s.trim().parse::<f32>().unwrap_or(0.0)
}

/// "Rara · T2 · nv.5-12" — rarity (as exported after mods) and tier shown beside the real name.
pub fn item_meta(it: &Item) -> String {
    let mut parts: Vec<String> = Vec::new();
    if RARITY_NAMES.contains(&it.rarity.as_str()) {
        parts.push(RARITIES[it.rarity_index() as usize].to_string());
    }
    if let Some(tier) = it.debug.split(|c: char| c == '_' || c == ' ').find(|t| {
        let l = t.to_lowercase();
        l.strip_prefix('t').map(|r| !r.is_empty() && (r.chars().all(|c| c.is_ascii_digit()) || r == "x")).unwrap_or(false)
    }) {
        parts.push(tier.to_uppercase());
    }
    if it.level_max > 0 {
        parts.push(if it.level_min == it.level_max || it.level_min == 0 { format!("nv.{}", it.level_max) } else { format!("nv.{}-{}", it.level_min, it.level_max) });
    }
    parts.join(" · ")
}

pub fn friendly_name(debug: &str) -> String {
    const DROP: &[&str] = &["armor", "armour", "weapon", "craft", "material", "food", "prop", "decoration", "block", "ammo", "tool", "consumable", "collection", "vanity", "npc", "item"];
    let mut words: Vec<String> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    let mut leading = true;
    for token in debug.split(|c: char| c == '_' || c == ' ').filter(|t| !t.is_empty()) {
        let lower = token.to_lowercase();
        if leading && DROP.contains(&lower.as_str()) {
            continue;
        }
        leading = false;
        let tier = lower.strip_prefix('t').map(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit()) || r == "x").unwrap_or(false);
        if tier {
            tags.push(token.to_uppercase());
            continue;
        }
        if let Some(level) = lower.strip_prefix("lvl") {
            if !level.is_empty() && level.chars().all(|c| c.is_ascii_digit()) {
                tags.push(format!("nv.{}", level.trim_start_matches('0')));
            }
            continue;
        }
        if matches!(lower.as_str(), "1h" | "2h") {
            words.push(token.to_uppercase());
            continue;
        }
        let mut current = String::new();
        let chars: Vec<char> = token.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            let prev = if i > 0 { Some(chars[i - 1]) } else { None };
            let next = chars.get(i + 1).copied();
            let boundary = match prev {
                Some(p) => (p.is_lowercase() && c.is_uppercase()) || (p.is_alphabetic() && c.is_ascii_digit()) || (p.is_ascii_digit() && c.is_alphabetic()) || (p.is_uppercase() && c.is_uppercase() && next.map(|n| n.is_lowercase()).unwrap_or(false)),
                None => false,
            };
            if boundary && !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            current.push(*c);
        }
        if !current.is_empty() {
            words.push(current);
        }
    }
    let words: Vec<String> = words
        .into_iter()
        .map(|w| {
            let lower = w.to_lowercase();
            let lower = if lower == "legs" { "trousers".to_string() } else { lower };
            if matches!(lower.as_str(), "of" | "the" | "and" | "de") {
                lower
            } else if w.len() <= 3 && w.chars().all(|c| c.is_uppercase() || c.is_ascii_digit()) {
                w
            } else {
                let mut cs = lower.chars();
                cs.next().map(|f| f.to_uppercase().collect::<String>() + cs.as_str()).unwrap_or_default()
            }
        })
        .collect();
    let mut out = words.join(" ");
    if out.is_empty() {
        out = debug.trim_matches('_').replace('_', " ");
    }
    if !tags.is_empty() {
        out = format!("{out} ({})", tags.join(" "));
    }
    out
}

pub fn parse_items(text: &str) -> Vec<Item> {
    let mut lines = text.lines();
    let Some(header) = lines.next() else { return Vec::new() };
    let columns: Vec<&str> = header.trim().split(',').collect();
    let col = |name: &str| columns.iter().position(|c| *c == name);
    let c = |parts: &[&str], name: &str| -> String { col(name).and_then(|i| parts.get(i)).map(|s| s.trim().to_string()).unwrap_or_default() };
    let mut out = Vec::new();
    for line in lines {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 14 {
            continue;
        }
        let mut item = Item {
            id: num_u(&c(&parts, "item_id")),
            guid: c(&parts, "guid"),
            debug: c(&parts, "debug_name"),
            name: c(&parts, "name"),
            category: c(&parts, "category"),
            rarity: c(&parts, "rarity"),
            stack: num_u(&c(&parts, "max_stack")).max(1),
            slot: c(&parts, "slot"),
            level_min: num_u(&c(&parts, "level_min")).max(1),
            level_max: num_u(&c(&parts, "level_max")).max(1),
            give_level: num_u(&c(&parts, "give_level")).max(1),
            dmg_set: c(&parts, "dmg_set") == "1",
            arm_set: c(&parts, "ar_set") == "1",
            ..Default::default()
        };
        if item.id == 0 || item.guid.is_empty() {
            continue;
        }
        if item.name.is_empty() {
            item.name = item.debug.clone();
        }
        let dmg_cols = ["speed_mod", "dmg_mod", "cut", "pierce", "blunt", "fire", "ice", "fog", "lightning", "healing", "poison", "wood", "stone", "metal", "explosion"];
        for (i, name) in dmg_cols.iter().enumerate() {
            item.dmg[i] = num_f(&c(&parts, name));
        }
        let arm_cols = ["ar_physical", "ar_blunt", "ar_pierce", "ar_cut", "ar_magical", "ar_fire", "ar_ice", "ar_fog", "ar_lightning"];
        for (i, name) in arm_cols.iter().enumerate() {
            item.arm[i] = num_f(&c(&parts, name));
        }
        item.group = group_of(&item.category, &item.slot).to_string();
        out.push(item);
    }
    out.sort_by(|a, b| {
        let key = |i: &Item| i.name.trim_start_matches(|c: char| c == '_' || c.is_ascii_digit()).to_lowercase();
        key(a).cmp(&key(b)).then(a.id.cmp(&b.id))
    });
    out
}

pub fn load_items(dir: &Path) -> Result<Vec<Item>, String> {
    let path = dir.join("export").join("m182_items.csv");
    let text = std::fs::read_to_string(&path).map_err(|_| {
        "La lista de ítems aún no existe. Instala la carpeta de mod m182_spawn_variants actualizada, abre el juego una vez hasta el menú principal, ciérralo y vuelve a abrir este panel.".to_string()
    })?;
    let items = parse_items(&text);
    if items.is_empty() {
        return Err("La lista de ítems está vacía. Abre el juego una vez con el mod m182_spawn_variants actualizado.".to_string());
    }
    Ok(items)
}

pub const GENERIC_PICKUP: &str = "c15a5c9d-4956-49ff-9d72-9f522229cf1c";
pub const DIRECT_PICKUP: &str = "45cc62a2-e527-4e09-ab94-567d683dc680";

pub fn load_generic_pickup(dir: &Path) -> String {
    if let Ok(text) = std::fs::read_to_string(dir.join("export").join("m182_items_diag.txt")) {
        for line in text.lines() {
            if let Some(rest) = line.trim().strip_prefix("generic_pickup=") {
                let guid = rest.trim();
                if guid.len() == 36 {
                    return guid.to_string();
                }
            }
        }
    }
    GENERIC_PICKUP.to_string()
}

/// The level caps the game ships with. Earlier versions of this mod raised them, and EML keeps
/// whatever a patched game file contained, so the exported values cannot be trusted for these two.
pub const FREE_SKILLS_KEY: &str = "freeSkills";

pub const MULTIPLIER_KEYS: [&str; 2] = ["stackFactor", "altarFactor"];

pub const VANILLA_CAPS: [(&str, f64); 2] = [("itemLevelCap", 55.0), ("playerLevelCap", 50.0)];

pub fn load_balance(dir: &Path) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(dir.join("export").join("m182_balance.csv")) {
        for line in text.lines().skip(1) {
            if let Some((k, v)) = line.split_once(',') {
                if let Ok(v) = v.trim().parse::<f64>() {
                    out.insert(k.trim().to_string(), v);
                }
            }
        }
    }
    for (key, value) in VANILLA_CAPS {
        out.insert(key.to_string(), value);
    }
    for key in MULTIPLIER_KEYS {
        out.insert(key.to_string(), 1.0);
    }
    out
}

pub fn hex_of(values: &[f32]) -> String {
    values.iter().flat_map(|v| v.to_le_bytes()).map(|b| format!("{b:02x}")).collect()
}

/// The last four damage fields (wood, stone, metal, explosion) are unsigned integers in the game.
pub const DMG_INT_FROM: usize = 11;

pub fn normalize_dmg(d: &mut [f32; 15]) {
    for (i, v) in d.iter_mut().enumerate() {
        if !v.is_finite() || *v < 0.0 {
            *v = 0.0;
        }
        if i >= DMG_INT_FROM {
            *v = v.round().min(4_000_000_000.0);
        }
    }
}

/// Bytes of the damage block exactly as the game keeps them: 11 floats followed by 4 u32.
pub fn dmg_hex(values: &[f32; 15]) -> String {
    let mut out = hex_of(&values[..DMG_INT_FROM]);
    for v in &values[DMG_INT_FROM..] {
        let n = if v.is_finite() && *v > 0.0 { v.round().min(4_000_000_000.0) as u32 } else { 0 };
        out.extend(n.to_le_bytes().iter().map(|b| format!("{b:02x}")));
    }
    out
}

pub fn level_hex(min: u32, max: u32) -> String {
    min.to_le_bytes().iter().chain(max.to_le_bytes().iter()).map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Edit {
    pub dmg: Option<[f32; 15]>,
    pub arm: Option<[f32; 9]>,
}

#[derive(Default)]
pub struct ItemsState {
    pub rows: Vec<Item>,
    pub balance_base: BTreeMap<String, f64>,
    pub balance: BTreeMap<String, f64>,
    pub saved: BTreeMap<u32, Edit>,
    pub working: BTreeMap<u32, Edit>,
    pub level: u32,
    pub rarity: u32,
    pub amount: u32,
    pub selected_for: Option<usize>,
    pub notice: String,
    pub outbox: VecDeque<(Instant, Value)>,
    pub generic_pickup: String,
    pub direct: bool,
    pub names_applied: bool,
    pub sent: BTreeMap<u32, Edit>,
    pub dirty: BTreeMap<u32, Instant>,
    pub levels: Levels,
    pub push_due: std::cell::Cell<Option<Instant>>,
}

pub fn load_saved(dir: &Path) -> (BTreeMap<u32, Edit>, BTreeMap<String, f64>) {
    let mut items = BTreeMap::new();
    let mut balance = BTreeMap::new();
    let Some(v) = std::fs::read_to_string(dir.join(STATS_FILE)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
        return (items, balance);
    };
    if let Some(map) = v.get("items").and_then(|m| m.as_object()) {
        for (k, e) in map {
            let Ok(id) = k.parse::<u32>() else { continue };
            let mut edit = Edit::default();
            if let Some(a) = e.get("dmg").and_then(|d| d.as_array()) {
                if a.len() == 15 {
                    let mut arr = [0f32; 15];
                    for (i, x) in a.iter().enumerate() {
                        arr[i] = x.as_f64().unwrap_or(0.0) as f32;
                    }
                    normalize_dmg(&mut arr);
                    edit.dmg = Some(arr);
                }
            }
            if let Some(a) = e.get("arm").and_then(|d| d.as_array()) {
                if a.len() == 9 {
                    let mut arr = [0f32; 9];
                    for (i, x) in a.iter().enumerate() {
                        arr[i] = x.as_f64().unwrap_or(0.0) as f32;
                    }
                    edit.arm = Some(arr);
                }
            }
            if edit.dmg.is_some() || edit.arm.is_some() {
                items.insert(id, edit);
            }
        }
    }
    if let Some(map) = v.get("balance").and_then(|m| m.as_object()) {
        for (k, x) in map {
            if let Some(f) = x.as_f64() {
                balance.insert(k.clone(), f);
            }
        }
    }
    (items, balance)
}

pub type Levels = BTreeMap<u32, (u32, u32)>;

pub fn load_levels(dir: &Path) -> Levels {
    let mut out = Levels::new();
    let Some(v) = std::fs::read_to_string(dir.join(STATS_FILE)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return out };
    if let Some(map) = v.get("levels").and_then(|m| m.as_object()) {
        for (k, r) in map {
            let (Ok(id), Some(a)) = (k.parse::<u32>(), r.as_array()) else { continue };
            if let (Some(lo), Some(hi)) = (a.first().and_then(|x| x.as_u64()), a.get(1).and_then(|x| x.as_u64())) {
                if lo <= hi && hi <= 100_000 {
                    out.insert(id, (lo as u32, hi as u32));
                }
            }
        }
    }
    out
}

/// Item levels the game was asked to deliver, read back from the spawner log.
pub fn delivered_levels(log: &str) -> BTreeMap<u32, u32> {
    let mut out = BTreeMap::new();
    for line in log.lines() {
        let Some(rest) = line.split("delivery: item ").nth(1) else { continue };
        let mut parts = rest.split_whitespace();
        let (Some(id), Some(_), Some(_), Some(level)) = (parts.next(), parts.next(), parts.next(), parts.next()) else { continue };
        if let (Ok(id), Ok(level)) = (id.parse::<u32>(), level.parse::<u32>()) {
            let slot = out.entry(id).or_insert(level);
            *slot = (*slot).max(level);
        }
    }
    out
}

pub fn stats_json(items: &BTreeMap<u32, Edit>, balance: &BTreeMap<String, f64>, levels: &Levels) -> Value {
    let mut map = serde_json::Map::new();
    for (id, e) in items {
        let mut o = serde_json::Map::new();
        if let Some(d) = e.dmg {
            let mut d = d;
            normalize_dmg(&mut d);
            o.insert("dmg".into(), json!(d.iter().map(|v| *v as f64).collect::<Vec<_>>()));
        }
        if let Some(a) = e.arm {
            o.insert("arm".into(), json!(a.iter().map(|v| *v as f64).collect::<Vec<_>>()));
        }
        map.insert(id.to_string(), Value::Object(o));
    }
    let ranges: serde_json::Map<String, Value> = levels.iter().map(|(id, (lo, hi))| (id.to_string(), json!([lo, hi]))).collect();
    json!({"items": Value::Object(map), "balance": balance, "levels": Value::Object(ranges)})
}

fn lua_num(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{:.0}", v)
    } else {
        let text = format!("{:.9}", v);
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

pub fn lua_data(items: &BTreeMap<u32, Edit>, balance: &BTreeMap<String, f64>, levels: &Levels) -> String {
    let list = |values: &[f32]| values.iter().map(|v| lua_num(*v as f64)).collect::<Vec<_>>().join(",");
    let mut s = String::from("return{overrides={");
    for (id, e) in items {
        s.push_str(&format!("[{id}]={{"));
        if let Some(d) = e.dmg {
            s.push_str(&format!("dmg={{{}}},", list(&d)));
        }
        if let Some(a) = e.arm {
            s.push_str(&format!("arm={{{}}},", list(&a)));
        }
        s.push_str("},");
    }
    s.push_str("},levels={");
    for (id, (lo, hi)) in levels {
        s.push_str(&format!("[{id}]={{{lo},{hi}}},"));
    }
    s.push_str("},balance={");
    for (k, v) in balance {
        s.push_str(&format!("[\"{k}\"]={},", lua_num(*v)));
    }
    s.push_str("}}\n");
    s
}

pub fn lua_code() -> String {
    let mut s = String::new();
    s.push_str(
        r#"local loaded, DATA = false, nil
if io.exists("src/m182_data.lua") == true then
    loaded, DATA = pcall(require, "m182_data")
end
if not loaded or type(DATA) ~= "table" then DATA = {} end
local OVERRIDES = type(DATA.overrides) == "table" and DATA.overrides or {}
local LEVELS = type(DATA.levels) == "table" and DATA.levels or {}
local BALANCE = type(DATA.balance) == "table" and DATA.balance or {}
for _, o in pairs(OVERRIDES) do
    if type(o) == "table" then
        if type(o.dmg) == "table" then
            local named = {}
            for i, key in ipairs(__DMG_NAMES__) do if o.dmg[i] ~= nil then named[key] = o.dmg[i] end end
            o.dmg = named
        end
        if type(o.arm) == "table" then
            local named = {}
            for i, key in ipairs(__ARM_NAMES__) do if o.arm[i] ~= nil then named[key] = o.arm[i] end end
            o.arm = named
        end
    end
end
local FREE_SKILLS = BALANCE["freeSkills"] == 1
BALANCE["freeSkills"] = nil
local STACK = tonumber(BALANCE["stackFactor"]) or 1
local ALTAR = tonumber(BALANCE["altarFactor"]) or 1
BALANCE["stackFactor"] = nil
BALANCE["altarFactor"] = nil
if STACK < 1 then STACK = 1 end
if ALTAR < 1 then ALTAR = 1 end
local WIDEN = tonumber(BALANCE["itemLevelCap"]) or 0
if WIDEN <= 55 then WIDEN = 0 end
print(string.format("[m182-item-stats] data: itemLevelCap=%s playerLevelCap=%s stackFactor=%s altarFactor=%s", tostring(BALANCE["itemLevelCap"]), tostring(BALANCE["playerLevelCap"]), tostring(STACK), tostring(ALTAR)))
if next(OVERRIDES) == nil and next(LEVELS) == nil and next(BALANCE) == nil and not FREE_SKILLS and WIDEN == 0 and STACK == 1 and ALTAR == 1 then
    print("[m182-item-stats] no item settings saved, nothing to apply")
    do return end
end

local function try(f)
    local ok, r = pcall(f)
    if ok then return r end
    return nil
end

local function hash_of(v)
    if v == nil then return nil end
    local t = type(v)
    if t == "number" then return v end
    if t == "table" then return v.value or v.id or v.hash end
    local inner = try(function() return v.value end)
    if type(inner) == "number" then return inner end
    return tonumber(tostring(v))
end

local INTEGER_KEYS = {
    durabilityLossCap = true, playerBaseHealth = true, playerHealthPerAP = true, playerBaseMana = true,
    playerManaPerAP = true, playerBaseStamina = true, playerStaminaPerAP = true, itemLevelCap = true,
    playerLevelCap = true, hookShotCostBase = true, maximumAnimalsInWorld = true,
    woodDamage = true, stoneDamage = true, metalDamage = true, explosionDamage = true,
}

local function put(container, key, value)
    if type(value) ~= "number" or value ~= value or value == math.huge or value == -math.huge then return false end
    if INTEGER_KEYS[key] then
        value = math.floor(value + 0.5)
        if value < 0 then value = 0 end
    end
    return pcall(function() container[key] = value end)
end

local applied, failed = 0, 0
print("[m182-item-stats] start")

local function join(t, n)
    local out = {}
    for i = 1, n do out[i] = tostring(t[i]) end
    return table.concat(out, ",")
end

local function unsigned(id)
    if type(id) ~= "number" then return nil end
    id = math.floor(id)
    if id < 0 then id = id + 4294967296 end
    return id
end

local function close(a, b)
    if type(a) ~= "number" or type(b) ~= "number" then return false end
    return math.abs(a - b) <= 0.001 + math.abs(b) * 0.0001
end

local function write_group(container, values, label, report)
    if container == nil then
        failed = failed + 1
        report.missing = report.missing + 1
        return
    end
    for k, v in pairs(values) do
        local target = container
        if label == "dmg" and k ~= "speedMod" and k ~= "dmgMod" then target = try(function() return container.distribution end) end
        if label == "arm" then target = try(function() return container.distribution end) end
        if target ~= nil and put(target, k, v) then
            applied = applied + 1
            report.written = report.written + 1
            local back = try(function() return target[k] end)
            local expected = v
            if INTEGER_KEYS[k] then expected = math.floor(v + 0.5) end
            if not close(back, expected) then
                report.mismatch = report.mismatch + 1
                if report.first == nil then report.first = string.format("%s=%s expected %s", k, tostring(back), tostring(expected)) end
            end
        else
            failed = failed + 1
            report.missing = report.missing + 1
        end
    end
end

local found = {}
local levelsDone = {}
local widened = 0
local stacked = 0
local items = try(function() return game.assets.get_resources_by_type("keen::ItemInfo") end)
if items ~= nil then
    pcall(function()
        for _, resource in ipairs(items) do
            pcall(function()
                local data = resource.data
                local id = unsigned(hash_of(data.itemId))
                local range = id and LEVELS[id]
                if STACK > 1 then
                    local ms = try(function() return data.maxStackSize end)
                    if type(ms) == "number" and ms > 1 then
                        local want = math.min(65535, math.floor(ms * STACK + 0.5))
                        if want > ms and put(data, "maxStackSize", want) then stacked = stacked + 1 end
                    end
                end
                if WIDEN > 0 then
                    local wspan = try(function() return data.itemLevelRange end)
                    local wlo = try(function() return wspan.minLevel end)
                    local whi = try(function() return wspan.maxLevel end)
                    if type(wlo) == "number" and type(whi) == "number" and whi > wlo and whi < WIDEN and put(wspan, "maxLevel", WIDEN) then
                        widened = widened + 1
                    end
                end
                if range ~= nil then
                    local span = try(function() return data.itemLevelRange end)
                    local lo = try(function() return span.minLevel end)
                    local hi = try(function() return span.maxLevel end)
                    local wrote = false
                    if span ~= nil and type(lo) == "number" and type(hi) == "number" then
                        local newLo, newHi = math.min(lo, range[1]), math.max(hi, range[2])
                        if newLo ~= lo then wrote = put(span, "minLevel", newLo) end
                        if newHi ~= hi then wrote = put(span, "maxLevel", newHi) end
                        print(string.format("[m182-item-stats] item %d level range %s-%s -> %s-%s (%s)", id, tostring(lo), tostring(hi),
                            tostring(try(function() return span.minLevel end)), tostring(try(function() return span.maxLevel end)), wrote and "written" or "unchanged"))
                    else
                        print(string.format("[m182-item-stats] item %d has no readable level range", id))
                    end
                    levelsDone[id] = true
                end
                local o = id and OVERRIDES[id]
                if o == nil then return end
                local report = { written = 0, mismatch = 0, missing = 0 }
                if o.dmg ~= nil then write_group(try(function() return data.damageSetup end), o.dmg, "dmg", report) end
                if o.arm ~= nil then write_group(try(function() return data.armorSetup end), o.arm, "arm", report) end
                found[id] = (found[id] or 0) + 1
                print(string.format("[m182-item-stats] item %d (%s): wrote %d, not written %d, read-back mismatches %d%s",
                    id, tostring(try(function() return data.debugName end)), report.written, report.missing, report.mismatch,
                    report.first and (" first: " .. report.first) or ""))
            end)
        end
    end)
end
for id in pairs(OVERRIDES) do
    if found[id] == nil then print(string.format("[m182-item-stats] item %d not found in the game data", id)) end
end
for id in pairs(LEVELS) do
    if levelsDone[id] == nil then print(string.format("[m182-item-stats] item %d (level range) not found in the game data", id)) end
end

print(string.format("[m182-item-stats] items done applied=%d failed=%d widened=%d stacked=%d", applied, failed, widened, stacked))

if FREE_SKILLS then
    local nodes, freed = 0, 0
    local trees = try(function() return game.assets.get_resources_by_type("keen::SkillTreeResource") end)
    if type(trees) == "table" then
        for _, tree in ipairs(trees) do
            pcall(function()
                for _, node in ipairs(tree.data.nodes) do
                    nodes = nodes + 1
                    if put(node, "costs", 0) then freed = freed + 1 end
                end
            end)
        end
    end
    print(string.format("[m182-item-stats] skills free of cost: %d of %d nodes", freed, nodes))
end

local tables = try(function() return game.assets.get_resources_by_type("keen::BalancingTable") end)
if type(tables) == "table" and #tables > 0 then
    local d = tables[1].data
    if ALTAR > 1 then
        local list = try(function() return d.altarsPerFlameLevel end)
        local before, after = {}, {}
        if list ~= nil then
            local count = 0
            for i, v in ipairs(list) do
                count = count + 1
                before[i] = v
                if type(v) == "number" and v > 0 then
                    put(list, i, math.min(255, math.floor(v * ALTAR + 0.5)))
                end
                after[i] = try(function() return list[i] end)
            end
            print(string.format("[m182-item-stats] altars per flame level: %d entries, before {%s} after {%s}", count, join(before, count), join(after, count)))
        else
            print("[m182-item-stats] altars per flame level: field not found")
        end
    end
    for key, value in pairs(BALANCE) do
        local rarity = string.match(key, "^rarityDurabilityMod%.(%a+)$")
        local ok
        if rarity then
            ok = put(d.rarityDurabilityMod, rarity, value)
        else
            ok = put(d, key, value)
        end
        if ok then applied = applied + 1 else failed = failed + 1 end
        local back = try(function()
            if rarity then return d.rarityDurabilityMod[rarity] end
            return d[key]
        end)
        print(string.format("[m182-item-stats] balance %s = %s (%s)", key, tostring(back), ok and "written" or "not written"))
    end
end

print(string.format("[m182-item-stats] values applied=%d failed=%d", applied, failed))

"#,
    );
    let names = |keys: &[&str]| format!("{{{}}}", keys.iter().map(|k| format!("\"{k}\"")).collect::<Vec<_>>().join(","));
    let s = s.replace("__DMG_NAMES__", &names(&DMG_KEYS)).replace("__ARM_NAMES__", &names(&ARM_KEYS));
    s
}

pub fn write_mod(dir: &Path, items: &BTreeMap<u32, Edit>, balance: &BTreeMap<String, f64>, levels: &Levels) -> Result<(), String> {
    let root = dir.join("mods").join(MOD_DIR);
    let src = root.join("src");
    std::fs::create_dir_all(&src).map_err(|e| e.to_string())?;
    let manifest = r#"{
    "id": "zz_m182_item_stats",
    "name": "M182 Item Stats",
    "description": "Applies the item stat and balance values chosen in the M182 Admin Panel.",
    "version": "1.0.0",
    "capabilities": ["patch"],
    "dependencies": [],
    "authors": ["M182"],
    "license": null,
    "icon": null
}
"#;
    std::fs::write(root.join("mod.json"), manifest).map_err(|e| e.to_string())?;
    std::fs::write(src.join("mod.lua"), lua_code()).map_err(|e| e.to_string())?;
    std::fs::write(src.join(DATA_FILE), lua_data(items, balance, levels)).map_err(|e| e.to_string())?;
    Ok(())
}

impl ItemsState {
    pub fn base_edit(item: &Item) -> Edit {
        Edit { dmg: Some(item.dmg), arm: Some(item.arm) }
    }

    pub fn current(&self, item: &Item) -> Edit {
        self.working.get(&item.id).cloned().or_else(|| self.saved.get(&item.id).cloned()).unwrap_or_else(|| Self::base_edit(item))
    }

    pub fn is_modified(&self, item: &Item) -> bool {
        self.saved.contains_key(&item.id)
    }

    pub fn set_edit(&mut self, item: &Item, edit: Edit) {
        let mut edit = edit;
        if let Some(d) = edit.dmg.as_mut() {
            normalize_dmg(d);
        }
        let stripped = Edit {
            dmg: edit.dmg.filter(|d| *d != item.dmg),
            arm: edit.arm.filter(|a| *a != item.arm),
        };
        if stripped.dmg.is_none() && stripped.arm.is_none() {
            self.saved.remove(&item.id);
        } else {
            self.saved.insert(item.id, stripped);
        }
        self.working.remove(&item.id);
        self.dirty.insert(item.id, Instant::now());
    }

    /// Everything the game has to change in memory for this item: the damage and armour blocks
    /// (only when edited, or when a previous edit has to be undone) and the allowed level range
    /// (only when the level to deliver lies outside the range the item normally has).
    pub fn stats_for(&self, item: &Item, level: u32) -> Option<Value> {
        let edit = self.saved.get(&item.id);
        let previous = self.sent.get(&item.id);
        let mut out = serde_json::Map::new();
        let mut put_dmg = |new: [f32; 15]| {
            let mut olds = vec![dmg_hex(&item.dmg)];
            if let Some(p) = previous.and_then(|p| p.dmg) {
                olds.push(dmg_hex(&p));
            }
            out.insert("dmg_new".into(), json!(dmg_hex(&new)));
            out.insert("dmg_old".into(), json!(olds));
        };
        match (edit.and_then(|e| e.dmg), previous.and_then(|p| p.dmg)) {
            (Some(d), _) => put_dmg(d),
            (None, Some(_)) => put_dmg(item.dmg),
            _ => {}
        }
        let arm = match (edit.and_then(|e| e.arm), previous.and_then(|p| p.arm)) {
            (Some(a), _) => Some(a),
            (None, Some(_)) => Some(item.arm),
            _ => None,
        };
        if let Some(a) = arm {
            let mut olds = vec![hex_of(&item.arm)];
            if let Some(p) = previous.and_then(|p| p.arm) {
                olds.push(hex_of(&p));
            }
            out.insert("arm_new".into(), json!(hex_of(&a)));
            out.insert("arm_old".into(), json!(olds));
        }
        if level > 0 && item.level_max > 0 && (level < item.level_min || level > item.level_max) {
            let (lo, hi) = (item.level_min.min(level), item.level_max.max(level));
            out.insert("lvl_new".into(), json!(level_hex(lo, hi)));
            out.insert("lvl_old".into(), json!([level_hex(item.level_min, item.level_max)]));
        }
        if out.is_empty() {
            None
        } else {
            Some(Value::Object(out))
        }
    }

    /// Remembers what a delivery at `level` needs from the game data so the item still
    /// exists, with its level and rarity, after the game is restarted: the item's own level
    /// range and the global item level cap. Returns true when something new has to be saved.
    pub fn note_delivery(&mut self, item: &Item, level: u32) -> bool {
        let mut changed = false;
        if level > 0 && item.level_max > 0 && (level < item.level_min || level > item.level_max) {
            let want = (item.level_min.min(level), item.level_max.max(level));
            let merged = match self.levels.get(&item.id) {
                Some(have) => (have.0.min(want.0), have.1.max(want.1)),
                None => want,
            };
            if self.levels.get(&item.id) != Some(&merged) {
                self.levels.insert(item.id, merged);
                changed = true;
            }
        }
        changed
    }

    pub fn adopt_logged_deliveries(&mut self, log: &str) -> bool {
        let mut changed = false;
        for (id, level) in delivered_levels(log) {
            let Some(item) = self.rows.iter().find(|r| r.id == id).cloned() else { continue };
            changed |= self.note_delivery(&item, level);
        }
        changed
    }

    pub fn stats_request(&self, item: &Item) -> Option<Value> {
        self.stats_for(item, self.level)
    }

    /// Saves the item to disk and queues one live command so a running game picks it up.
    pub fn apply_item(&mut self, dir: &Path, item: &Item) -> Result<(), String> {
        self.dirty.remove(&item.id);
        let stats = self.stats_for(item, 0);
        match self.saved.get(&item.id) {
            Some(e) => {
                self.sent.insert(item.id, e.clone());
            }
            None => {
                self.sent.remove(&item.id);
            }
        }
        self.persist(dir)?;
        if let Some(stats) = stats {
            self.outbox.push_back((
                Instant::now(),
                json!({
                    "cmd": "item_stats",
                    "label": item.name,
                    "guid": item.guid,
                    "item_id": item.id,
                    "stats": stats,
                }),
            ));
        }
        Ok(())
    }

    pub fn reset_all(&mut self, dir: &Path) -> Result<usize, String> {
        let ids: Vec<u32> = self.saved.keys().copied().collect();
        let count = ids.len();
        for id in ids {
            let Some(item) = self.rows.iter().find(|r| r.id == id).cloned() else {
                self.saved.remove(&id);
                self.sent.remove(&id);
                continue;
            };
            self.saved.remove(&id);
            self.working.remove(&id);
            self.apply_item(dir, &item)?;
        }
        self.saved.clear();
        self.working.clear();
        self.dirty.clear();
        self.balance = self.balance_base.clone();
        self.persist(dir)?;
        Ok(count)
    }

    pub fn persist(&self, dir: &Path) -> Result<(), String> {
        self.persist_local(dir)?;
        self.push_to_server();
        Ok(())
    }

    pub fn data_message(&self) -> String {
        json!({ "lua": lua_data(&self.saved, &self.balance_overrides(), &self.levels) }).to_string()
    }

    pub fn push_to_server(&self) {
        if crate::remote::active() {
            self.push_due.set(Some(Instant::now()));
        }
    }

    pub fn push_now(&self) {
        self.push_due.set(None);
        let _ = crate::remote::submit(DATA_MESSAGE_FILE, &self.data_message());
    }

    pub fn persist_local(&self, dir: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&stats_json(&self.saved, &self.balance_overrides(), &self.levels)).map_err(|e| e.to_string())?;
        write_atomic(dir, STATS_FILE, "m182_item_stats.json.tmp", &text)?;
        write_mod(dir, &self.saved, &self.balance_overrides(), &self.levels)
    }

    pub fn balance_overrides(&self) -> BTreeMap<String, f64> {
        let mut out: BTreeMap<String, f64> = self
            .balance
            .iter()
            .map(|(k, v)| (k.clone(), balance_normalize(k, *v)))
            .filter(|(k, v)| self.balance_base.get(k).map(|b| (b - *v).abs() > 1e-9).unwrap_or(false))
            .collect();
        for (key, vanilla) in VANILLA_CAPS {
            let value = self.balance.get(key).copied().unwrap_or(vanilla);
            out.insert(key.to_string(), balance_normalize(key, value));
        }
        if self.balance.get(FREE_SKILLS_KEY).copied().unwrap_or(0.0) > 0.5 {
            out.insert(FREE_SKILLS_KEY.to_string(), 1.0);
        }
        out
    }

    pub fn item_level_cap(&self) -> u32 {
        self.balance.get("itemLevelCap").copied().unwrap_or(VANILLA_CAPS[0].1).round().clamp(1.0, 255.0) as u32
    }
}

impl App {
    pub fn items_pump(&mut self) {
        if self.items.push_due.get().is_some_and(|t| t.elapsed() >= Duration::from_millis(2500)) {
            self.items.push_now();
        }
        let due: Vec<u32> = self.items.dirty.iter().filter(|(_, t)| t.elapsed() >= Duration::from_millis(1500)).map(|(id, _)| *id).collect();
        for id in due {
            let Some(item) = self.items.rows.iter().find(|r| r.id == id).cloned() else {
                self.items.dirty.remove(&id);
                continue;
            };
            let dir = self.dir.clone();
            self.items.notice = match self.items.apply_item(&dir, &item) {
                Ok(()) => "Guardado.".to_string(),
                Err(e) => format!("Error: {e}"),
            };
        }
        while let Some((at, _)) = self.items.outbox.front() {
            if *at > Instant::now() {
                break;
            }
            let Some((_, cmd)) = self.items.outbox.pop_front() else { break };
            self.items.notice = match powers::command(&self.dir, &cmd) {
                Ok(()) => "Enviado.".to_string(),
                Err(e) => format!("Error: {e}"),
            };
        }
    }

    pub fn item_controls(&mut self, ui: &mut egui::Ui, selected: Option<usize>) {
        let Some(index) = selected.filter(|i| *i < self.items.rows.len()) else {
            ui.label(RichText::new("Elige un ítem.").color(MUTED));
            return;
        };
        let item = self.items.rows[index].clone();
        if self.items.selected_for != Some(index) {
            self.items.selected_for = Some(index);
            self.items.level = item.give_level.clamp(1, self.items.item_level_cap());
            self.items.rarity = item.rarity_index();
            self.items.amount = 1;
        }
        let rarity_name = RARITIES[item.rarity_index().min(4) as usize];
        if let Some(entry) = self.item_cat.entries.get(index) {
            ui.label(RichText::new(entry.label.clone()).font(title_font(20.0)).color(GOLD));
            ui.label(RichText::new(item.debug.clone()).color(MUTED).size(13.5));
        }
        ui.label(RichText::new(format!("{}  ·  rareza base {}  ·  niveles {}–{}", item.group, rarity_name, item.level_min, item.level_max)).color(MUTED).size(15.0));
        divider(ui);
        label_title(ui, "NIVEL DEL ÍTEM");
        let cap = self.items.item_level_cap();
        self.items.level = self.items.level.clamp(1, cap);
        ui.add(egui::Slider::new(&mut self.items.level, 1..=cap));
        label_title(ui, "RAREZA");
        egui::ComboBox::from_id_salt("item_rarity").selected_text(RARITIES[self.items.rarity.min(4) as usize]).show_ui(ui, |ui| {
            for (i, name) in RARITIES.iter().enumerate() {
                ui.selectable_value(&mut self.items.rarity, i as u32, *name);
            }
        });
        if item.stack > 1 {
            label_title(ui, "CANTIDAD");
            let max = item.stack.clamp(2, 9999);
            self.items.amount = self.items.amount.clamp(1, max);
            ui.add(egui::Slider::new(&mut self.items.amount, 1..=max).logarithmic(true));
        } else {
            self.items.amount = 1;
        }
        ui.checkbox(&mut self.items.direct, "Entregar directo al inventario").on_hover_text("El objeto se recoge solo al aparecer junto a ti. Si no está marcado, cae al suelo.");
        divider(ui);
        self.item_stats_ui(ui, index);
        if !self.items.notice.is_empty() {
            ui.label(RichText::new(self.items.notice.clone()).color(GOOD));
        }
    }

    pub fn item_stats_ui(&mut self, ui: &mut egui::Ui, index: usize) {
        let item = self.items.rows[index].clone();
        let mut edit = self.items.current(&item);
        let modified = self.items.is_modified(&item);
        label_title(ui, "MODIFICADORES DE STATS");
        if modified {
            ui.label(RichText::new("Personalizado.").color(GOOD));
        }
        let mut changed = false;
        let show_dmg = item.dmg_set || item.category == "Weapons" || item.dmg.iter().any(|v| *v != 0.0);
        let show_arm = item.arm_set || item.slot.starts_with("Armour") || item.arm.iter().any(|v| *v != 0.0);
        if show_dmg {
            let mut values = edit.dmg.unwrap_or(item.dmg);
            ui.label(RichText::new("Daño del arma").color(GOLD_DIM));
            egui::Grid::new(("dmg", item.id)).num_columns(2).show(ui, |ui| {
                for i in 0..15 {
                    ui.label(DMG_LABELS[i]);
                    if i >= DMG_INT_FROM {
                        changed |= ui.add(egui::DragValue::new(&mut values[i]).speed(1.0).range(0.0..=1_000_000.0).max_decimals(0)).changed();
                        values[i] = values[i].round();
                    } else {
                        let speed = if i < 2 { 0.01 } else { 0.5 };
                        changed |= ui.add(egui::DragValue::new(&mut values[i]).speed(speed).range(0.0..=100000.0).max_decimals(3)).changed();
                    }
                    ui.end_row();
                }
            });
            edit.dmg = Some(values);
        }
        if show_arm {
            let mut values = edit.arm.unwrap_or(item.arm);
            ui.label(RichText::new("Resistencias de la armadura").color(GOLD_DIM));
            egui::Grid::new(("arm", item.id)).num_columns(2).show(ui, |ui| {
                for i in 0..9 {
                    ui.label(ARM_LABELS[i]);
                    changed |= ui.add(egui::DragValue::new(&mut values[i]).speed(0.5).range(0.0..=100000.0).max_decimals(3)).changed();
                    ui.end_row();
                }
            });
            edit.arm = Some(values);
        }
        if !show_dmg && !show_arm {
            ui.label(RichText::new("Sin stats propios.").color(MUTED));
        }
        if changed {
            self.items.set_edit(&item, edit.clone());
        }
        if show_dmg || show_arm {
            ui.horizontal_wrapped(|ui| {
                for (label, factor, width) in [("×2", 2.0_f32, 54.0), ("×5", 5.0, 54.0), ("×10", 10.0, 58.0)] {
                    if button(ui, label, Vec2::new(width, 28.0), true, false) {
                        let mut e = edit.clone();
                        if let Some(d) = e.dmg.as_mut() {
                            d[1] *= factor;
                        }
                        if let Some(a) = e.arm.as_mut() {
                            a.iter_mut().for_each(|v| *v *= factor);
                        }
                        self.items.set_edit(&item, e);
                    }
                }
            });
            if button(ui, "Restaurar valores originales", Vec2::new(ui.available_width(), 30.0), modified, false) {
                self.items.set_edit(&item, ItemsState::base_edit(&item));
                self.items.notice = "Valores originales restaurados.".to_string();
            }
        }
    }

    pub fn balance_ui(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let mut free = self.items.balance.get(FREE_SKILLS_KEY).copied().unwrap_or(0.0) > 0.5;
        if ui.checkbox(&mut free, "Habilidades sin costo").changed() {
            self.items.balance.insert(FREE_SKILLS_KEY.to_string(), if free { 1.0 } else { 0.0 });
            changed = true;
        }
        if BALANCE_LABELS.iter().all(|(k, _)| !self.items.balance.contains_key(*k)) {
            ui.label(RichText::new("Sin valores todavía.").color(MUTED));
        }
        for (key, label) in BALANCE_LABELS {
            if let Some(value) = self.items.balance.get_mut(*key) {
                ui.horizontal(|ui| {
                    ui.label(*label);
                    if balance_is_integer(key) {
                        changed |= ui.add(egui::DragValue::new(value).speed(1.0).max_decimals(0).range(0.0..=1_000_000.0)).changed();
                        *value = value.round();
                    } else {
                        changed |= ui.add(egui::DragValue::new(value).speed(0.05).max_decimals(3)).changed();
                    }
                });
            }
        }
        if changed {
            let dir = self.dir.clone();
            self.items.notice = match self.items.persist(&dir) {
                Ok(()) => saved_notice(),
                Err(e) => format!("Error: {e}"),
            };
        }
        if button(ui, "Restaurar", Vec2::new(110.0, 32.0), true, false) {
            self.items.balance = self.items.balance_base.clone();
            let dir = self.dir.clone();
            let _ = self.items.persist(&dir);
            self.items.notice = saved_notice();
        }
    }

    fn profile_ui(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("PERFIL DEL SERVIDOR").font(title_font(22.0)).color(GOLD));
        ui.label(RichText::new("Cada perfil guarda sus propias zonas, apariciones, equipos, lugares y plantillas. Se carga siempre el último que usaste.").color(GOLD_DIM).size(14.5));
        ui.add_space(6.0);
        let mut chosen: Option<String> = None;
        ui.horizontal(|ui| {
            ui.label("Perfil activo");
            egui::ComboBox::from_id_salt("profile").width(260.0).selected_text(self.profile.clone()).show_ui(ui, |ui| {
                for name in &self.profiles {
                    if ui.selectable_label(*name == self.profile, name).clicked() {
                        chosen = Some(name.clone());
                    }
                }
            });
        });
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.profile_new).hint_text("Nombre del perfil nuevo").desired_width(260.0));
            let ready = crate::profile::clean_name(&self.profile_new).is_some();
            if button(ui, "CREAR PERFIL", Vec2::new(160.0, 30.0), ready, false) {
                chosen = crate::profile::clean_name(&self.profile_new);
                self.profile_new.clear();
            }
        });
        if let Some(name) = chosen {
            self.change_profile(&name);
        }
        if !self.profile_notice.is_empty() {
            ui.label(RichText::new(self.profile_notice.clone()).color(GOOD));
        }
        ui.label(RichText::new("Carpeta de los perfiles: profiles, dentro de la carpeta del juego.").color(MUTED).size(13.5));
    }

    pub fn settings_tab(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(12.0);
            self.profile_ui(ui);
            divider(ui);
            ui.label(RichText::new("AJUSTES GLOBALES").font(title_font(22.0)).color(GOLD));
            divider(ui);
            self.balance_ui(ui);
            divider(ui);
            if crate::remote::active() {
                ui.label(RichText::new("Los ajustes se guardan en el servidor al cambiarlos. Reinicia el servidor para aplicarlos.").color(GOLD_DIM).size(14.5));
            }
            if button(ui, "RESTAURAR TODO A VALORES DEL JUEGO", Vec2::new(ui.available_width().min(360.0), 34.0), true, false) {
                let dir = self.dir.clone();
                self.items.notice = match self.items.reset_all(&dir) {
                    Ok(n) => format!("Restaurado ({n} ítems). {}", saved_notice()),
                    Err(e) => format!("Error: {e}"),
                };
            }
            if !self.items.notice.is_empty() {
                ui.add_space(8.0);
                ui.label(RichText::new(self.items.notice.clone()).color(GOOD));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "item_id,guid,debug_name,name,category,rarity,max_stack,slot,level_min,level_max,give_guid,pickup_guid,pickup_name,give_level,speed_mod,dmg_mod,cut,pierce,blunt,fire,ice,fog,lightning,healing,poison,wood,stone,metal,explosion,dmg_set,ar_physical,ar_blunt,ar_pierce,ar_cut,ar_magical,ar_fire,ar_ice,ar_fog,ar_lightning,ar_set";

    fn sample() -> String {
        format!(
            "{HEADER}\n1234,g-sword,Sword_Iron,Iron Sword,Weapons,Rare,1,MainHand,3,20,give-1,,,3,1,1.25,12.5,3,0,7,0,0,0,0,0,0,0,0,0,1,0,0,0,0,0,0,0,0,0,0\n\
             77,g-helm,Helm_A,Adventurer Helmet,Equipment,Common,1,Armour_Head,1,10,give-2,pick-2,LootPickup_Helm,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,5,2,3,4,0,1,0,0,0,1\n\
             9,g-bread,Bread,Bread,Consumables,Common,20,,1,1,give-3,,,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0\n"
        )
    }

    #[test]
    fn parses_items_and_groups() {
        let items = parse_items(&sample());
        assert_eq!(items.len(), 3);
        let helm = items.iter().find(|i| i.id == 77).unwrap();
        assert_eq!(helm.group, "Armaduras");
        assert_eq!(helm.arm[0], 5.0);
        assert!(helm.arm_set);
        let sword = items.iter().find(|i| i.id == 1234).unwrap();
        assert_eq!(sword.group, "Armas");
        assert_eq!(sword.dmg[1], 1.25);
        assert_eq!(sword.rarity_index(), 2);
        assert!(sword.deliverable());
        assert_eq!(sword.give_level, 3);
        let bread = items.iter().find(|i| i.id == 9).unwrap();
        assert_eq!(bread.group, "Comida y pociones");
    }

    #[test]
    fn dump_empty_lua_when_asked() {
        let Ok(path) = std::env::var("M182_DUMP_EMPTY_LUA") else { return };
        std::fs::write(&path, lua_code()).unwrap();
        let json = serde_json::to_string_pretty(&stats_json(&BTreeMap::new(), &BTreeMap::new(), &Levels::new())).unwrap();
        std::fs::write(format!("{path}.json"), json).unwrap();
    }

    #[test]
    fn dump_lua_when_asked() {
        let Ok(path) = std::env::var("M182_DUMP_LUA") else { return };
        let items = parse_items(&sample());
        let sword = items.iter().find(|i| i.id == 1234).unwrap();
        let helm = items.iter().find(|i| i.id == 77).unwrap();
        let mut dmg = sword.dmg;
        dmg[1] = 5.0;
        dmg[2] = 33.5;
        let mut arm = helm.arm;
        arm[0] = 50.0;
        let mut saved = BTreeMap::new();
        saved.insert(sword.id, Edit { dmg: Some(dmg), arm: None });
        saved.insert(helm.id, Edit { dmg: None, arm: Some(arm) });
        let mut balance = BTreeMap::new();
        balance.insert("durabilityStart".to_string(), 500.0);
        balance.insert("rarityDurabilityMod.epic".to_string(), 3.5);
        let mut levels = Levels::new();
        levels.insert(1234, (3, 255));
        levels.insert(555, (1, 9));
        std::fs::write(path, lua_data(&saved, &balance, &levels)).unwrap();
    }

    #[test]
    fn generic_pickup_comes_from_the_diag_file_or_the_default() {
        let dir = std::env::temp_dir().join(format!("m182_generic_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("export")).unwrap();
        assert_eq!(load_generic_pickup(&dir), GENERIC_PICKUP);
        std::fs::write(dir.join("export").join("m182_items_diag.txt"), "loca: 5 texts\ngeneric_pickup=aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n").unwrap();
        assert_eq!(load_generic_pickup(&dir), "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn junk_items_are_internal() {
        assert!(is_internal_item("_Consumable_Potion_Mana", "Consumables"));
        assert!(is_internal_item("Z_Prop_Decoration_T3_Fence_OLD_NORECIPE", "BuildTools"));
        assert!(is_internal_item("8kLoreText_scroll_bard_song_01", "Collectibles"));
        assert!(is_internal_item("VC_Body_Feet_01", "Customization"));
        assert!(!is_internal_item("Material_T5_Night_Blackflower", "Materials"));
    }

    #[test]
    fn integer_balance_fields_never_receive_decimals() {
        assert_eq!(balance_normalize("itemLevelCap", 85.8), 86.0);
        assert_eq!(balance_normalize("durabilityLossCap", 0.1), 0.0);
        assert_eq!(balance_normalize("playerLevelCap", -4.0), 0.0);
        assert_eq!(balance_normalize("durabilityStart", 398.5), 398.5);
        assert_eq!(balance_normalize("critBonus", f64::NAN), 0.0);
        let mut state = ItemsState::default();
        state.balance_base.insert("itemLevelCap".to_string(), 55.0);
        state.balance_base.insert("durabilityStart".to_string(), 145.0);
        state.balance.insert("itemLevelCap".to_string(), 85.8);
        state.balance.insert("durabilityStart".to_string(), 145.0);
        let o = state.balance_overrides();
        assert_eq!(o.len(), 2);
        assert_eq!(o["itemLevelCap"], 86.0);
        assert_eq!(o["playerLevelCap"], 50.0);
        let lua = lua_data(&BTreeMap::new(), &o, &Levels::new());
        assert!(lua.contains("[\"itemLevelCap\"]=86,"));
        assert!(lua_code().contains("INTEGER_KEYS"));
    }

    #[test]
    fn stack_and_altar_multipliers_travel_with_the_data_and_are_off_by_default() {
        let mut st = ItemsState::default();
        st.balance_base = load_balance(std::path::Path::new("/nonexistent"));
        st.balance = st.balance_base.clone();
        let quiet = st.balance_overrides();
        assert!(!quiet.contains_key("stackFactor") && !quiet.contains_key("altarFactor"));
        st.balance.insert("stackFactor".into(), 5.0);
        st.balance.insert("altarFactor".into(), 5.0);
        let lua = lua_data(&BTreeMap::new(), &st.balance_overrides(), &Levels::new());
        assert!(lua.contains("[\"stackFactor\"]=5,") && lua.contains("[\"altarFactor\"]=5,"), "{lua}");
        assert!(lua_code().contains("maxStackSize") && lua_code().contains("altarsPerFlameLevel"));
    }

    #[test]
    fn friendly_names_are_readable() {
        assert_eq!(friendly_name("Armor_lvl02_Tinko_Arms"), "Tinko Arms (nv.2)");
        assert_eq!(friendly_name("Armor_T0_Fur_Legs"), "Fur Trousers (T0)");
        assert_eq!(friendly_name("Armor_T1_Mage_Wizard_DPS_Chest_Loot"), "Mage Wizard DPS Chest Loot (T1)");
        assert_eq!(friendly_name("T3_Gem_GemOfOverpowering"), "Gem Gem of Overpowering (T3)");
        assert_eq!(friendly_name("Weapon_T2_1H_SwordEpic_02"), "1H Sword Epic 02 (T2)");
        assert_eq!(friendly_name("_Material_TX_Trophy_Wolf_Heavy_head"), "Trophy Wolf Heavy Head (TX)");
        assert_eq!(friendly_name("Food_T6_raw_mushroom_Shiitake"), "Raw Mushroom Shiitake (T6)");
    }

    #[test]
    fn hex_roundtrip_layout() {
        assert_eq!(hex_of(&[1.0, 2.5]), "0000803f00002040");
    }

    #[test]
    fn damage_block_keeps_integer_fields_as_integers() {
        let mut d = [0f32; 15];
        d[0] = 1.0;
        d[11] = 63.0;
        d[12] = 76.4;
        d[13] = 98.0;
        d[14] = -3.0;
        normalize_dmg(&mut d);
        assert_eq!(d[12], 76.0);
        assert_eq!(d[14], 0.0);
        let h = dmg_hex(&d);
        assert_eq!(h.len(), 120);
        assert_eq!(&h[0..8], "0000803f");
        assert_eq!(&h[88..96], "3f000000");
        assert_eq!(&h[96..104], "4c000000");
        assert_eq!(&h[112..120], "00000000");
        assert_eq!(level_hex(1, 255), "01000000ff000000");
    }

    #[test]
    fn level_range_is_only_widened_when_needed() {
        let items = parse_items(&sample());
        let sword = items.iter().find(|i| i.id == 1234).unwrap().clone();
        let st = ItemsState::default();
        assert!(st.stats_for(&sword, 10).is_none());
        assert!(st.stats_for(&sword, 0).is_none());
        let wide = st.stats_for(&sword, 255).unwrap();
        assert_eq!(wide["lvl_new"], level_hex(3, 255));
        assert_eq!(wide["lvl_old"][0], level_hex(3, 20));
        assert!(wide.get("dmg_new").is_none());
        let low = st.stats_for(&sword, 1).unwrap();
        assert_eq!(low["lvl_new"], level_hex(1, 20));
    }

    #[test]
    fn the_message_for_the_server_carries_the_data_table() {
        let mut state = ItemsState::default();
        state.balance.insert("freeSkills".to_string(), 1.0);
        let v: Value = serde_json::from_str(&state.data_message()).unwrap();
        let lua = v["lua"].as_str().unwrap();
        assert!(lua.starts_with("return{") && lua.contains("[\"freeSkills\"]=1,"), "{lua}");
    }

    #[test]
    fn lua_data_has_values_and_the_code_is_static() {
        let items = parse_items(&sample());
        let sword = items.iter().find(|i| i.id == 1234).unwrap();
        let mut dmg = sword.dmg;
        dmg[1] = 5.0;
        let mut saved = BTreeMap::new();
        saved.insert(sword.id, Edit { dmg: Some(dmg), arm: None });
        let mut balance = BTreeMap::new();
        balance.insert("durabilityStart".to_string(), 500.0);
        balance.insert("rarityDurabilityMod.epic".to_string(), 3.5);
        let lua = lua_data(&saved, &balance, &Levels::new());
        assert!(lua.starts_with("return{"));
        assert!(lua.contains("[1234]={dmg={"));
        assert!(lua.contains("[\"durabilityStart\"]=500,"));
        assert!(lua.contains("[\"rarityDurabilityMod.epic\"]=3.5,"));
        assert!(lua_code().contains("\"speedMod\",\"dmgMod\","));
        assert!(lua_code().contains("woodDamage = true, stoneDamage = true, metalDamage = true, explosionDamage = true"));
        assert!(!lua_code().contains("1234") && !lua_code().contains("durabilityStart"));
    }

    #[test]
    fn reset_all_clears_items_balance_and_the_mod() {
        let dir = std::env::temp_dir().join(format!("m182_items_reset_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let items = parse_items(&sample());
        let sword = items.iter().find(|i| i.id == 1234).unwrap().clone();
        let mut state = ItemsState { rows: items, ..Default::default() };
        state.balance_base.insert("durabilityStart".to_string(), 100.0);
        state.balance.insert("durabilityStart".to_string(), 900.0);
        let mut edit = ItemsState::base_edit(&sword);
        edit.dmg.as_mut().unwrap()[1] = 9.0;
        state.set_edit(&sword, edit);
        state.apply_item(&dir, &sword).unwrap();
        state.outbox.clear();
        assert_eq!(state.reset_all(&dir).unwrap(), 1);
        assert!(state.saved.is_empty());
        assert_eq!(state.balance["durabilityStart"], 100.0);
        assert_eq!(state.outbox.len(), 1);
        assert_eq!(state.outbox[0].1["stats"]["dmg_new"], dmg_hex(&sword.dmg));
        let lua = std::fs::read_to_string(dir.join("mods").join(MOD_DIR).join("src").join(DATA_FILE)).unwrap();
        assert!(!lua.contains("dmg={1,9,"));
        assert!(!lua.contains("durabilityStart\"] = 900"));
        let (loaded, bal) = load_saved(&dir);
        assert!(loaded.is_empty());
        assert_eq!(bal.len(), 2);
        assert_eq!(bal["itemLevelCap"], 55.0);
        assert_eq!(bal["playerLevelCap"], 50.0);
    }

    #[test]
    fn skill_tree_abilities_are_hidden() {
        assert!(is_internal_item("Ability_DoubleJump", "Materials"));
        assert!(!is_internal_item("Material_Wood", "Materials"));
    }

    #[test]
    fn apply_item_queues_live_commands_and_writes_files() {
        let dir = std::env::temp_dir().join(format!("m182_items_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let items = parse_items(&sample());
        let sword = items.iter().find(|i| i.id == 1234).unwrap().clone();
        let mut state = ItemsState { rows: items, ..Default::default() };
        let mut edit = ItemsState::base_edit(&sword);
        edit.dmg.as_mut().unwrap()[1] = 9.0;
        state.set_edit(&sword, edit);
        state.apply_item(&dir, &sword).unwrap();
        assert_eq!(state.outbox.len(), 1);
        let first = &state.outbox[0].1;
        assert_eq!(first["cmd"], "item_stats");
        assert_eq!(first["item_id"], 1234);
        assert_eq!(first["stats"]["dmg_old"][0], dmg_hex(&sword.dmg));
        assert!(dir.join(STATS_FILE).exists());
        let lua = std::fs::read_to_string(dir.join("mods").join(MOD_DIR).join("src").join(DATA_FILE)).unwrap();
        assert!(lua.contains("dmg={1,9,"));
        let (loaded, _) = load_saved(&dir);
        assert_eq!(loaded.get(&1234).unwrap().dmg.unwrap()[1], 9.0);
        let mut edit2 = ItemsState::base_edit(&sword);
        edit2.dmg.as_mut().unwrap()[1] = 11.0;
        state.set_edit(&sword, edit2);
        state.outbox.clear();
        state.apply_item(&dir, &sword).unwrap();
        let second = &state.outbox[0].1;
        assert_eq!(second["stats"]["dmg_old"].as_array().unwrap().len(), 2);
        let restore = ItemsState::base_edit(&sword);
        state.set_edit(&sword, restore);
        state.apply_item(&dir, &sword).unwrap();
        assert!(!state.saved.contains_key(&1234));
        assert_eq!(state.outbox.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deliveries_keep_the_item_valid_after_a_restart() {
        let dir = std::env::temp_dir().join(format!("m182_levels_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut st = ItemsState::default();
        st.balance_base.insert("itemLevelCap".into(), 55.0);
        st.balance.insert("itemLevelCap".into(), 55.0);
        let mut item = Item::default();
        item.id = 1275936258;
        item.level_min = 5;
        item.level_max = 5;
        st.rows.push(item.clone());

        assert!(!st.note_delivery(&item, 5));
        assert!(st.note_delivery(&item, 255));
        assert_eq!(st.levels.get(&item.id), Some(&(5, 255)));
        assert_eq!(st.balance["itemLevelCap"], 55.0);
        assert!(!st.note_delivery(&item, 255));
        assert!(!st.note_delivery(&item, 100));

        st.persist(&dir).unwrap();
        assert_eq!(load_levels(&dir).get(&item.id), Some(&(5, 255)));
        let lua = std::fs::read_to_string(dir.join("mods").join(MOD_DIR).join("src").join(DATA_FILE)).unwrap();
        assert!(lua.contains("[1275936258]={5,255}"), "{lua}");
        assert!(lua.contains("[\"itemLevelCap\"]=55"), "{lua}");
        assert!(lua.contains("[\"playerLevelCap\"]=50"), "{lua}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn level_caps_are_forced_back_to_the_games_own_values_even_if_the_export_shows_more() {
        let dir = std::env::temp_dir().join(format!("m182_caps_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("export")).unwrap();
        std::fs::write(dir.join("export").join("m182_balance.csv"), "key,value\nitemLevelCap,255\nplayerLevelCap,255\ndurabilityStart,145\n").unwrap();
        let base = load_balance(&dir);
        assert_eq!(base["itemLevelCap"], 55.0);
        assert_eq!(base["playerLevelCap"], 50.0);
        assert_eq!(base["durabilityStart"], 145.0);
        let mut st = ItemsState::default();
        st.balance_base = base.clone();
        st.balance = base;
        assert_eq!(st.item_level_cap(), 55);
        let forced = st.balance_overrides();
        assert_eq!(forced["itemLevelCap"], 55.0);
        assert_eq!(forced["playerLevelCap"], 50.0);
        assert!(!forced.contains_key("durabilityStart"));
        st.balance.insert("itemLevelCap".into(), 120.0);
        assert_eq!(st.item_level_cap(), 120);
        assert_eq!(st.balance_overrides()["itemLevelCap"], 120.0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn earlier_deliveries_are_recovered_from_the_log() {
        let log = "[1ms] delivery: item 11 placed (level 255 rarity 4 amount 1)\n[2ms] spawn requested\n[3ms] delivery: item 11 placed (level 40 rarity 0 amount 1)\n[4ms] delivery: item 12 placed (level 1 rarity 4 amount 1)\nnoise delivery: item x placed";
        let got = delivered_levels(log);
        assert_eq!(got.get(&11), Some(&255));
        assert_eq!(got.get(&12), Some(&1));
        let mut st = ItemsState::default();
        let mut a = Item::default();
        a.id = 11;
        a.level_min = 3;
        a.level_max = 30;
        let mut b = Item::default();
        b.id = 12;
        b.level_min = 1;
        b.level_max = 30;
        st.rows = vec![a, b];
        assert!(st.adopt_logged_deliveries(log));
        assert_eq!(st.levels.len(), 1);
        assert_eq!(st.levels[&11], (3, 255));
    }

    #[test]
    fn dump_mod_when_asked() {
        let (Ok(from), Ok(to)) = (std::env::var("M182_MOD_FROM"), std::env::var("M182_MOD_TO")) else { return };
        let (from, to) = (std::path::PathBuf::from(from), std::path::PathBuf::from(to));
        let rows = parse_items(&std::fs::read_to_string(from.join("export").join("m182_items.csv")).unwrap());
        let (saved, saved_balance) = load_saved(&from);
        let base = load_balance(&from);
        let mut balance = base.clone();
        for (k, v) in saved_balance {
            balance.insert(k, v);
        }
        let mut st = ItemsState { rows, balance_base: base, balance, saved, levels: load_levels(&from), ..Default::default() };
        if let Ok(log) = std::fs::read_to_string(from.join("m182_spawner.log")) {
            st.adopt_logged_deliveries(&log);
        }
        st.persist(&to).unwrap();
    }

    #[test]
    fn shipped_baseline_mod_matches_the_generator() {
        let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("spawner-mods");
        if let Ok(out) = std::env::var("M182_WRITE_BASELINE") {
            let root = std::path::PathBuf::from(out);
            write_mod(&root, &BTreeMap::new(), &BTreeMap::new(), &Levels::new()).unwrap();
            return;
        }
        let shipped = target.join(MOD_DIR).join("src").join("mod.lua");
        if let Ok(text) = std::fs::read_to_string(shipped) {
            assert_eq!(text, lua_code());
        }
    }
}

#[cfg(test)]
mod meta_tests {
    use super::*;
    #[test]
    fn meta_shows_rarity_tier_level() {
        let mut it = Item::default();
        it.debug = "Armor_Warrior_Tank_Helmet_Loot_T2".into();
        it.rarity = "Epic".into();
        it.level_min = 5;
        it.level_max = 12;
        let m = item_meta(&it);
        assert!(m.contains("T2") && m.contains("nv.5-12") && m.starts_with(RARITIES[3]));
    }
}
