//! The blueprints shipped in `/blueprints` are the panel's front door: an
//! operator deploys one before writing anything of their own. A blueprint that
//! no longer round-trips through the current schema is a broken product, not a
//! stale file, so every shipped YAML is parsed and validated here.

use std::path::{Path, PathBuf};

fn blueprint_files() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../blueprints");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", dir.display(), e))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("yaml"))
        .collect();
    files.sort();
    files
}

#[test]
fn every_shipped_blueprint_parses_and_validates() {
    let files = blueprint_files();
    assert!(!files.is_empty(), "no blueprints found to check");

    for path in &files {
        let text = std::fs::read_to_string(path).unwrap();
        let blueprint: nexus_config::Blueprint = serde_yaml::from_str(&text)
            .unwrap_or_else(|e| panic!("{} failed to parse: {}", path.display(), e));
        blueprint
            .validate()
            .unwrap_or_else(|e| panic!("{} failed validation: {}", path.display(), e));
    }
}

#[test]
fn dayz_blueprint_declares_steam_workshop_mods() {
    // DayZ has no mod source other than the Workshop, so the blueprint that
    // motivated the Steam Workshop adapter must keep pointing at it.
    let path = blueprint_files()
        .into_iter()
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some("dayz.yaml"))
        .expect("dayz.yaml is missing");

    let text = std::fs::read_to_string(&path).unwrap();
    let blueprint: nexus_config::Blueprint = serde_yaml::from_str(&text).unwrap();

    let mods = blueprint.mods.expect("dayz.yaml declares no mod support");
    assert!(
        mods.marketplaces.iter().any(|m| m == "steam_workshop"),
        "dayz.yaml should list the steam_workshop marketplace, got {:?}",
        mods.marketplaces
    );

    // The `-mod=` argument is how DayZ loads `@ModName` folders; without it an
    // installed Workshop mod would never be read.
    assert!(
        blueprint.startup.args.iter().any(|a| a.contains("-mod=")),
        "dayz.yaml startup args should pass -mod="
    );
}
