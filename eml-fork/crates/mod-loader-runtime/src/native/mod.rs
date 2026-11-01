//! M182 native layer: small, signature-checked patches of the game's own code that
//! cannot be expressed as data changes (see `faction`).
//!
//! Safety rules for everything in here:
//! * Nothing is patched unless the target function matches its signature exactly once.
//! * Every failure path logs and returns; nothing panics (the release profile aborts).
//! * `m182_native.json` has a master switch (`"enabled": false`) that disables all of it.

pub mod chat;
pub mod config;
pub mod diag;
pub mod faction;
pub mod groups;
pub mod invites;
pub mod pillar_cfg;
pub mod roster;
mod signatures;

#[cfg(windows)]
mod guard;

#[cfg(windows)]
mod panel_host;

#[cfg(windows)]
mod pillar;

#[cfg(windows)]
mod patch;

#[cfg(windows)]
mod spawn_probe;

#[cfg(windows)]
mod spawner;

#[cfg(windows)]
mod table;

#[cfg(windows)]
mod veil_beam;

#[cfg(windows)]
mod vfx;

use std::path::PathBuf;

#[cfg(windows)]
pub fn attach() {
    use std::time::Duration;

    let dir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => {
            tracing::warn!(target: "m182::native", error = %e, "cannot determine game directory, native layer off");
            return;
        }
    };

    let path = config::config_path(&dir);
    let cfg = match config::load(&path) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(target: "m182::native", error = %e, file = ?path, "invalid config, native layer off");
            return;
        }
    };

    tracing::info!(target: "m182::native", ?cfg, file = ?path, "native layer config");

    if !cfg.enabled {
        tracing::info!(target: "m182::native", "disabled by config");
        return;
    }

    config::LIVE.apply(&cfg);

    if cfg.panel {
        panel_host::start(dir.clone());
    }

    vfx::start();

    if patch::install() != patch::Outcome::Retry {
        spawn_watcher(path);
        return;
    }

    // Code not readable yet (very early start): retry in the background.
    let spawned = std::thread::Builder::new()
        .name("m182-native-install".into())
        .spawn(move || {
            for _ in 0..240 {
                std::thread::sleep(Duration::from_millis(500));
                if patch::install() != patch::Outcome::Retry {
                    break;
                }
            }
            watch(path);
        });

    if let Err(e) = spawned {
        tracing::warn!(target: "m182::native", error = %e, "could not start installer thread");
    }
}

#[cfg(not(windows))]
pub fn attach() {}

#[cfg(windows)]
fn spawn_watcher(path: PathBuf) {
    if let Err(e) = std::thread::Builder::new()
        .name("m182-native-config".into())
        .spawn(move || watch(path))
    {
        tracing::warn!(target: "m182::native", error = %e, "could not start config watcher");
    }
}

/// Reloads `m182_native.json` when it changes so PvP can be toggled while playing.
#[cfg(windows)]
fn watch(path: PathBuf) {
    use std::time::Duration;

    let mut last = config::modified(&path);
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let now = config::modified(&path);
        if now == last {
            continue;
        }
        last = now;

        match config::load(&path) {
            Ok(cfg) => {
                config::LIVE.apply(&cfg);
                tracing::info!(target: "m182::native", pvp = config::LIVE.pvp(), friend_checks = config::LIVE.pvp_friend_checks(), "config reloaded");
            }
            Err(e) => {
                tracing::warn!(target: "m182::native", error = %e, "config reload failed, keeping previous settings");
            }
        }
    }
}
