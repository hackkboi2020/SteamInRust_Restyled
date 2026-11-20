//! Finding games already on this PC: a games folder, the itch.io app's
//! library, and the Minecraft launcher.
//!
//! These only read folders and files; nothing is changed.

use crate::exe::{find_exes, pick_main};
use crate::library::Source;
use crate::text::display_name;
use std::io::Read;
use std::path::{Path, PathBuf};

/// A game found on disk, not yet in the library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    pub exe: PathBuf,
    pub source: Source,
    /// A cover picture the source itself knows (itch.io has one per game).
    pub cover_url: Option<String>,
}

/// Each folder directly inside `root` is treated as one game.
pub fn scan_folder(root: &Path) -> Vec<Found> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else { return out };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let folder = entry.file_name().to_string_lossy().to_string();
        let exes = find_exes(&entry.path(), 3);
        if let Some(main) = pick_main(&folder, &exes) {
            out.push(Found { name: display_name(&folder), exe: main.path.clone(), source: Source::Folder, cover_url: None });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// Where the itch.io app keeps installed games, if it is installed.
pub fn itch_apps_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("APPDATA")?).join("itch").join("apps");
    dir.is_dir().then_some(dir)
}

/// What the itch.io app wrote about a game when it installed it
/// (`.itch/receipt.json.gz`): its title and cover picture.
fn itch_receipt(game_dir: &Path) -> Option<(String, Option<String>)> {
    let file = std::fs::File::open(game_dir.join(".itch").join("receipt.json.gz")).ok()?;
    let mut json = String::new();
    // A receipt is a few kilobytes; cap what we read from a file we don't control.
    flate2::read::GzDecoder::new(file).take(1 << 20).read_to_string(&mut json).ok()?;
    let v: serde_json::Value = serde_json::from_str(&json).ok()?;
    let game = v.get("game")?;
    let title = game.get("title")?.as_str()?.trim().to_string();
    let cover = game.get("coverUrl").and_then(|c| c.as_str()).filter(|c| c.starts_with("https://")).map(String::from);
    (!title.is_empty()).then_some((title, cover))
}

/// Games installed with the itch.io app.
pub fn scan_itch() -> Vec<Found> {
    let Some(apps) = itch_apps_dir() else { return Vec::new() };
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&apps) else { return out };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let dir = entry.path();
        let folder = entry.file_name().to_string_lossy().to_string();
        let (name, cover_url) = itch_receipt(&dir).unwrap_or_else(|| (display_name(&folder), None));
        let exes = find_exes(&dir, 3);
        if let Some(main) = pick_main(&name, &exes).or_else(|| pick_main(&folder, &exes)) {
            out.push(Found { name, exe: main.path.clone(), source: Source::Itch, cover_url });
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

/// The Minecraft launcher, when installed in one of its usual places.
pub fn scan_minecraft() -> Vec<Found> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(base) = std::env::var_os(var) {
            candidates.push(PathBuf::from(base).join("Minecraft Launcher").join("MinecraftLauncher.exe"));
        }
    }
    if let Some(base) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(base).join("Programs").join("Minecraft Launcher").join("MinecraftLauncher.exe"));
    }
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .map(|exe| vec![Found { name: "Minecraft".into(), exe, source: Source::Minecraft, cover_url: None }])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("sir-scan-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn a_games_folder_yields_one_game_per_subfolder() {
        let root = temp("folder");
        std::fs::create_dir_all(root.join("Dead_Cells-v1.2")).unwrap();
        std::fs::write(root.join("Dead_Cells-v1.2").join("deadcells.exe"), b"x").unwrap();
        std::fs::write(root.join("Dead_Cells-v1.2").join("unins000.exe"), vec![0u8; 100]).unwrap();
        std::fs::create_dir_all(root.join("Empty")).unwrap();
        std::fs::write(root.join("loose.exe"), b"x").unwrap();
        let found = scan_folder(&root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Dead Cells");
        assert!(found[0].exe.ends_with("deadcells.exe"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_itch_receipt_gives_title_and_cover() {
        let dir = temp("itch");
        std::fs::create_dir_all(dir.join(".itch")).unwrap();
        let json = br#"{"game":{"title":"Night in the Woods","coverUrl":"https://img.itch.zone/c.png"}}"#;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(json).unwrap();
        std::fs::write(dir.join(".itch").join("receipt.json.gz"), gz.finish().unwrap()).unwrap();
        let (title, cover) = itch_receipt(&dir).unwrap();
        assert_eq!(title, "Night in the Woods");
        assert_eq!(cover.as_deref(), Some("https://img.itch.zone/c.png"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_or_broken_receipt_is_just_none() {
        let dir = temp("itch-bad");
        assert!(itch_receipt(&dir).is_none());
        std::fs::create_dir_all(dir.join(".itch")).unwrap();
        std::fs::write(dir.join(".itch").join("receipt.json.gz"), b"not gzip").unwrap();
        assert!(itch_receipt(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
