use std::{fs::File, io::BufReader, rc::Rc};

use kfc::{container::{KFCCursor, KFCFile, KFCReader, KFCReaderOptions, KFCWriteOptions, KFCWriter}, reflection::TypeRegistry};

use crate::{alias::{Path, PathBuf}, log::{debug, error, info, warn}};

pub fn load_kfc_file(
    kfc_path: impl AsRef<Path>
) -> Result<Rc<KFCFile>, ()> {
    let kfc_path = kfc_path.as_ref();

    match KFCFile::from_path(kfc_path, false) {
        Ok(file) => Ok(Rc::new(file)),
        Err(e) => {
            error!(
                error = %e,
                path = ?kfc_path,
                "Failed to open KFC file",
            );
            Err(())
        }
    }
}

pub fn create_reader(
    dir: &Path,
    file_name: &str,
) -> Result<KFCCursor<KFCReader>, ()> {
    match KFCReader::new_with_options(
        dir,
        file_name,
        KFCReaderOptions {
            kfc_extension: "kfc.bak",
            ..Default::default()
        }
    ).and_then(|reader| reader.into_cursor()) {
        Ok(reader) => Ok(reader),
        Err(e) => {
            error!(
                path = ?dir,
                file_name = %file_name,
                error = %e,
                "Failed to create KFC reader",
            );
            Err(())
        }
    }
}

pub fn create_writer(
    dir: &Path,
    file_name: &str,
    type_registry: &Rc<TypeRegistry>,
    ref_file: &Rc<KFCFile>,
) -> Result<KFCWriter<Rc<KFCFile>, Rc<TypeRegistry>>, ()> {
    let writer = match KFCWriter::new_incremental_with_options(
        dir,
        file_name,
        ref_file.clone(),
        type_registry.clone(),
        KFCWriteOptions {
            overwrite_containers: true,
            in_place: true,
            ..Default::default()
        }
    ) {
        Ok(writer) => writer,
        Err(e) => {
            error!(
                path = ?dir,
                file_name = %file_name,
                error = %e,
                "Failed to create KFC writer",
            );
            return Err(());
        }
    };

    Ok(writer)
}

/// The resources stream that pairs with a .kfc file, and that stream's
/// own backup path. Backups are pairs: the .kfc directory and the stream
/// only describe the same generation together, so one is never backed up
/// or restored without the other.
fn resources_path(kfc_path: &Path) -> PathBuf {
    kfc_path.with_extension("kfc_resources")
}

fn resources_bak_path(kfc_path: &Path) -> PathBuf {
    let resources = resources_path(kfc_path);
    let mut bak_path = resources.to_string();
    bak_path.push_str(".bak");
    Path::new(&bak_path).to_path_buf()
}

pub fn create_backup(
    kfc_path: impl AsRef<Path>
) -> Result<PathBuf, ()> {
    let path = kfc_path.as_ref();
    let mut bak_path = path.to_string();
    bak_path.push_str(".bak");
    let bak_path = Path::new(&bak_path);

    if bak_path.exists() && !validate_backup(path, bak_path) {
        warn!(
            path = ?path,
            backup_path = ?bak_path,
            "Backup file is not compatible with the current KFC file, creating a new backup.",
        );

        if !bak_path.is_file() {
            error!(
                path = ?bak_path,
                "Backup path is not a file, please remove it manually.",
            );

            return Err(());
        }

        if let Err(e) = std::fs::remove_file(bak_path) {
            warn!(
                path = ?bak_path,
                error = %e,
                "Failed to remove old backup file",
            );
        }
    }

    let mut kfc_bak_created = false;

    if !bak_path.exists() {
        if let Err(e) = std::fs::copy(path, bak_path) {
            error!(
                path = ?bak_path,
                error = %e,
                "Failed to create backup file",
            );

            return Err(());
        } else {
            info!(
                path = ?bak_path,
                "Backup file created successfully",
            );

            kfc_bak_created = true;
        }
    }

    // Back the resources stream up alongside the .kfc whenever the .kfc
    // backup is (re)created, or when the stream backup is missing (first
    // run after this pairing was introduced). A kept .kfc backup with an
    // existing stream backup is left untouched: at that point the live
    // stream is a later export and copying it would poison the pair.
    let resources = resources_path(path);
    let res_bak_path = resources_bak_path(path);

    if resources.is_file() && (kfc_bak_created || !res_bak_path.exists()) {
        if let Err(e) = std::fs::copy(&resources, &res_bak_path) {
            error!(
                path = ?res_bak_path,
                error = %e,
                "Failed to create resource backup file",
            );

            // Never leave a partial stream backup behind; a later
            // restore would happily install the truncated half.
            let _ = std::fs::remove_file(&res_bak_path);

            return Err(());
        } else {
            info!(
                path = ?res_bak_path,
                "Resource backup file created successfully",
            );
        }
    }

    Ok(bak_path.to_path_buf())
}

pub fn restore_backup(
    kfc_path: impl AsRef<Path>,
) -> Result<(), ()> {
    let path = kfc_path.as_ref();
    let mut bak_path = path.to_string();
    bak_path.push_str(".bak");
    let bak_path = Path::new(&bak_path);

    if !bak_path.exists() {
        warn!(
            path = ?bak_path,
            "Backup file does not exist, cannot restore.",
        );
        return Err(());
    }

    if !bak_path.is_file() {
        error!(
            path = ?bak_path,
            "Backup path is not a file, please remove it manually.",
        );
        return Err(());
    }

    if !validate_backup(path, bak_path) {
        warn!(
            path = ?path,
            backup_path = ?bak_path,
            "Backup file is not compatible with the current KFC file, cannot restore.",
        );
        return Err(());
    }

    if let Err(e) = std::fs::copy(bak_path, path) {
        error!(
            path = ?path,
            backup_path = ?bak_path,
            error = %e,
            "Failed to restore backup file",
        );
        return Err(());
    } else {
        info!(
            path = ?path,
            backup_path = ?bak_path,
            "Backup file restored successfully",
        );
    }

    // Restore the resources half of the pair as well, when one was backed
    // up. Restoring only the .kfc leaves a directory pointing into a
    // stream from another generation, which the game refuses to load.
    let res_bak_path = resources_bak_path(path);

    if res_bak_path.is_file() {
        let resources = resources_path(path);

        if let Err(e) = std::fs::copy(&res_bak_path, &resources) {
            error!(
                path = ?resources,
                backup_path = ?res_bak_path,
                error = %e,
                "Failed to restore resource backup file",
            );
            return Err(());
        } else {
            info!(
                path = ?resources,
                backup_path = ?res_bak_path,
                "Resource backup file restored successfully",
            );
        }
    }

    Ok(())
}

pub fn validate_backup(
    kfc_path: &Path,
    kfc_path_bak: &Path
) -> bool {
    let version_bak = match KFCFile::get_version_tag(kfc_path_bak) {
        Ok(file) => file,
        Err(_) => return false,
    };

    let version = match KFCFile::get_version_tag(kfc_path) {
        Ok(file) => file,
        Err(_) => return false,
    };

    version == version_bak
}

pub fn load_type_registry(
    game_dir: &Path,
    cache_dir: &Path,
    file_name: &str,
) -> Result<(TypeRegistry, bool), ()> {
    if let Err(e) = std::fs::create_dir_all(cache_dir) {
        warn!(
            error = %e,
            path = ?cache_dir,
            "Failed to create cache directory, unable to load or save type registry",
        );
        return Err(());
    }

    let types_path = cache_dir.join("types.json");
    let exe_path = game_dir.join(file_name).with_extension("exe");
    let kfc_path = game_dir.join(file_name).with_extension("kfc");

    let type_registry = match File::open(&types_path) {
        Ok(file) => {
            let reader = BufReader::new(file);

            match serde_json::from_reader::<_, TypeRegistry>(reader) {
                Ok(registry) => Some(registry),
                Err(e) => {
                    debug!(
                        error = %e,
                        path = ?types_path,
                        "Failed to read type registry from file, attempting to extract types...",
                    );

                    None
                },
            }
        }
        Err(e) => {
            debug!(
                error = %e,
                path = ?types_path,
                "Failed to read type registry from file, attempting to extract types...",
            );

            None
        },
    };

    let version_tag = match KFCFile::get_version_tag(&kfc_path) {
        Ok(tag) => Some(tag),
        Err(e) => {
            warn!(
                error = %e,
                path = ?types_path,
                kfc_path = ?kfc_path,
                "Failed to get version tag from KFC file, assuming types are outdated, attempting to extract types...",
            );

            None
        }
    };

    let type_registry = match type_registry {
        Some(type_registry) => {
            if let Some(version_tag) = &version_tag {
                if version_tag != &type_registry.version {
                    warn!(
                        path = ?types_path,
                        kfc_path = ?kfc_path,
                        "Type registry is outdated, attempting to extract types again..."
                    );

                    None
                } else {
                    Some(type_registry)
                }
            } else {
                None
            }
        },
        None => None
    };

    let (type_registry, is_dirty) = match type_registry {
        Some(type_registry) => (type_registry, false),
        None => match TypeRegistry::load_from_executable(&exe_path) {
            Ok(mut registry) => {
                if let Some(version_tag) = version_tag {
                    registry.version = version_tag;

                    match serde_json::to_string(&registry) {
                        Ok(json) => {
                            if let Err(e) = std::fs::write(&types_path, json) {
                                warn!(
                                    error = %e,
                                    path = ?types_path,
                                    "Failed to write type registry to file",
                                );
                            }
                        }
                        Err(e) => {
                            warn!(
                                error = %e,
                                path = ?types_path,
                                "Failed to serialize type registry to JSON",
                            );
                        }
                    }
                } else {
                    warn!(
                        path = ?types_path,
                        kfc_path = ?kfc_path,
                        "No version tag found, type registry cannot be saved",
                    );
                }

                (registry, true)
            }
            Err(e) => {
                error!(
                    error = %e,
                    path = ?types_path,
                    exe_path = ?exe_path,
                    "Failed to load type registry",
                );

                return Err(());
            }
        }
    };

    info!(
        path = ?types_path,
        exe_path = ?exe_path,
        kfc_path = ?kfc_path,
        version = type_registry.version,
        is_dirty = is_dirty,
        "Type registry loaded successfully",
    );

    Ok((type_registry, is_dirty))
}

pub fn export_lua_definitions(
    cache_dir: &Path,
    type_registry: &TypeRegistry,
    force: bool,
) {
    let lua_cache_dir = cache_dir.join("lua");

    if let Err(e) = std::fs::create_dir_all(&lua_cache_dir) {
        warn!(
            error = %e,
            path = ?lua_cache_dir,
            "Failed to create lua cache directory, unable to generate lua definitions",
        );
    } else {
        let lua_type_def_path = lua_cache_dir.join("types.lua");
        let lua_base_def_path = lua_cache_dir.join("base.lua");

        if force || !lua_type_def_path.exists() {
            let def = crate::definition::generator::generate(type_registry);

            match std::fs::write(&lua_type_def_path, def) {
                Ok(_) => info!(
                    path = ?lua_type_def_path,
                    "Lua type definition file has been generated",
                ),
                Err(e) => {
                    warn!(
                        error = %e,
                        path = ?lua_type_def_path,
                        "Failed to write lua type definition file",
                    );
                }
            }
        }

        if force || !lua_base_def_path.exists() {
            match std::fs::write(&lua_base_def_path, crate::definition::DEFINITION_FILE) {
                Ok(_) => info!(
                    path = ?lua_base_def_path,
                    "Lua base definition file has been generated",
                ),
                Err(e) => {
                    warn!(
                        error = %e,
                        path = ?lua_base_def_path,
                        "Failed to write lua base definition file",
                    );
                }
            }
        }
    }
}
