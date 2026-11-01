//! Rust equivalents of the game's faction relation functions.
//!
//! Every function here is a straight re-statement of the game's own logic (verified
//! against the original machine code, see the tests) plus one optional rule:
//! with `pvp` enabled two entities of the `Player` faction are hostile to each other.

use std::sync::atomic::{AtomicU32, Ordering};

pub const PLAYER: u8 = 7;
pub const TEAM_FACTIONS: [u8; 2] = [8, 11];

static TEAM_MASK: AtomicU32 = AtomicU32::new(0);
static ZONE_PVP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Faction lent to players outside the always-PvP zones while PvP is off elsewhere. It is a
/// player-group faction that never counts as a hostile player pair.
pub const PEACE: u8 = 8;

/// At least one always-PvP zone exists and PvP is off for the rest of the world.
pub fn set_zone_pvp(on: bool) {
    ZONE_PVP.store(on, Ordering::Relaxed);
}

pub fn zone_pvp() -> bool {
    ZONE_PVP.load(Ordering::Relaxed)
}

/// PvP as the faction hooks must see it: the global switch or the always-PvP zones.
pub fn pvp_effective(global: bool) -> bool {
    global || zone_pvp()
}
static FRIENDLY_FIRE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Members of the same team can hurt each other (off by default: teams protect their members).
pub fn set_friendly_fire(on: bool) {
    FRIENDLY_FIRE.store(on, Ordering::Relaxed);
}

pub fn friendly_fire() -> bool {
    FRIENDLY_FIRE.load(Ordering::Relaxed)
}

/// Two members of the same lent team faction.
#[inline]
fn same_team_in(a: u8, b: u8, mask: u32) -> bool {
    a == b && TEAM_FACTIONS.contains(&a) && mask & (1u32 << a) != 0
}

/// Faction values currently lent to teams (bit n set = value n is a team).
pub fn set_team_mask(mask: u32) {
    TEAM_MASK.store(mask, Ordering::Relaxed);
}

pub fn team_mask() -> u32 {
    TEAM_MASK.load(Ordering::Relaxed)
}

#[inline]
fn player_group(x: u8) -> bool {
    // Player, AutomatedPlayer, PlayerAlly
    matches!(x, 7 | 8 | 11)
}

#[inline]
fn player_pair_in(a: u8, b: u8, mask: u32) -> bool {
    let known = |x: u8| x == PLAYER || (TEAM_FACTIONS.contains(&x) && mask & (1u32 << x) != 0);
    (a == PLAYER && b == PLAYER) || (a != b && known(a) && known(b))
}

#[inline]
fn player_pair(a: u8, b: u8) -> bool {
    player_pair_in(a, b, team_mask())
}

/// `isEnemy(a, b)` exactly as the game computes it.
pub fn enemy_vanilla(a: u8, b: u8) -> bool {
    if a == 0 {
        return false;
    }
    if player_group(a) && player_group(b) {
        return false;
    }
    if a == 1 {
        return true;
    }
    if a == b {
        return false;
    }
    match a {
        3 => b != 6,
        6 => !matches!(b, 3 | 10 | 13 | 14),
        10 => !matches!(b, 6 | 2),
        2 => b != 10,
        13 => !matches!(b, 5 | 6),
        5 => b != 13,
        14 => b != 6,
        _ => true,
    }
}

/// `isFriend(a, b)` exactly as the game computes it.
pub fn friend_vanilla(a: u8, b: u8) -> bool {
    a == b || (player_group(a) && player_group(b))
}

/// `isFriend(a, b, options)` exactly as the game computes it.
/// `flag` is the byte at `options + 1` (false when `options` is null).
pub fn friend_opts_vanilla(a: u8, b: u8, flag: bool) -> bool {
    if a == b {
        return true;
    }
    if player_group(a) {
        return player_group(b);
    }
    matches!(b, 7 | 8) && flag
}

pub fn enemy_in(a: u8, b: u8, pvp: bool, mask: u32) -> bool {
    if pvp && player_pair_in(a, b, mask) {
        return true;
    }
    enemy_vanilla(a, b)
}

pub fn enemy(a: u8, b: u8, pvp: bool) -> bool {
    if pvp && (player_pair(a, b) || (friendly_fire() && same_team_in(a, b, team_mask()))) {
        return true;
    }
    enemy_vanilla(a, b)
}

pub fn friend(a: u8, b: u8, pvp: bool) -> bool {
    if friendly_fire() && same_team_in(a, b, team_mask()) {
        return false;
    }
    if pvp && player_pair(a, b) {
        return false;
    }
    friend_vanilla(a, b)
}

pub fn friend_opts(a: u8, b: u8, flag: bool, pvp: bool) -> bool {
    if friendly_fire() && same_team_in(a, b, team_mask()) {
        return false;
    }
    if pvp && player_pair(a, b) {
        return false;
    }
    friend_opts_vanilla(a, b, flag)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Truth tables produced by running the game's original machine code in an emulator
    // for every input pair.
    static F1: &[u8] = include_bytes!("../../tests/golden/golden_f1.bin");
    static F2: &[u8] = include_bytes!("../../tests/golden/golden_f2.bin");
    static F2_NULL: &[u8] = include_bytes!("../../tests/golden/golden_f2null.bin");
    static F3: &[u8] = include_bytes!("../../tests/golden/golden_f3.bin");
    static F3_NULL: &[u8] = include_bytes!("../../tests/golden/golden_f3null.bin");
    static F4: &[u8] = include_bytes!("../../tests/golden/golden_f4.bin");

    fn ptr_value(null: bool, v: u8) -> u8 {
        if null { 0 } else { v }
    }

    #[test]
    fn enemy_matches_game_for_all_inputs() {
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                let i = a as usize * 256 + b as usize;
                assert_eq!(enemy_vanilla(a, b) as u8, F1[i], "F1 a={a} b={b}");
                assert_eq!(enemy_vanilla(a, b) as u8, F2[i], "F2 a={a} b={b}");
            }
        }
    }

    #[test]
    fn enemy_pointer_variant_null_handling() {
        assert_eq!(enemy_vanilla(0, 0) as u8, F2_NULL[0]);
        for b in 0..=255u8 {
            assert_eq!(enemy_vanilla(ptr_value(true, 0), b) as u8, F2_NULL[1 + b as usize]);
        }
        for a in 0..=255u8 {
            assert_eq!(enemy_vanilla(a, 0) as u8, F2_NULL[257 + a as usize]);
        }
    }

    #[test]
    fn friend_values_matches_game() {
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                assert_eq!(friend_vanilla(a, b) as u8, F4[a as usize * 256 + b as usize], "a={a} b={b}");
            }
        }
    }

    #[test]
    fn friend_opts_matches_game() {
        for opt in 0..3usize {
            let flag = opt == 2;
            for a in 0..=255u8 {
                for b in 0..=255u8 {
                    let i = (opt * 256 + a as usize) * 256 + b as usize;
                    assert_eq!(friend_opts_vanilla(a, b, flag) as u8, F3[i], "opt={opt} a={a} b={b}");
                }
            }
            let base = opt * 513;
            for b in 0..=255u8 {
                assert_eq!(friend_opts_vanilla(0, b, flag) as u8, F3_NULL[base + b as usize]);
            }
            for a in 0..=255u8 {
                assert_eq!(friend_opts_vanilla(a, 0, flag) as u8, F3_NULL[base + 256 + a as usize]);
            }
            assert_eq!(friend_opts_vanilla(0, 0, flag) as u8, F3_NULL[base + 512]);
        }
    }

    #[test]
    fn teammates_share_a_faction_and_everyone_else_stays_hostile() {
        let team_a = 1u32 << 8;
        let both = (1u32 << 8) | (1u32 << 11);
        assert!(!enemy_in(8, 8, true, team_a));
        assert!(enemy_in(8, PLAYER, true, team_a));
        assert!(enemy_in(PLAYER, 8, true, team_a));
        assert!(enemy_in(PLAYER, PLAYER, true, team_a));
        assert!(!enemy_in(11, PLAYER, true, team_a));
        assert!(!enemy_in(11, 11, true, both));
        assert!(enemy_in(8, 11, true, both));
        assert!(enemy_in(11, 8, true, both));
        assert!(enemy_in(11, PLAYER, true, both));
        assert!(!enemy_in(8, 8, false, both));
        assert!(!enemy_in(8, 11, false, both));
        assert!(enemy_in(4, 8, true, both));
        assert!(enemy_in(8, 4, true, both));
    }

    #[test]
    fn friendly_fire_makes_only_teammates_hostile() {
        let mask = 1u32 << 8;
        assert!(!same_team_in(PLAYER, PLAYER, mask));
        assert!(same_team_in(8, 8, mask));
        assert!(!same_team_in(8, 8, 0));
        assert!(!same_team_in(8, 11, (1 << 8) | (1 << 11)));
        assert!(!same_team_in(4, 4, 1 << 4));
    }

    #[test]
    fn pvp_only_changes_player_vs_player() {
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                let special = a == PLAYER && b == PLAYER;
                if !special {
                    assert_eq!(enemy(a, b, true), enemy_vanilla(a, b));
                    assert_eq!(friend(a, b, true), friend_vanilla(a, b));
                    assert_eq!(friend_opts(a, b, true, true), friend_opts_vanilla(a, b, true));
                }
                assert_eq!(enemy(a, b, false), enemy_vanilla(a, b));
                assert_eq!(friend(a, b, false), friend_vanilla(a, b));
            }
        }
        assert!(enemy(PLAYER, PLAYER, true));
        assert!(!friend(PLAYER, PLAYER, true));
        assert!(!enemy(PLAYER, PLAYER, false));
        // companions and automated players stay friendly with players
        assert!(!enemy(11, PLAYER, true));
        assert!(!enemy(PLAYER, 11, true));
        assert!(!enemy(8, PLAYER, true));
        // monsters still hostile to players
        assert!(enemy(4, PLAYER, true));
    }

    #[test]
    fn zone_pvp_hurts_only_players_inside_the_zone() {
        // inside: PLAYER vs PLAYER; outside: PEACE vs anyone
        assert!(enemy_in(PLAYER, PLAYER, true, 0));
        assert!(!enemy_in(PEACE, PLAYER, true, 0));
        assert!(!enemy_in(PEACE, PEACE, true, 0));
        assert!(!enemy_in(PLAYER, PEACE, true, 0));
        // creatures stay hostile to both
        assert!(enemy_in(1, PEACE, true, 0));
        assert!(enemy_in(PEACE, 3, true, 0));
    }

    #[test]
    fn zone_pvp_team_inside_does_not_hurt_itself() {
        let mask = 1u32 << 11;
        assert!(!enemy_in(11, 11, true, mask));
        assert!(enemy_in(11, PLAYER, true, mask));
        assert!(!enemy_in(PEACE, 11, true, mask));
    }
}
