//! The saved list of local games.

use crate::id;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Source {
    /// Added by hand.
    #[default]
    Manual,
    /// Found by scanning a games folder.
    Folder,
    Itch,
    Minecraft,
}

/// Pictures for a game, as web addresses (the app's image cache keeps them on
/// disk, so they still show offline once seen).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Art {
    /// Wide banner behind the game's page.
    pub hero: Option<String>,
    /// Tall box art for the library grid.
    pub cover: Option<String>,
    /// Small square icon.
    pub icon: Option<String>,
    /// Transparent title logo.
    pub logo: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    pub description: String,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    pub release_date: String,
    pub genres: Vec<String>,
    /// Features like "Single-player", "Co-op", "Steam Achievements".
    pub features: Vec<String>,
    pub website: String,
    /// The Steam game this was matched to, if any.
    pub steam_app_id: Option<u32>,
}

impl Meta {
    pub fn is_empty(&self) -> bool {
        self.description.is_empty() && self.developers.is_empty() && self.genres.is_empty() && self.steam_app_id.is_none()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalGame {
    pub name: String,
    pub exe: String,
    pub args: String,
    pub source: Source,
    pub art: Art,
    pub meta: Meta,
    pub notes: String,
    /// A line shown to Steam friends while playing (rich presence).
    pub status: String,
    /// Seconds played with this launcher.
    pub playtime_secs: u64,
    /// Unix time of the last session's start.
    pub last_played: Option<u64>,
    pub added: u64,
    /// Launch options to join a LAN host; `{ip}` and `{port}` are replaced.
    pub join_args: String,
    /// The game's own port when hosting a LAN game (0 = not set).
    pub host_port: u16,
    /// Hand-chosen art and info should survive a refresh.
    pub art_locked: bool,
}

impl LocalGame {
    pub fn new(name: impl Into<String>, exe: impl Into<String>, source: Source, now: u64) -> Self {
        Self { name: name.into(), exe: exe.into(), source, added: now, ..Default::default() }
    }

    /// The id Steam uses for this game (see [`id::game_id`]).
    pub fn game_id(&self) -> u64 {
        id::game_id(&self.exe, &self.name)
    }

    pub fn same_program(&self, exe: &str) -> bool {
        self.exe.eq_ignore_ascii_case(exe)
    }
}

/// Everything saved: the games, plus the few settings that go with them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub games: Vec<LocalGame>,
    /// SteamGridDB key (optional), for more art than the official store has.
    pub sgdb_key: String,
    /// Folder scanned for games.
    pub games_folder: String,
}

impl Store {
    /// A damaged or missing save is an empty library, never a crash.
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }

    pub fn to_json(&self) -> Option<String> {
        serde_json::to_string(self).ok()
    }

    /// Add a game unless that program is already in the library. Returns its index.
    pub fn add(&mut self, g: LocalGame) -> Result<usize, usize> {
        if let Some(i) = self.games.iter().position(|x| x.same_program(&g.exe)) {
            return Err(i);
        }
        self.games.push(g);
        Ok(self.games.len() - 1)
    }

    /// Time played: add a finished session.
    pub fn record_session(&mut self, index: usize, started: u64, secs: u64) {
        if let Some(g) = self.games.get_mut(index) {
            g.playtime_secs = g.playtime_secs.saturating_add(secs);
            g.last_played = Some(started);
        }
    }

    /// Most recently played first.
    pub fn recent(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.games.len()).filter(|&i| self.games[i].last_played.is_some()).collect();
        v.sort_by_key(|&i| std::cmp::Reverse(self.games[i].last_played));
        v
    }
}

/// "2h 15m", "40m", "less than a minute".
pub fn format_playtime(secs: u64) -> String {
    match secs {
        0..=59 => "less than a minute".into(),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_broken_save_is_an_empty_library() {
        assert!(Store::from_json("{not json").games.is_empty());
        assert!(Store::from_json("").games.is_empty());
    }

    #[test]
    fn saves_round_trip_and_old_saves_without_new_fields_load() {
        let mut s = Store::default();
        s.add(LocalGame::new("Hades", r"C:\G\Hades.exe", Source::Manual, 5)).unwrap();
        let back = Store::from_json(&s.to_json().unwrap());
        assert_eq!(back.games, s.games);
        let old = Store::from_json(r#"{"games":[{"name":"Old","exe":"o.exe"}]}"#);
        assert_eq!(old.games[0].name, "Old");
        assert_eq!(old.games[0].playtime_secs, 0);
    }

    #[test]
    fn the_same_program_is_not_added_twice_whatever_its_case() {
        let mut s = Store::default();
        assert_eq!(s.add(LocalGame::new("A", r"C:\G\a.exe", Source::Manual, 0)), Ok(0));
        assert_eq!(s.add(LocalGame::new("A again", r"c:\g\A.EXE", Source::Folder, 0)), Err(0));
        assert_eq!(s.games.len(), 1);
    }

    #[test]
    fn sessions_add_up_and_recent_is_ordered() {
        let mut s = Store::default();
        s.add(LocalGame::new("A", "a.exe", Source::Manual, 0)).unwrap();
        s.add(LocalGame::new("B", "b.exe", Source::Manual, 0)).unwrap();
        s.record_session(0, 100, 30);
        s.record_session(0, 100, 40);
        s.record_session(1, 200, 5);
        assert_eq!(s.games[0].playtime_secs, 70);
        assert_eq!(s.recent(), vec![1, 0]);
    }

    #[test]
    fn playtime_reads_naturally() {
        assert_eq!(format_playtime(5), "less than a minute");
        assert_eq!(format_playtime(2400), "40m");
        assert_eq!(format_playtime(8100), "2h 15m");
    }
}
