//! Events and raids: groups of enemies ("templates") that are sent to the zones the administrator
//! picks, automatically every few minutes or at the push of a button.

use std::collections::BTreeSet;

use eframe::egui::{self, RichText, Vec2};

use crate::spawners::{self, Drop, Member, RaidTemplate, Spawner, MAX_DROPS, MAX_MEMBERS, MAX_TEMPLATES};
use crate::theme::*;
use crate::App;

pub struct EventState {
    pub zones: BTreeSet<usize>,
    pub notice: String,
    pub templates: Vec<RaidTemplate>,
    pub loaded: bool,
    pub selected: Option<usize>,
    pub loot_open: Option<usize>,
    pub minutes: u32,
    pub creature_query: String,
    pub item_query: String,
    pub item: Option<usize>,
    pub amount: u32,
    pub rarity: u32,
    pub item_level: u32,
    pub chance: u32,
    /// Templates changed after the last time they were sent to the automatic raids.
    pub unapplied: bool,
    /// The zone choice was restored from disk and from the raids that are already running.
    pub restored: bool,
}

impl Default for EventState {
    fn default() -> Self {
        EventState {
            zones: BTreeSet::new(),
            notice: String::new(),
            templates: Vec::new(),
            loaded: false,
            selected: None,
            loot_open: None,
            minutes: 60,
            creature_query: String::new(),
            item_query: String::new(),
            item: None,
            amount: 1,
            rarity: 0,
            item_level: 1,
            chance: 100,
            unapplied: false,
            restored: false,
        }
    }
}

enum Action {
    Launch(usize),
    Activate,
    Apply,
    Save,
}

/// Height for a zone: a saved place inside it, otherwise where the administrator stands.
pub fn zone_height(places: &[(f64, f64, f64)], zone: (f64, f64, f64), me: Option<[f64; 3]>) -> Option<f64> {
    let (zx, zz, r) = zone;
    places.iter().find(|(x, _, z)| ((x - zx).powi(2) + (z - zz).powi(2)).sqrt() <= r).map(|(_, y, _)| *y).or_else(|| me.map(|p| p[1]))
}

/// A spawner that sends templates to random points of a zone. Automatic ones repeat every
/// `every` seconds; with `once` the first wave comes at once and the spawner disappears afterwards.
pub fn raid_spawner(id: String, place: &str, at: [f64; 3], radius: f32, every: u32, once: bool, templates: &[RaidTemplate], pickup: &str) -> Spawner {
    let first = templates.first().and_then(|t| t.members.first()).cloned();
    Spawner {
        id,
        creature: if once { templates.first().map(|t| t.name.clone()).unwrap_or_default() } else { "Raids automáticos".to_string() },
        template: first.map(|m| m.template).unwrap_or_default(),
        place: place.to_string(),
        at,
        level: 1,
        count: 1,
        health: 0,
        damage: 1.0,
        radius: 3.0,
        respawn: 300,
        once,
        drops: Vec::new(),
        pickup: pickup.to_string(),
        pool: templates.iter().take(MAX_TEMPLATES).cloned().collect(),
        every,
        area: radius,
    }
}

fn new_member(creature: String, template: String) -> Member {
    Member { creature, template, level: 50, count: 3, health: 0, damage: 1.0, drops: Vec::new() }
}

pub const CHOICE_FILE: &str = "m182_events.json";

impl App {
    /// Brings back the zones and the interval chosen before: from the file the panel keeps and from
    /// the automatic raids that are already running on the server.
    fn restore_choice(&mut self) {
        if self.ev.restored || self.social.zones.is_empty() {
            return;
        }
        self.ev.restored = true;
        let mut names: BTreeSet<String> = BTreeSet::new();
        if let Some(v) = std::fs::read_to_string(self.dir.join(CHOICE_FILE)).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) {
            if let Some(list) = v.get("zones").and_then(|z| z.as_array()) {
                names.extend(list.iter().filter_map(|n| n.as_str().map(str::to_string)));
            }
            if let Some(m) = v.get("minutes").and_then(|m| m.as_u64()) {
                self.ev.minutes = (m as u32).clamp(1, 1440);
            }
        }
        for s in self.spawners.iter().filter(|s| !s.pool.is_empty() && !s.once) {
            names.insert(s.place.clone());
            if s.every >= 60 {
                self.ev.minutes = (s.every / 60).clamp(1, 1440);
            }
        }
        for (i, z) in self.social.zones.iter().enumerate() {
            if names.contains(&z.name) {
                self.ev.zones.insert(i);
            }
        }
    }

    fn save_choice(&self) {
        let names: Vec<&str> = self.ev.zones.iter().filter_map(|&i| self.social.zones.get(i).map(|z| z.name.as_str())).collect();
        let body = serde_json::json!({ "zones": names, "minutes": self.ev.minutes }).to_string();
        let _ = std::fs::write(self.dir.join(CHOICE_FILE), body);
    }

    fn creature_name(&self, i: usize) -> Option<String> {
        self.creatures.entries.get(i).map(|e| e.alias.clone().unwrap_or_else(|| e.label.clone()))
    }

    fn item_name(&self, i: usize) -> Option<String> {
        self.items.rows.get(i).map(|r| self.item_cat.entries.get(i).map(|e| e.label.clone()).unwrap_or_else(|| r.name.clone()))
    }

    fn zone_spots(&self) -> Result<Vec<(String, [f64; 3], f32)>, String> {
        let spots: Vec<(f64, f64, f64)> = self.places.iter().map(|p| (p.x, p.y, p.z)).collect();
        let mut out = Vec::new();
        for &i in &self.ev.zones {
            let Some(z) = self.social.zones.get(i) else { continue };
            let y = zone_height(&spots, (z.x, z.z, z.radius), self.player).ok_or_else(|| format!("Para «{}» guarda un lugar dentro de la zona (hace falta su altura).", z.name))?;
            out.push((z.name.clone(), [z.x, y, z.z], z.radius as f32));
        }
        if out.is_empty() {
            return Err("Elige al menos una zona.".to_string());
        }
        Ok(out)
    }

    fn save_templates_file(&mut self) {
        if let Err(e) = spawners::save_templates(&self.dir, &self.ev.templates) {
            self.ev.notice = format!("Error: {e}");
        }
    }

    fn ready_templates(&self) -> Vec<RaidTemplate> {
        self.ev.templates.iter().filter(|t| !t.members.is_empty()).cloned().collect()
    }

    fn has_automatic(&self) -> bool {
        self.spawners.iter().any(|s| !s.pool.is_empty() && !s.once)
    }

    fn has_raids(&self) -> bool {
        self.spawners.iter().any(|s| s.once || !s.pool.is_empty())
    }

    /// Removes every raid (automatic ones and one-off ones) from the list and from the server.
    fn stop_all_raids(&mut self) {
        let ids: Vec<String> = self.spawners.iter().filter(|s| s.once || !s.pool.is_empty()).map(|s| s.id.clone()).collect();
        if ids.is_empty() {
            self.ev.notice = "No hay raids que detener.".to_string();
            return;
        }
        self.spawners.retain(|s| !ids.contains(&s.id));
        self.ev.unapplied = false;
        self.ev.notice = match spawners::save_change(&self.dir, &self.spawners, &spawners::op_delete_many(&ids)) {
            Ok(()) => format!("{} raid(s) detenidos y borrados del servidor.", ids.len()),
            Err(e) => format!("Error: {e}"),
        };
    }

    fn activate_auto(&mut self) {
        let list: Vec<RaidTemplate> = self.ready_templates().into_iter().filter(|t| t.active).collect();
        if list.is_empty() {
            self.ev.notice = "Marca al menos una plantilla con enemigos como «Usar en raids automáticos».".to_string();
            return;
        }
        let spots = match self.zone_spots() {
            Ok(s) => s,
            Err(e) => {
                self.ev.notice = e;
                return;
            }
        };
        let base_ms = spawners::now_ms();
        let wave: Vec<Spawner> = spots
            .iter()
            .enumerate()
            .map(|(n, (name, at, radius))| {
                let id = self.spawners.iter().find(|s| !s.pool.is_empty() && !s.once && &s.place == name).map(|s| s.id.clone()).unwrap_or_else(|| format!("s{}", base_ms + n as u128));
                raid_spawner(id, name, *at, *radius, self.ev.minutes.clamp(1, 1440) * 60, false, &list, &self.items.generic_pickup)
            })
            .collect();
        self.ev.unapplied = false;
        self.push_wave(wave, format!("Raids automáticos activos en {} zona(s), cada {} min.", spots.len(), self.ev.minutes));
    }

    /// Sends the edited templates to the automatic raids that are already running.
    fn apply_to_active(&mut self) {
        let list: Vec<RaidTemplate> = self.ready_templates().into_iter().filter(|t| t.active).collect();
        if list.is_empty() {
            let ids: Vec<String> = self.spawners.iter().filter(|s| !s.pool.is_empty() && !s.once).map(|s| s.id.clone()).collect();
            if ids.is_empty() {
                self.ev.notice = "No hay raids automáticos activos.".to_string();
                return;
            }
            self.spawners.retain(|s| !ids.contains(&s.id));
            self.ev.unapplied = false;
            self.ev.notice = match spawners::save_change(&self.dir, &self.spawners, &spawners::op_delete_many(&ids)) {
                Ok(()) => format!("No quedan plantillas automáticas: {} raid(s) automáticos detenidos.", ids.len()),
                Err(e) => format!("Error: {e}"),
            };
            return;
        }
        let mut wave = Vec::new();
        for s in self.spawners.iter().filter(|s| !s.pool.is_empty() && !s.once) {
            let mut s = s.clone();
            s.pool = list.clone();
            wave.push(s);
        }
        if wave.is_empty() {
            self.ev.notice = "No hay raids automáticos activos.".to_string();
            return;
        }
        let n = wave.len();
        self.ev.unapplied = false;
        self.push_wave(wave, format!("Cambios aplicados a {n} zona(s)."));
    }

    fn launch_now(&mut self, index: usize) {
        let Some(t) = self.ev.templates.get(index).cloned().filter(|t| !t.members.is_empty()) else {
            self.ev.notice = "Esa plantilla no tiene enemigos.".to_string();
            return;
        };
        let spots = match self.zone_spots() {
            Ok(s) => s,
            Err(e) => {
                self.ev.notice = e;
                return;
            }
        };
        let base_ms = spawners::now_ms();
        let wave: Vec<Spawner> = spots
            .iter()
            .enumerate()
            .map(|(n, (name, at, radius))| raid_spawner(format!("s{}", base_ms + n as u128), name, *at, *radius, 60, true, std::slice::from_ref(&t), &self.items.generic_pickup))
            .collect();
        self.push_wave(wave, format!("«{}» enviado a {} zona(s). Aparece si hay un jugador a menos de {} m.", t.name, spots.len(), spawners::RANGE_METERS as u32));
    }

    fn push_wave(&mut self, wave: Vec<Spawner>, ok: String) {
        let op = spawners::op_add_many(&wave);
        for w in wave {
            match self.spawners.iter().position(|s| s.id == w.id) {
                Some(i) => self.spawners[i] = w,
                None => self.spawners.push(w),
            }
        }
        self.ev.notice = match spawners::save_change(&self.dir, &self.spawners, &op) {
            Ok(()) => ok,
            Err(e) => format!("Error: {e}"),
        };
    }

    /// Raids that finished (or are too old) leave the list.
    pub fn clean_raids(&mut self) {
        let done = std::fs::read_to_string(self.dir.join(spawners::STATUS_FILE)).map(|t| spawners::parse_done(&t)).unwrap_or_default();
        let now = spawners::now_ms();
        let before = self.spawners.len();
        self.spawners.retain(|s| !(s.once && done.contains(&s.id)) && !spawners::expired(s, now));
        if self.spawners.len() != before {
            let _ = spawners::save(&self.dir, &self.spawners);
        }
    }

    pub fn events_tab(&mut self, ui: &mut egui::Ui) {
        if !self.ev.loaded {
            self.ev.loaded = true;
            self.ev.templates = spawners::load_templates(&self.dir);
            self.ev.selected = if self.ev.templates.is_empty() { None } else { Some(0) };
        }
        heading(ui, "Eventos y raids", 22.0);
        divider(ui);
        let mut action: Option<Action> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            let mut templates = std::mem::take(&mut self.ev.templates);
            let mut changed = false;
            ui.columns(2, |cols| {
                self.template_list(&mut cols[0], &mut templates, &mut changed, &mut action);
                self.template_editor(&mut cols[1], &mut templates, &mut changed);
            });
            self.ev.templates = templates;
            if changed {
                self.save_templates_file();
                if self.has_automatic() {
                    self.ev.unapplied = true;
                }
            }
            ui.add_space(10.0);
            divider(ui);
            self.launch_panel(ui, &mut action);
        });
        match action {
            Some(Action::Launch(i)) => self.launch_now(i),
            Some(Action::Activate) => self.activate_auto(),
            Some(Action::Apply) => self.apply_to_active(),
            Some(Action::Save) => self.save_templates_file(),
            None => {}
        }
    }

    fn template_list(&mut self, ui: &mut egui::Ui, templates: &mut Vec<RaidTemplate>, changed: &mut bool, action: &mut Option<Action>) {
        label_title(ui, &format!("PLANTILLAS ({}/{})", templates.len(), MAX_TEMPLATES));
        let mut remove = None;
        let mut copy = None;
        for (i, t) in templates.iter().enumerate() {
            let selected = self.ev.selected == Some(i);
            let stroke = if selected { GOLD } else { BRONZE };
            egui::Frame::none().fill(if selected { CARD_SELECTED } else { CARD }).stroke(egui::Stroke::new(if selected { 1.8_f32 } else { 1.0_f32 }, stroke)).rounding(4.0).inner_margin(egui::Margin::same(8.0)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let head = ui.horizontal(|ui| {
                    ui.label(RichText::new(if t.name.is_empty() { "(sin nombre)" } else { &t.name }).font(title_font(17.0)).color(PARCHMENT));
                    if t.active {
                        ui.label(RichText::new("· automático").color(GOOD).size(13.5));
                    }
                });
                let body = if t.members.is_empty() { "Sin enemigos todavía".to_string() } else { t.summary() };
                ui.label(RichText::new(body).color(MUTED).size(14.0));
                ui.horizontal_wrapped(|ui| {
                    if button(ui, "Editar", Vec2::new(70.0, 26.0), true, false) {
                        self.ev.selected = Some(i);
                        self.ev.loot_open = None;
                    }
                    if button(ui, "Lanzar ahora", Vec2::new(150.0, 26.0), !t.members.is_empty() && !self.ev.zones.is_empty(), true) {
                        *action = Some(Action::Launch(i));
                    }
                    if button(ui, "Duplicar", Vec2::new(80.0, 26.0), templates.len() < MAX_TEMPLATES, false) {
                        copy = Some(i);
                    }
                    if button(ui, "Borrar", Vec2::new(70.0, 26.0), true, false) {
                        remove = Some(i);
                    }
                });
                let _ = head;
            });
            ui.add_space(4.0);
        }
        if let Some(i) = copy {
            let mut t = templates[i].clone();
            t.name = format!("{} (copia)", t.name);
            templates.push(t);
            self.ev.selected = Some(templates.len() - 1);
            *changed = true;
        }
        if let Some(i) = remove {
            templates.remove(i);
            self.ev.selected = match self.ev.selected {
                Some(s) if s == i => None,
                Some(s) if s > i => Some(s - 1),
                other => other,
            };
            self.ev.loot_open = None;
            *changed = true;
        }
        if button(ui, "+ Nueva plantilla", Vec2::new(ui.available_width(), 34.0), templates.len() < MAX_TEMPLATES, true) {
            templates.push(RaidTemplate { name: format!("Plantilla {}", templates.len() + 1), members: Vec::new(), active: true });
            self.ev.selected = Some(templates.len() - 1);
            self.ev.loot_open = None;
            *changed = true;
        }
    }

    fn template_editor(&mut self, ui: &mut egui::Ui, templates: &mut [RaidTemplate], changed: &mut bool) {
        label_title(ui, "EDITAR PLANTILLA");
        let Some(index) = self.ev.selected.filter(|i| *i < templates.len()) else {
            ui.label(RichText::new("Crea una plantilla o elige una con «Editar».").color(MUTED));
            return;
        };
        ui.horizontal(|ui| {
            ui.label("Nombre");
            *changed |= ui.add(egui::TextEdit::singleline(&mut templates[index].name).desired_width(200.0)).changed();
        });
        *changed |= ui.checkbox(&mut templates[index].active, "Usar en los raids automáticos").changed();
        ui.add_space(4.0);

        let mut remove_member = None;
        let member_count = templates[index].members.len();
        for mi in 0..member_count {
            let open = self.ev.loot_open == Some(mi);
            egui::Frame::none().fill(CARD).stroke(egui::Stroke::new(1.0_f32, BRONZE)).rounding(4.0).inner_margin(egui::Margin::same(8.0)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let m = &mut templates[index].members[mi];
                ui.label(RichText::new(if m.creature.is_empty() { "Enemigo" } else { &m.creature }).font(title_font(16.0)).color(PARCHMENT));
                ui.horizontal_wrapped(|ui| {
                    ui.label("Cantidad");
                    *changed |= ui.add(egui::DragValue::new(&mut m.count).range(1..=20)).changed();
                    ui.label("Nivel");
                    *changed |= ui.add(egui::DragValue::new(&mut m.level).range(1..=1000)).changed();
                    ui.label("Daño ×");
                    *changed |= ui.add(egui::DragValue::new(&mut m.damage).range(0.1..=1000.0).speed(0.1)).changed();
                });
                ui.horizontal_wrapped(|ui| {
                    let label = if m.drops.is_empty() { "Botín: el normal del juego".to_string() } else { format!("Botín: {} objeto(s)", m.drops.len()) };
                    ui.label(RichText::new(label).color(GOLD_DIM).size(14.0));
                    if button(ui, if open { "Cerrar botín" } else { "Editar botín" }, Vec2::new(120.0, 24.0), true, false) {
                        self.ev.loot_open = if open { None } else { Some(mi) };
                    }
                    if button(ui, "Quitar enemigo", Vec2::new(130.0, 24.0), true, false) {
                        remove_member = Some(mi);
                    }
                });
                if open {
                    self.loot_editor(ui, &mut m.drops, changed);
                }
            });
            ui.add_space(4.0);
        }
        if let Some(mi) = remove_member {
            templates[index].members.remove(mi);
            self.ev.loot_open = None;
            *changed = true;
        }

        if member_count < MAX_MEMBERS {
            ui.label("Añadir enemigo");
            ui.add(egui::TextEdit::singleline(&mut self.ev.creature_query).hint_text("Buscar enemigo…").desired_width(240.0));
            let q = self.ev.creature_query.trim().to_lowercase();
            if !q.is_empty() {
                let found: Vec<(usize, String)> = self.creatures.entries.iter().enumerate().filter(|(_, e)| !e.internal && e.search.contains(&q)).take(8).map(|(i, e)| (i, e.alias.clone().unwrap_or_else(|| e.label.clone()))).collect();
                for (i, name) in found {
                    if ui.selectable_label(false, format!("+ {name}")).clicked() {
                        let guid = self.creatures.entries[i].guid.clone();
                        templates[index].members.push(new_member(name, guid));
                        self.ev.creature_query.clear();
                        *changed = true;
                    }
                }
            }
        } else {
            ui.label(RichText::new(format!("Máximo {MAX_MEMBERS} enemigos por plantilla.")).color(MUTED));
        }
    }

    pub(crate) fn loot_editor(&mut self, ui: &mut egui::Ui, drops: &mut Vec<Drop>, changed: &mut bool) {
        ui.separator();
        let mut remove = None;
        for (i, d) in drops.iter_mut().enumerate() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&d.name).color(PARCHMENT));
                *changed |= ui.add(egui::DragValue::new(&mut d.amount).range(1..=65535).prefix("× ")).changed();
                *changed |= ui.add(egui::DragValue::new(&mut d.chance).range(1..=100).suffix(" %")).changed();
                if button(ui, "Quitar", Vec2::new(64.0, 22.0), true, false) {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            drops.remove(i);
            *changed = true;
        }
        if drops.len() >= MAX_DROPS {
            ui.label(RichText::new(format!("Máximo {MAX_DROPS} objetos.")).color(MUTED));
            return;
        }
        ui.add(egui::TextEdit::singleline(&mut self.ev.item_query).hint_text("Buscar objeto…").desired_width(240.0));
        let q = self.ev.item_query.trim().to_lowercase();
        if !q.is_empty() {
            let found: Vec<(usize, String)> = self.item_cat.entries.iter().enumerate().filter(|(_, e)| e.search.contains(&q)).take(8).map(|(i, e)| (i, e.label.clone())).collect();
            for (i, name) in found {
                if ui.selectable_label(self.ev.item == Some(i), name).clicked() {
                    self.ev.item = Some(i);
                    if let Some(r) = self.items.rows.get(i) {
                        self.ev.item_level = r.give_level.clamp(1, 255);
                        self.ev.rarity = r.rarity_index();
                    }
                }
            }
        }
        if let Some(n) = self.ev.item.and_then(|i| self.item_name(i)) {
            ui.label(RichText::new(format!("Elegido: {n}")).color(GOOD));
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Cantidad");
            ui.add(egui::DragValue::new(&mut self.ev.amount).range(1..=65535));
            ui.label("Rareza");
            ui.add(egui::DragValue::new(&mut self.ev.rarity).range(0..=4));
            ui.label("Nivel");
            ui.add(egui::DragValue::new(&mut self.ev.item_level).range(1..=255));
            ui.label("Probabilidad (%)");
            ui.add(egui::DragValue::new(&mut self.ev.chance).range(1..=100));
        });
        if button(ui, "Añadir al botín", Vec2::new(170.0, 26.0), self.ev.item.is_some(), true) {
            if let Some(i) = self.ev.item {
                if let Some(item) = self.items.rows.get(i).filter(|r| r.deliverable()).cloned() {
                    let name = self.item_name(i).unwrap_or_else(|| item.name.clone());
                    drops.push(Drop { item: item.id, guid: item.guid.clone(), name, amount: self.ev.amount.clamp(1, 65535), rarity: self.ev.rarity.min(4), level: self.ev.item_level.clamp(1, 255), chance: self.ev.chance.clamp(1, 100) });
                    *changed = true;
                } else {
                    self.ev.notice = "Ese objeto no se puede entregar.".to_string();
                }
            }
        }
    }

    fn launch_panel(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        self.restore_choice();
        let before = (self.ev.zones.clone(), self.ev.minutes);
        label_title(ui, "ZONAS");
        if self.social.zones.is_empty() {
            ui.label(RichText::new("Crea zonas en la pestaña Zonas y bases.").color(MUTED));
        }
        ui.horizontal_wrapped(|ui| {
            for (i, z) in self.social.zones.iter().enumerate() {
                let mut on = self.ev.zones.contains(&i);
                if ui.checkbox(&mut on, format!("{} ({:.0} m)", z.name, z.radius)).changed() {
                    if on {
                        self.ev.zones.insert(i);
                    } else {
                        self.ev.zones.remove(&i);
                    }
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label("Raids automáticos cada (minutos)");
            ui.add(egui::Slider::new(&mut self.ev.minutes, 1..=1440));
        });
        if (self.ev.zones.clone(), self.ev.minutes) != before {
            self.save_choice();
        }
        let usable = self.ev.templates.iter().any(|t| t.active && !t.members.is_empty());
        if button(ui, "ACTIVAR RAIDS AUTOMÁTICOS EN LAS ZONAS ELEGIDAS", Vec2::new(ui.available_width(), 44.0), usable && !self.ev.zones.is_empty(), true) {
            *action = Some(Action::Activate);
        }
        if self.has_raids() {
            if button(ui, "DETENER TODOS LOS RAIDS", Vec2::new(ui.available_width(), 36.0), true, false) {
                self.stop_all_raids();
            }
        }
        if self.has_automatic() {
            let label = if self.ev.unapplied { "APLICAR LOS CAMBIOS A LOS RAIDS ACTIVOS (hay cambios sin aplicar)" } else { "Aplicar los cambios a los raids activos" };
            if button(ui, label, Vec2::new(ui.available_width(), 36.0), true, self.ev.unapplied) {
                *action = Some(Action::Apply);
            }
        }
        ui.label(RichText::new("Cada raid elige una plantilla al azar y la pone en un punto al azar dentro del radio de la zona, si hay un jugador a menos de 150 m. «Lanzar ahora» (en cada plantilla) la envía enseguida a las zonas elegidas. Borrar o desmarcar una plantilla no detiene los raids que ya están activos: pulsa «Aplicar los cambios» o «Detener todos los raids».").color(MUTED).size(14.0));
        if !self.ev.notice.is_empty() {
            ui.label(RichText::new(self.ev.notice.clone()).color(GOOD).size(15.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tpl(n: &str) -> RaidTemplate {
        RaidTemplate { name: n.into(), members: vec![new_member("Renegado".into(), format!("{n}-guid")), new_member("Lobo".into(), "lobo-guid".into())], active: true }
    }

    #[test]
    fn zone_height_prefers_a_saved_place_inside() {
        let places = [(100.0, 50.0, 100.0), (900.0, 7.0, 900.0)];
        assert_eq!(zone_height(&places, (105.0, 102.0, 30.0), Some([0.0, 99.0, 0.0])), Some(50.0));
        assert_eq!(zone_height(&places, (500.0, 500.0, 30.0), Some([0.0, 99.0, 0.0])), Some(99.0));
        assert_eq!(zone_height(&places, (500.0, 500.0, 30.0), None), None);
    }

    #[test]
    fn an_automatic_raid_carries_all_templates_and_the_zone_radius() {
        let s = raid_spawner("s9".into(), "Base", [1.0, 2.0, 3.0], 40.0, 3600, false, &[tpl("a"), tpl("b")], "pick");
        assert_eq!((s.every, s.area, s.pool.len()), (3600, 40.0, 2));
        assert_eq!(s.template, "a-guid");
        assert!(!s.once);
        let wire = spawners::op_add(&s);
        assert!(wire.contains("\"e\":3600") && wire.contains("\"a\":40"));
        assert!(wire.contains("\"u\":["));
    }

    #[test]
    fn launching_now_sends_one_template_that_disappears_when_dead() {
        let mut inactive = tpl("a");
        inactive.active = false;
        let s = raid_spawner("s1".into(), "Base", [0.0; 3], 40.0, 60, true, &[inactive], "pick");
        assert!(s.once);
        // a raid launched by hand is sent even if the template is not in the automatic rotation
        assert!(spawners::op_add(&s).contains("\"m\":[{\"u\""));
        let auto = raid_spawner("s2".into(), "Base", [0.0; 3], 40.0, 3600, false, &[tpl("x"), { let mut t = tpl("y"); t.active = false; t }], "pick");
        let wire = spawners::op_add(&auto);
        assert_eq!(wire.matches("\"u\":[").count(), 1);
    }

    #[test]
    fn a_template_summary_lists_each_enemy() {
        let mut t = tpl("a");
        t.members[0].count = 5;
        t.members[0].level = 40;
        assert_eq!(t.summary(), "5 × Renegado nv.40 + 3 × Lobo nv.50");
        assert_eq!(t.total(), 8);
    }
}
