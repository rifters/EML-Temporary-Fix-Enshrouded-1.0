use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::{config::LIVE, faction, groups, signatures};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Finished (patched, or a signature is missing and was reported).
    Done,
    /// Game code was not readable yet.
    Retry,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GetCurrentProcess() -> *mut c_void;
    fn VirtualProtect(addr: *mut c_void, size: usize, new: u32, old: *mut u32) -> i32;
    fn VirtualAlloc(addr: *mut c_void, size: usize, kind: u32, protect: u32) -> *mut c_void;
    fn FlushInstructionCache(process: *mut c_void, addr: *const c_void, size: usize) -> i32;
}

const PAGE_EXECUTE_READWRITE: u32 = 0x40;
const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const JUMP_LEN: usize = 14;
const MEM_COMMIT_RESERVE: u32 = 0x3000;

static INSTALLED: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------- hooks

#[inline]
fn read_faction(ptr: *const u8) -> u8 {
    if ptr.is_null() { 0 } else { unsafe { ptr.read() } }
}

static TEAM_CHECKS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

static ZONE_PAIRS: std::sync::Mutex<Vec<(u8, u8, bool)>> = std::sync::Mutex::new(Vec::new());

fn team_check_report(a: u8, b: u8, hostile: bool, via: &str) {
    if faction::zone_pvp() && matches!(a, 7 | 8 | 11) && matches!(b, 7 | 8 | 11) {
        if let Ok(mut seen) = ZONE_PAIRS.lock() {
            if !seen.contains(&(a, b, hostile)) && seen.len() < 40 {
                seen.push((a, b, hostile));
                let line = format!("pvp zones: relation check {via} {a} vs {b} -> {}", if hostile { "hostile" } else { "not hostile" });
                super::spawner::log(&line);
                super::diag::line(&line);
            }
        }
    }
    let teams = faction::TEAM_FACTIONS;
    if (teams.contains(&a) || teams.contains(&b)) && faction::team_mask() != 0 && TEAM_CHECKS.fetch_add(1, Ordering::Relaxed) < 30 {
        super::spawner::log(&format!("teams: relation check {via} {a} vs {b} -> {}", if hostile { "hostile" } else { "not hostile" }));
    }
}

extern "win64" fn enemy_values(rcx: usize, rdx: usize) -> u8 {
    let (a, b) = (rcx as u8, rdx as u8);
    let hostile = faction::enemy(a, b, faction::pvp_effective(LIVE.pvp()));
    team_check_report(a, b, hostile, "values");
    hostile as u8
}

extern "win64" fn enemy_ptrs(a: *const u8, b: *const u8) -> u8 {
    let (fa, fb) = (read_faction(a), read_faction(b));
    if groups::is_protected(a as usize) || groups::is_protected(b as usize) {
        if let Some(line) = groups::protected_report(a as usize, b as usize, fa, fb) {
            super::spawner::log(&line);
        }
        return 0;
    }
    let hostile = faction::enemy(fa, fb, faction::pvp_effective(LIVE.pvp()));
    team_check_report(fa, fb, hostile, "pointers");
    hostile as u8
}

extern "win64" fn friend_values(rcx: usize, rdx: usize) -> u8 {
    faction::friend(rcx as u8, rdx as u8, LIVE.pvp_friend_checks()) as u8
}

extern "win64" fn friend_opts(a: *const u8, b: *const u8, opts: *const u8) -> u8 {
    let flag = !opts.is_null() && unsafe { opts.add(1).read() } != 0;
    let (fa, fb) = (read_faction(a), read_faction(b));
    faction::friend_opts(fa, fb, flag, LIVE.pvp_friend_checks()) as u8
}

// The game may rely on registers that the Windows ABI treats as volatile (it was
// built with whole-program optimisation), so the stubs preserve all of them.
macro_rules! stub {
    ($name:ident, $target:path) => {
        #[unsafe(naked)]
        unsafe extern "win64" fn $name() {
            core::arch::naked_asm!(
                "push rcx",
                "push rdx",
                "push r8",
                "push r9",
                "push r10",
                "push r11",
                "sub rsp, 0x88",
                "movups [rsp + 0x20], xmm0",
                "movups [rsp + 0x30], xmm1",
                "movups [rsp + 0x40], xmm2",
                "movups [rsp + 0x50], xmm3",
                "movups [rsp + 0x60], xmm4",
                "movups [rsp + 0x70], xmm5",
                "call {f}",
                "movups xmm0, [rsp + 0x20]",
                "movups xmm1, [rsp + 0x30]",
                "movups xmm2, [rsp + 0x40]",
                "movups xmm3, [rsp + 0x50]",
                "movups xmm4, [rsp + 0x60]",
                "movups xmm5, [rsp + 0x70]",
                "add rsp, 0x88",
                "pop r11",
                "pop r10",
                "pop r9",
                "pop r8",
                "pop rdx",
                "pop rcx",
                "ret",
                f = sym $target,
            );
        }
    };
}

stub!(stub_enemy_values, enemy_values);
stub!(stub_enemy_ptrs, enemy_ptrs);
stub!(stub_friend_values, friend_values);
stub!(stub_friend_opts, friend_opts);


// ---------------------------------------------------------------- chat commands

pub(super) static CHAT_TRAMPOLINE: AtomicUsize = AtomicUsize::new(0);
pub(super) static PUMP_TRAMPOLINE: AtomicUsize = AtomicUsize::new(0);
/// The server's player table, captured the first time the game runs its per-tick chat pump.
pub(super) static PLAYERS_OBJECT: AtomicUsize = AtomicUsize::new(0);

static REPLY: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

static PERMISSIONS_RESTRICTED: AtomicU64 = AtomicU64::new(0);
static PERMISSIONS_HOOKED: AtomicBool = AtomicBool::new(false);

pub fn permissions_hooked() -> bool {
    PERMISSIONS_HOOKED.load(Ordering::SeqCst)
}

/// Entities (1-based, up to 64) whose permissions the game must see without chests and building.
pub fn restrict_permissions(entities: u64) {
    PERMISSIONS_RESTRICTED.store(entities, Ordering::SeqCst);
}

pub fn restricted_permissions() -> u64 {
    PERMISSIONS_RESTRICTED.load(Ordering::SeqCst)
}

extern "win64" fn filter_permissions(entity: u32, permissions: u32, caller: usize) -> u32 {
    let bit = entity.wrapping_sub(1);
    let shown = if bit < 64 && PERMISSIONS_RESTRICTED.load(Ordering::Relaxed) >> bit & 1 != 0 {
        super::roster::visitor_permissions(permissions as u8) as u32
    } else {
        permissions
    };
    if super::diag::on() {
        let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
        super::diag::permission(entity, permissions, shown, caller.wrapping_sub(base));
    }
    shown
}

// Tail of the game's permission lookup: reads the stored byte, lets `filter_permissions` narrow it
// for visitors and finishes the way the original did.
#[unsafe(naked)]
unsafe extern "win64" fn stub_permissions() {
    core::arch::naked_asm!(
        "movzx eax, byte ptr [r9 + r8 + 0x9d]",
        "push rcx",
        "push rdx",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "sub rsp, 0x88",
        "movups [rsp + 0x20], xmm0",
        "movups [rsp + 0x30], xmm1",
        "movups [rsp + 0x40], xmm2",
        "movups [rsp + 0x50], xmm3",
        "movups [rsp + 0x60], xmm4",
        "movups [rsp + 0x70], xmm5",
        "mov ecx, edx",
        "mov edx, eax",
        "mov r8, [rsp + 0xb8]",
        "call {f}",
        "movups xmm0, [rsp + 0x20]",
        "movups xmm1, [rsp + 0x30]",
        "movups xmm2, [rsp + 0x40]",
        "movups xmm3, [rsp + 0x50]",
        "movups xmm4, [rsp + 0x60]",
        "movups xmm5, [rsp + 0x70]",
        "add rsp, 0x88",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdx",
        "pop rcx",
        "mov byte ptr [rcx], al",
        "mov rax, rcx",
        "ret",
        f = sym filter_permissions,
    );
}

/// Replaces the text of the message being queued, so the game delivers our line instead.
fn replace_text(text: usize, line: &str) -> bool {
    let Ok(mut buffer) = REPLY.lock() else { return false };
    buffer.clear();
    buffer.reserve(1100);
    let bytes = line.as_bytes();
    buffer.extend_from_slice(&bytes[..bytes.len().min(0x3f0)]);
    let (pointer, length) = (buffer.as_ptr() as u64, buffer.len() as u64);
    let mut head = [0u8; 16];
    head[0..8].copy_from_slice(&pointer.to_le_bytes());
    head[8..16].copy_from_slice(&length.to_le_bytes());
    super::spawner::write_bytes(text, &head)
}

/// `!equipo ...`: the answer replaces the typed line, so everyone in the chat sees it.
fn team_command(text: usize, message: &str, client: u32) -> u8 {
    let asking = (client & 0x3f) + 1;
    let online: Vec<super::invites::Person> = super::table::entries()
        .into_iter()
        .map(|e| super::invites::Person { entity: e.entity(), name: e.name.clone().unwrap_or_else(|| format!("ID {}", e.entity())), id: e.id.clone() })
        .collect();
    let Some(who) = online.iter().find(|p| p.entity == asking).cloned() else { return 1 };
    let Ok(dir) = std::env::current_dir() else { return 1 };
    let Some(line) = super::invites::handle(message, &who, &online, &dir) else { return 1 };
    super::spawner::log(&format!("teams: {} -> {line}", who.name));
    if line.contains("cannot find") || line.contains("No encuentro") {
        super::spawner::log(&format!("teams: online now {:?}; typed \"{}\"", online.iter().map(|p| (p.entity, p.name.clone())).collect::<Vec<_>>(), message.trim()));
    }
    if replace_text(text, &line) { 0 } else { 1 }
}

extern "win64" fn hook_chat(_out: usize, _context: usize, _kind: usize, text: usize, client: usize) -> u8 {
    let key = LIVE.chat_key();
    if key.is_empty() {
        return 0;
    }
    let Some(head) = super::spawn_probe::read_bytes(text, 16) else { return 0 };
    let (pointer, length) = (super::spawn_probe::u64_at(&head, 0) as usize, super::spawn_probe::u64_at(&head, 8) as usize);
    if length == 0 || length > 0x400 {
        return 0;
    }
    let Some(bytes) = super::spawn_probe::read_bytes(pointer, length) else { return 0 };
    let message = String::from_utf8_lossy(&bytes);
    if super::invites::is_command(&message) {
        return team_command(text, &message, client as u32);
    }
    if !super::chat::has_prefix(&message) {
        super::chat::note_other(&message, client as u32);
        return 0;
    }
    if LIVE.chat_admin_only() {
        let table = super::table::entries();
        match table.iter().find(|e| e.client == client as u32) {
            Some(entry) if super::roster::is_admin(entry.permissions) => {}
            Some(entry) => {
                super::spawner::log(&format!("chat: client {} is not an admin (permissions {:#04x}); command ignored", client as u32, entry.permissions));
                return 1;
            }
            None => {
                super::spawner::log(&format!("chat: client {} not found in the player table; command ignored", client as u32));
                return 1;
            }
        }
    }
    let Ok(dir) = std::env::current_dir() else { return 0 };
    match super::chat::handle(&message, &key, client as u32, &dir) {
        super::chat::Verdict::Pass => 0,
        super::chat::Verdict::Swallowed(line) => {
            super::spawner::log(&line);
            1
        }
        super::chat::Verdict::Teams(seq) => {
            let line = super::spawner::teams_line(seq);
            let sent = replace_text(text, &line);
            super::spawner::log(&format!("chat: client {} asked for the teams ({}): {line}", client as u32, if sent { "sent" } else { "could not be sent" }));
            if sent { 0 } else { 1 }
        }
        super::chat::Verdict::Roster(seq) => {
            let line = super::spawner::roster_line(seq, client as u32);
            let sent = replace_text(text, &line);
            super::spawner::log(&format!("chat: client {} asked for the player list ({}): {line}", client as u32, if sent { "sent" } else { "could not be sent" }));
            if sent { 0 } else { 1 }
        }
    }
}

// Entry of the game's "enqueue chat message". When the message is one of ours it is
// dropped and the caller gets the same "ok" result a delivered message would give.
#[unsafe(naked)]
unsafe extern "win64" fn stub_chat() {
    core::arch::naked_asm!(
        "push rcx",
        "push rdx",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push rax",
        "sub rsp, 0x90",
        "movups [rsp + 0x30], xmm0",
        "movups [rsp + 0x40], xmm1",
        "movups [rsp + 0x50], xmm2",
        "movups [rsp + 0x60], xmm3",
        "movups [rsp + 0x70], xmm4",
        "movups [rsp + 0x80], xmm5",
        "mov eax, dword ptr [rsp + 0x100]",
        "mov dword ptr [rsp + 0x20], eax",
        "call {f}",
        "movzx eax, al",
        "mov dword ptr [rsp + 0x28], eax",
        "movups xmm0, [rsp + 0x30]",
        "movups xmm1, [rsp + 0x40]",
        "movups xmm2, [rsp + 0x50]",
        "movups xmm3, [rsp + 0x60]",
        "movups xmm4, [rsp + 0x70]",
        "movups xmm5, [rsp + 0x80]",
        "mov eax, dword ptr [rsp + 0x28]",
        "test eax, eax",
        "lea rsp, [rsp + 0x90]",
        "pop rax",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdx",
        "pop rcx",
        "jnz 2f",
        "jmp qword ptr [rip + {t}]",
        "2:",
        "mov byte ptr [rcx], 0",
        "mov dword ptr [rcx + 4], 0",
        "mov rax, rcx",
        "ret",
        f = sym hook_chat,
        t = sym CHAT_TRAMPOLINE,
    );
}

// Entry of the server's per-tick chat pump: remembers the player table (rcx), lets the M182 VFX
// module create its queued effects on this server tick, and goes on.
#[unsafe(naked)]
unsafe extern "win64" fn stub_pump() {
    core::arch::naked_asm!(
        "mov qword ptr [rip + {object}], rcx",
        "push rcx",
        "push rdx",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push rax",
        "sub rsp, 0x90",
        "movups [rsp + 0x30], xmm0",
        "movups [rsp + 0x40], xmm1",
        "movups [rsp + 0x50], xmm2",
        "movups [rsp + 0x60], xmm3",
        "movups [rsp + 0x70], xmm4",
        "movups [rsp + 0x80], xmm5",
        "call {f}",
        "movups xmm0, [rsp + 0x30]",
        "movups xmm1, [rsp + 0x40]",
        "movups xmm2, [rsp + 0x50]",
        "movups xmm3, [rsp + 0x60]",
        "movups xmm4, [rsp + 0x70]",
        "movups xmm5, [rsp + 0x80]",
        "lea rsp, [rsp + 0x90]",
        "pop rax",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdx",
        "pop rcx",
        "jmp qword ptr [rip + {t}]",
        object = sym PLAYERS_OBJECT,
        f = sym hook_pump,
        t = sym PUMP_TRAMPOLINE,
    );
}

extern "win64" fn hook_pump() {
    super::vfx::server_tick();
    super::veil_beam::tick();
}

fn install_pump(sections: &[&'static [u8]]) {
    let target = match find_unique(sections, &signatures::PLAYERS_PUMP) {
        Ok(addr) => addr,
        Err(count) => {
            super::spawner::log(&format!("player table: pump signature matched {count} times; names and base permissions off"));
            return;
        }
    };
    let page = unsafe { VirtualAlloc(std::ptr::null_mut(), 0x1000, MEM_COMMIT_RESERVE, PAGE_EXECUTE_READWRITE) } as *mut u8;
    if page.is_null() {
        super::spawner::log("player table: trampoline allocation failed");
        return;
    }
    let displaced = signatures::PLAYERS_PUMP_DISPLACED;
    unsafe {
        std::ptr::copy_nonoverlapping(target, page, displaced);
        let tail = page.add(displaced);
        tail.write(0xFF);
        tail.add(1).write(0x25);
        tail.add(2).cast::<u32>().write_unaligned(0);
        tail.add(6).cast::<u64>().write_unaligned(target.add(displaced) as u64);
    }
    PUMP_TRAMPOLINE.store(page as usize, Ordering::SeqCst);
    match unsafe { write_jump(target, stub_pump as usize) } {
        Ok(()) => super::spawner::log("player table hooked"),
        Err(code) => {
            PUMP_TRAMPOLINE.store(0, Ordering::SeqCst);
            super::spawner::log(&format!("player table: VirtualProtect failed ({code})"));
        }
    }
}

fn install_permissions(sections: &[&'static [u8]]) {
    let target = match find_unique(sections, &signatures::PLAYER_PERMISSIONS) {
        Ok(addr) => addr,
        Err(count) => {
            super::spawner::log(&format!("permissions: lookup signature matched {count} times; base visitors fall back to the player table"));
            return;
        }
    };
    match unsafe { write_jump(target.add(signatures::PLAYER_PERMISSIONS_AT), stub_permissions as usize) } {
        Ok(()) => {
            PERMISSIONS_HOOKED.store(true, Ordering::SeqCst);
            super::spawner::log("permissions hooked");
        }
        Err(code) => super::spawner::log(&format!("permissions: VirtualProtect failed ({code})")),
    }
}

fn install_chat(sections: &[&'static [u8]]) {
    if LIVE.chat_key().is_empty() {
        return;
    }
    let target = match find_unique(sections, &signatures::CHAT_ENQUEUE) {
        Ok(addr) => addr,
        Err(count) => {
            tracing::warn!(target: "m182::native", matches = count, "chat function signature not found exactly once; chat commands off");
            super::spawner::log(&format!("chat commands off: chat function signature matched {count} times"));
            return;
        }
    };

    let page = unsafe { VirtualAlloc(std::ptr::null_mut(), 0x1000, MEM_COMMIT_RESERVE, PAGE_EXECUTE_READWRITE) } as *mut u8;
    if page.is_null() {
        tracing::warn!(target: "m182::native", "chat trampoline allocation failed; chat commands off");
        return;
    }
    let displaced = signatures::CHAT_DISPLACED;
    unsafe {
        std::ptr::copy_nonoverlapping(target, page, displaced);
        let tail = page.add(displaced);
        tail.write(0xFF);
        tail.add(1).write(0x25);
        tail.add(2).cast::<u32>().write_unaligned(0);
        tail.add(6).cast::<u64>().write_unaligned(target.add(displaced) as u64);
    }
    CHAT_TRAMPOLINE.store(page as usize, Ordering::SeqCst);

    match unsafe { write_jump(target, stub_chat as usize) } {
        Ok(()) => {
            tracing::info!(target: "m182::native", address = format_args!("{:#x}", target as usize), "chat commands hooked");
            super::spawner::log("chat commands ready");
        }
        Err(code) => {
            CHAT_TRAMPOLINE.store(0, Ordering::SeqCst);
            tracing::error!(target: "m182::native", code, "VirtualProtect failed; chat commands off");
        }
    }
}

// ---------------------------------------------------------------- patching

struct Target {
    name: &'static str,
    signature: &'static [u8],
    hook: unsafe extern "win64" fn(),
}

pub fn install() -> Outcome {
    if INSTALLED.load(Ordering::SeqCst) {
        return Outcome::Done;
    }

    let Some(sections) = executable_sections() else {
        return Outcome::Retry;
    };

    let vfx = super::vfx::wanted();
    let pillar = super::pillar::wanted();
    if super::config::LIVE.spawn_probe() || super::config::LIVE.spawner() || vfx || pillar {
        super::spawn_probe::install(&sections);
    }

    super::guard::install(&sections);
    install_chat(&sections);
    if super::config::LIVE.spawner() || vfx || pillar {
        install_pump(&sections);
    }
    if super::config::LIVE.spawner() {
        install_permissions(&sections);
    }

    let targets = [
        Target { name: "isEnemy(values)", signature: &signatures::IS_ENEMY_VALUES, hook: stub_enemy_values },
        Target { name: "isEnemy(pointers)", signature: &signatures::IS_ENEMY_PTRS, hook: stub_enemy_ptrs },
        Target { name: "isFriend(values)", signature: &signatures::IS_FRIEND_VALUES, hook: stub_friend_values },
        Target { name: "isFriend(options)", signature: &signatures::IS_FRIEND_OPTS, hook: stub_friend_opts },
    ];

    // Resolve everything first; only patch when the complete set is present so we never
    // end up with a half-hooked, inconsistent faction logic.
    let mut found = Vec::with_capacity(targets.len());
    for target in &targets {
        match find_unique(&sections, target.signature) {
            Ok(addr) => found.push((target, addr)),
            Err(count) => {
                tracing::warn!(target: "m182::native", function = target.name, matches = count, "signature not found exactly once; game version changed? PvP patch not applied");
                INSTALLED.store(true, Ordering::SeqCst);
                return Outcome::Done;
            }
        }
    }

    for (target, addr) in &found {
        match unsafe { write_jump(*addr, target.hook as usize) } {
            Ok(()) => {
                tracing::info!(target: "m182::native", function = target.name, address = format_args!("{:#x}", *addr as usize), "hooked");
            }
            Err(code) => {
                tracing::error!(target: "m182::native", function = target.name, code, "VirtualProtect failed");
                INSTALLED.store(true, Ordering::SeqCst);
                return Outcome::Done;
            }
        }
    }

    INSTALLED.store(true, Ordering::SeqCst);
    tracing::info!(target: "m182::native", pvp = LIVE.pvp(), "faction hooks installed");
    Outcome::Done
}

pub(super) unsafe fn write_jump(at: *mut u8, destination: usize) -> Result<(), u32> {
    let mut jump = [0u8; JUMP_LEN];
    // jmp qword ptr [rip + 0]
    jump[..6].copy_from_slice(&[0xFF, 0x25, 0, 0, 0, 0]);
    jump[6..].copy_from_slice(&(destination as u64).to_le_bytes());

    let mut old = 0u32;
    if unsafe { VirtualProtect(at.cast(), JUMP_LEN, PAGE_EXECUTE_READWRITE, &mut old) } == 0 {
        return Err(1);
    }
    unsafe { std::ptr::copy_nonoverlapping(jump.as_ptr(), at, JUMP_LEN) };
    let mut ignored = 0u32;
    unsafe {
        VirtualProtect(at.cast(), JUMP_LEN, old, &mut ignored);
        FlushInstructionCache(GetCurrentProcess(), at.cast(), JUMP_LEN);
    }
    Ok(())
}

pub(super) fn find_unique(sections: &[&'static [u8]], signature: &[u8]) -> Result<*mut u8, usize> {
    let mut hit: Option<*mut u8> = None;
    let mut count = 0usize;
    let first = signature[0];

    for section in sections {
        if section.len() < signature.len() {
            continue;
        }
        let last_start = section.len() - signature.len();
        let mut i = 0usize;
        while i <= last_start {
            if section[i] == first && &section[i..i + signature.len()] == signature {
                count += 1;
                hit = Some(unsafe { section.as_ptr().add(i) } as *mut u8);
                i += signature.len();
            } else {
                i += 1;
            }
        }
    }

    match (count, hit) {
        (1, Some(addr)) => Ok(addr),
        _ => Err(count),
    }
}

/// Executable sections of the main module, or `None` if the header looks wrong.
fn executable_sections() -> Option<Vec<&'static [u8]>> {
    unsafe {
        let base = GetModuleHandleW(std::ptr::null()) as *const u8;
        if base.is_null() {
            return None;
        }
        if base.cast::<u16>().read_unaligned() != 0x5A4D {
            return None;
        }
        let nt = base.add(base.add(0x3C).cast::<u32>().read_unaligned() as usize);
        if nt.cast::<u32>().read_unaligned() != 0x0000_4550 {
            return None;
        }
        let section_count = nt.add(6).cast::<u16>().read_unaligned() as usize;
        let optional_size = nt.add(20).cast::<u16>().read_unaligned() as usize;
        let table = nt.add(24 + optional_size);

        let mut sections = Vec::new();
        for i in 0..section_count.min(96) {
            let header = table.add(i * 40);
            let virtual_size = header.add(8).cast::<u32>().read_unaligned() as usize;
            let virtual_address = header.add(12).cast::<u32>().read_unaligned() as usize;
            let characteristics = header.add(36).cast::<u32>().read_unaligned();
            if characteristics & SCN_MEM_EXECUTE != 0 && virtual_size > 0 {
                sections.push(std::slice::from_raw_parts(base.add(virtual_address), virtual_size));
            }
        }
        if sections.is_empty() { None } else { Some(sections) }
    }
}
