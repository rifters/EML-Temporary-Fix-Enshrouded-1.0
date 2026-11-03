use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_NAME: &str = "Default";
const FOLDER: &str = "profiles";
const MARKER: &str = "active.txt";
const FILES: [&str; 7] = [
    "m182_zones.json",
    "m182_spawners.json",
    "m182_teams.json",
    "m182_places.json",
    "m182_raid_templates.json",
    "m182_sent.json",
    "m182_events.json",
];

pub fn clean_name(raw: &str) -> Option<String> {
    let name: String = raw.trim().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == ' ').take(32).collect();
    (!name.is_empty()).then_some(name)
}

fn profile_dir(dir: &Path, name: &str) -> PathBuf {
    dir.join(FOLDER).join(name)
}

fn store(dir: &Path, name: &str) -> std::io::Result<()> {
    let target = profile_dir(dir, name);
    fs::create_dir_all(&target)?;
    for file in FILES {
        let from = dir.join(file);
        if from.is_file() {
            fs::copy(&from, target.join(file))?;
        } else {
            let _ = fs::remove_file(target.join(file));
        }
    }
    Ok(())
}

fn load(dir: &Path, name: &str) -> std::io::Result<()> {
    let source = profile_dir(dir, name);
    for file in FILES {
        let from = source.join(file);
        let to = dir.join(file);
        if from.is_file() {
            fs::copy(&from, &to)?;
        } else if to.is_file() {
            fs::remove_file(&to)?;
        }
    }
    Ok(())
}

pub fn active(dir: &Path) -> String {
    fs::read_to_string(dir.join(FOLDER).join(MARKER)).ok().and_then(|t| clean_name(&t)).unwrap_or_else(|| DEFAULT_NAME.to_string())
}

pub fn list(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir.join(FOLDER))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| clean_name(&e.file_name().to_string_lossy()))
        .collect();
    let current = active(dir);
    if !names.contains(&current) {
        names.push(current);
    }
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

pub fn switch(dir: &Path, to: &str) -> Result<String, String> {
    let name = clean_name(to).ok_or_else(|| "invalid name".to_string())?;
    let current = active(dir);
    if current == name {
        return Ok(name);
    }
    store(dir, &current).map_err(|e| e.to_string())?;
    load(dir, &name).map_err(|e| e.to_string())?;
    fs::create_dir_all(profile_dir(dir, &name)).map_err(|e| e.to_string())?;
    fs::write(dir.join(FOLDER).join(MARKER), &name).map_err(|e| e.to_string())?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("m182_profile_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  Midnight Rain "), Some("Midnight Rain".into()));
        assert_eq!(clean_name("../x/y"), Some("xy".into()));
        assert_eq!(clean_name("   "), None);
    }

    #[test]
    fn switching_keeps_each_set_and_remembers_the_last() {
        let dir = sandbox("switch");
        fs::write(dir.join("m182_zones.json"), "aincrad-zones").unwrap();
        fs::write(dir.join("m182_spawners.json"), "aincrad-bosses").unwrap();
        fs::create_dir_all(dir.join(FOLDER)).unwrap();
        fs::write(dir.join(FOLDER).join(MARKER), "Aincrad").unwrap();
        assert_eq!(active(&dir), "Aincrad");

        assert_eq!(switch(&dir, "Midnight Rain").unwrap(), "Midnight Rain");
        assert_eq!(active(&dir), "Midnight Rain");
        assert!(!dir.join("m182_zones.json").exists());
        assert!(!dir.join("m182_spawners.json").exists());
        fs::write(dir.join("m182_zones.json"), "midnight-zones").unwrap();
        assert_eq!(list(&dir), vec!["Aincrad".to_string(), "Midnight Rain".to_string()]);

        assert_eq!(switch(&dir, "Aincrad").unwrap(), "Aincrad");
        assert_eq!(fs::read_to_string(dir.join("m182_zones.json")).unwrap(), "aincrad-zones");
        assert_eq!(fs::read_to_string(dir.join("m182_spawners.json")).unwrap(), "aincrad-bosses");

        assert_eq!(switch(&dir, "Midnight Rain").unwrap(), "Midnight Rain");
        assert_eq!(fs::read_to_string(dir.join("m182_zones.json")).unwrap(), "midnight-zones");
        assert!(!dir.join("m182_spawners.json").exists());
        assert_eq!(switch(&dir, "Midnight Rain").unwrap(), "Midnight Rain");
        assert_eq!(fs::read_to_string(dir.join("m182_zones.json")).unwrap(), "midnight-zones");
    }

    #[test]
    fn untouched_install_is_the_default_profile() {
        let dir = sandbox("fresh");
        fs::write(dir.join("m182_zones.json"), "mine").unwrap();
        assert_eq!(active(&dir), DEFAULT_NAME);
        assert_eq!(list(&dir), vec![DEFAULT_NAME.to_string()]);
        switch(&dir, "Other").unwrap();
        assert!(!dir.join("m182_zones.json").exists());
        switch(&dir, DEFAULT_NAME).unwrap();
        assert_eq!(fs::read_to_string(dir.join("m182_zones.json")).unwrap(), "mine");
    }
}
