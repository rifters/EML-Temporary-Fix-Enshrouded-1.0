//! M182 safety net around the data patching.
//!
//! The game refuses to start ("Could not find core resource package") when
//! `enshrouded.kfc` is damaged or points outside `enshrouded.kfc_resources`.
//! That can happen when a patch is cut half way (crash, antivirus lock, power loss)
//! or when it ends in a file the game cannot read. The backup is a pair
//! (`.kfc.bak` + `.kfc_resources.bak`): with append-only exports the `.kfc`
//! alone used to be enough, but the in-place writer rewrites stream data,
//! so the two halves are always restored together.

use std::{fs, path::Path};

use kfc::container::KFCFile;

use crate::log::{error, info, warn};

const MARKER: &str = "m182_patching";

fn marker(cache_dir: &Path) -> std::path::PathBuf {
    cache_dir.join(MARKER)
}

fn paths(game_dir: &Path, file_name: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    (
        game_dir.join(format!("{file_name}.kfc")),
        game_dir.join(format!("{file_name}.kfc.bak")),
        game_dir.join(format!("{file_name}.kfc_resources")),
    )
}

/// Marks that a patch is running. If the process dies, the mark stays.
pub fn begin(cache_dir: &Path) {
    let _ = fs::create_dir_all(cache_dir);
    let _ = fs::write(marker(cache_dir), b"patching");
}

pub fn end(cache_dir: &Path) {
    let _ = fs::remove_file(marker(cache_dir));
}

/// Is `kfc` something the game can open together with its resources and containers?
pub fn verify(game_dir: &Path, file_name: &str) -> Result<(), String> {
    let (kfc, _, res) = paths(game_dir, file_name);
    let file = KFCFile::from_path(&kfc, false).map_err(|e| format!("{} cannot be read: {e}", kfc.display()))?;
    let res_len = fs::metadata(&res).map_err(|e| format!("{} is missing: {e}", res.display()))?.len();

    for (i, chunk) in file.resource_chunks().iter().enumerate() {
        let used = if chunk.compressed_size > 0 { chunk.compressed_size } else { chunk.size };
        if chunk.offset.saturating_add(used) > res_len {
            return Err(format!("resource chunk {i} ends at {} but {} has only {res_len} bytes", chunk.offset + used, res.display()));
        }
    }

    // containers the vanilla game already had are the game's business; the ones a patch
    // added must exist, and no existing one may be shorter than the file says.
    let (_, bak, _) = paths(game_dir, file_name);
    let vanilla = KFCFile::from_path(&bak, false).map(|f| f.containers().len()).unwrap_or(usize::MAX);

    for (i, c) in file.containers().iter().enumerate() {
        let dat = game_dir.join(format!("{file_name}_{i:03}.dat"));
        match fs::metadata(&dat) {
            Err(_) if i >= vanilla => return Err(format!("{} is missing", dat.display())),
            Ok(m) if m.len() > 0 && m.len() < c.size => {
                return Err(format!("{} has {} bytes but the game file expects {}", dat.display(), m.len(), c.size));
            }
            _ => {}
        }
    }
    Ok(())
}

fn restore_original(game_dir: &Path, file_name: &str) -> bool {
    let (kfc, bak, res) = paths(game_dir, file_name);
    if KFCFile::get_version_tag(&bak).is_err() {
        warn!(path = ?bak, "No usable .kfc.bak to restore");
        return false;
    }
    match fs::copy(&bak, &kfc) {
        Ok(_) => {
            info!(path = ?kfc, "Original enshrouded.kfc restored from backup");

            // Restore the resources half of the pair as well, when one
            // was backed up. A .kfc restored on its own points into a
            // stream from another generation and the game refuses it.
            let res_bak = game_dir.join(format!("{file_name}.kfc_resources.bak"));

            if res_bak.is_file() {
                if let Err(e) = fs::copy(res_bak, &res) {
                    error!(error = %e, "Failed to restore original enshrouded.kfc_resources");
                    return false;
                }
                info!(path = ?res, "Original enshrouded.kfc_resources restored from backup");
            }

            true
        }
        Err(e) => {
            error!(error = %e, "Failed to restore original enshrouded.kfc");
            false
        }
    }
}

/// The next start must patch again.
fn forget_cache(cache_dir: &Path) {
    let _ = fs::remove_file(cache_dir.join("files.json"));
}

/// Failed patch: original kfc back, and ask for a fresh attempt next start.
pub fn rollback(game_dir: &Path, cache_dir: &Path, file_name: &str) {
    restore_original(game_dir, file_name);
    forget_cache(cache_dir);
    end(cache_dir);
}

/// Start of every run: undo what a previous interrupted or broken patch left behind.
pub fn recover(game_dir: &Path, file_name: &str) {
    let cache_dir = game_dir.join(".cache");
    let interrupted = marker(&cache_dir).exists();
    let broken = verify(game_dir, file_name);

    if interrupted || broken.is_err() {
        warn!(interrupted, broken = ?broken.as_ref().err(), "Game files look damaged by an earlier patch, restoring the original ones");
        if restore_original(game_dir, file_name) {
            forget_cache(&cache_dir);
        }
        end(&cache_dir);
    }
}
