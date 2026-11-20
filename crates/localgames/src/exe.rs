//! Finding a game's main program inside its folder.
//!
//! Game folders hold many `.exe` files: the game, an uninstaller, crash
//! reporters, redistributable installers. [`pick_main`] guesses which one the
//! person means to launch.

use crate::text::normalize;
use std::cmp::Reverse;
use std::path::{Path, PathBuf};

/// Programs that are never the game itself.
const NOISE: &[&str] = &[
    "unins", "uninstall", "setup", "install", "crash", "redist", "vcredist", "vc_redist", "dxsetup", "dotnet", "directx", "oalinst",
    "helper", "notification", "updater", "webhelper", "cefprocess", "werfault", "unitycrash", "diagnostic", "eac_", "easyanticheat",
];

pub fn is_noise(file_name: &str) -> bool {
    let l = file_name.to_ascii_lowercase();
    NOISE.iter().any(|n| l.contains(n))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub size: u64,
}

/// The most likely main program: one whose name matches the folder's wins,
/// then the one closest to the top of the folder, then the biggest.
pub fn pick_main<'a>(folder_name: &str, candidates: &'a [Candidate]) -> Option<&'a Candidate> {
    let want = normalize(folder_name);
    candidates
        .iter()
        .filter(|c| c.path.file_name().and_then(|n| n.to_str()).is_some_and(|n| !is_noise(n)))
        .max_by_key(|c| {
            let stem = c.path.file_stem().and_then(|s| s.to_str()).map(normalize).unwrap_or_default();
            let name_match = stem.len() >= 3 && !want.is_empty() && (stem == want || want.contains(&stem) || stem.contains(&want));
            (name_match, Reverse(c.path.components().count()), c.size)
        })
}

/// Every `.exe` under `root`, up to `max_depth` folders down (0 = only
/// `root`). Symlinks are not followed, and the result is capped.
pub fn find_exes(root: &Path, max_depth: usize) -> Vec<Candidate> {
    const CAP: usize = 500;
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            if out.len() >= CAP {
                return out;
            }
            let Ok(kind) = entry.file_type() else { continue };
            let path = entry.path();
            if kind.is_dir() {
                if depth < max_depth {
                    stack.push((path, depth + 1));
                }
            } else if kind.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                out.push(Candidate { path, size });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(p: &str, size: u64) -> Candidate {
        Candidate { path: PathBuf::from(p), size }
    }

    #[test]
    fn noise_is_recognised() {
        assert!(is_noise("unins000.exe"));
        assert!(is_noise("UnityCrashHandler64.exe"));
        assert!(is_noise("vc_redist.x64.exe"));
        assert!(!is_noise("Hades.exe"));
    }

    #[test]
    fn a_program_named_like_the_folder_wins_over_a_bigger_one() {
        let list = [c("G/Hades/Support.exe", 9_000), c("G/Hades/Hades.exe", 100)];
        assert_eq!(pick_main("Hades", &list).unwrap().path, PathBuf::from("G/Hades/Hades.exe"));
    }

    #[test]
    fn noise_never_wins_even_if_it_is_the_only_big_file() {
        let list = [c("G/Game/unins000.exe", 90_000), c("G/Game/run.exe", 10)];
        assert_eq!(pick_main("Something", &list).unwrap().path, PathBuf::from("G/Game/run.exe"));
    }

    #[test]
    fn without_a_name_match_the_shallowest_then_biggest_is_chosen() {
        let list = [c("G/Game/bin/deep.exe", 5_000), c("G/Game/top.exe", 10), c("G/Game/top2.exe", 20)];
        assert_eq!(pick_main("Unrelated", &list).unwrap().path, PathBuf::from("G/Game/top2.exe"));
    }

    #[test]
    fn only_noise_means_no_guess() {
        assert!(pick_main("Game", &[c("G/Game/unins000.exe", 1)]).is_none());
        assert!(pick_main("Game", &[]).is_none());
    }

    #[test]
    fn find_exes_respects_depth_and_extension() {
        let root = std::env::temp_dir().join(format!("sir-exe-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        std::fs::write(root.join("top.EXE"), b"x").unwrap();
        std::fs::write(root.join("a/readme.txt"), b"x").unwrap();
        std::fs::write(root.join("a/b/mid.exe"), b"x").unwrap();
        std::fs::write(root.join("a/b/c/deep.exe"), b"x").unwrap();
        let names = |depth| {
            let mut v: Vec<String> = find_exes(&root, depth).iter().map(|c| c.path.file_name().unwrap().to_string_lossy().to_string()).collect();
            v.sort();
            v
        };
        assert_eq!(names(0), ["top.EXE"]);
        assert_eq!(names(2), ["mid.exe", "top.EXE"]);
        assert_eq!(names(3), ["deep.exe", "mid.exe", "top.EXE"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
