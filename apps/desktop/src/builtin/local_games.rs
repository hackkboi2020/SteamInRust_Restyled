//! Local games: programs that don't come from Steam.
//!
//! Add a program, scan a games folder, or import from itch.io or Minecraft.
//! Each game gets a hero banner, cover, icon, logo and a description fetched
//! from the internet (and can be changed by hand if the automatic match is
//! wrong), keeps its own play time, shows up to your Steam friends as
//! "Playing <name>", and can host or join games on your local network.
//!
//! The game's page follows Steam's layout: a hero banner, a play bar, and a
//! row of feature tabs underneath.

use client::Command;
use egui::{Align, Align2, Color32, CornerRadius, Layout, Rect, Sense, Ui, pos2, vec2};
use extensions::{Ctx, Extension, Manifest, PageSpec};
use localgames::fetch::{ArtKind, ArtOption, Candidate, Fetcher};
use localgames::library::{Art, LocalGame, Meta, Source, Store, format_playtime};
use localgames::scan::{self, Found};
use localgames::{lan, text};
use std::collections::HashMap;
use std::process::Command as Proc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use ui::widgets::EditMenu;
use ui::{Theme, icons, widgets as w};

/// Work done on other threads, reported back to the page.
enum Msg {
    /// Games found by a scan.
    Found { label: &'static str, list: Vec<Found> },
    /// Details and art for a game. `lock`: the person chose it by hand.
    Filled { exe: String, meta: Option<Meta>, art: Art, lock: bool },
    /// The automatic match wasn't clear; these are the possible games.
    NeedsPick { exe: String, list: Vec<Candidate> },
    /// Results of a search the person started.
    Candidates { exe: String, list: Vec<Candidate> },
    /// Pictures to choose from.
    Options { exe: String, kind: ArtKind, list: Vec<ArtOption> },
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    About,
    Notes,
    Lan,
    Properties,
}

/// The art finder on a game's Properties tab.
#[derive(Default)]
struct Finder {
    query: String,
    candidates: Vec<Candidate>,
    chosen: Option<Candidate>,
    /// Pictures found, by position in [`ArtKind::ALL`].
    options: [Vec<ArtOption>; 4],
    showing: Option<ArtKind>,
}

struct Running {
    exe: String,
    child: std::process::Child,
    started: Instant,
    started_unix: u64,
}

/// LAN discovery state. The socket is only opened once the LAN tab is used,
/// so people who never use it never see a firewall prompt.
struct Lan {
    beacon: Option<lan::Beacon>,
    error: Option<String>,
    peers: lan::Peers,
    instance: u64,
    hosting: Option<lan::Announce>,
    last_announce: Option<Instant>,
    nick: String,
    last_used: Instant,
}

impl Lan {
    fn new() -> Self {
        Self { beacon: None, error: None, peers: lan::Peers::default(), instance: rand::random(), hosting: None, last_announce: None, nick: String::new(), last_used: Instant::now() }
    }

    fn ensure(&mut self) {
        self.last_used = Instant::now();
        if self.beacon.is_none() && self.error.is_none() {
            match lan::Beacon::bind() {
                Ok(b) => self.beacon = Some(b),
                Err(e) => self.error = Some(format!("Couldn't listen on the network (port {}): {e}", lan::PORT)),
            }
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        // Not hosting and the tab hasn't been open for a while: let go of the socket.
        if self.hosting.is_none() && self.beacon.is_some() && now.duration_since(self.last_used) > Duration::from_secs(15) {
            self.beacon = None;
            self.peers = lan::Peers::default();
            return;
        }
        if let Some(b) = &self.beacon {
            b.poll(self.instance, &mut self.peers, now);
            self.peers.prune(now);
            if let Some(a) = &self.hosting {
                if self.last_announce.is_none_or(|t| now.duration_since(t) >= lan::INTERVAL) {
                    let _ = b.announce(a);
                    self.last_announce = Some(now);
                }
            }
        }
    }
}

pub struct LocalGames {
    rt: tokio::runtime::Handle,
    egui: egui::Context,
    store: Store,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// The open game, by its program path.
    selected: Option<String>,
    tab: Tab,
    filter: String,
    running: Option<Running>,
    finder: Finder,
    /// Automatic matches that need the person to choose, by program path.
    pending: HashMap<String, Vec<Candidate>>,
    busy: bool,
    last_failure: Option<Instant>,
    lan: Lan,
}

impl LocalGames {
    pub fn new(rt: tokio::runtime::Handle, egui: egui::Context) -> Self {
        let (tx, rx) = channel();
        Self {
            rt,
            egui,
            store: Store::default(),
            tx,
            rx,
            selected: None,
            tab: Tab::About,
            filter: String::new(),
            running: None,
            finder: Finder::default(),
            pending: HashMap::new(),
            busy: false,
            last_failure: None,
            lan: Lan::new(),
        }
    }

    fn index_of(&self, exe: &str) -> Option<usize> {
        self.store.games.iter().position(|g| g.exe == exe)
    }

    fn select(&mut self, exe: String) {
        let name = self.index_of(&exe).map(|i| self.store.games[i].name.clone()).unwrap_or_default();
        self.finder = Finder { query: name, candidates: self.pending.remove(&exe).unwrap_or_default(), ..Default::default() };
        self.tab = Tab::About;
        self.selected = Some(exe);
    }

    // ---- background work ----

    fn send_later<F>(&self, work: F)
    where
        F: std::future::Future<Output = Msg> + Send + 'static,
    {
        let (tx, egui) = (self.tx.clone(), self.egui.clone());
        self.rt.spawn(async move {
            let _ = tx.send(work.await);
            egui.request_repaint();
        });
    }

    /// Find a game on the internet and fill in its art and details. A clear
    /// match is applied; an unclear one is left for the person to choose.
    fn lookup(&self, exe: String, name: String) {
        let key = self.store.sgdb_key.clone();
        self.send_later(async move {
            let fetcher = match Fetcher::new(&key) {
                Ok(f) => f,
                Err(e) => return Msg::Failed(e.to_string()),
            };
            match fetcher.search(&name).await {
                Err(e) => Msg::Failed(format!("Couldn't look up {name}: {e}")),
                Ok(list) => {
                    let titles: Vec<&str> = list.iter().map(|c| c.title.as_str()).collect();
                    match text::best_match(&name, &titles) {
                        Some(i) => {
                            let (meta, art) = fetcher.fill(&list[i]).await;
                            Msg::Filled { exe, meta, art, lock: false }
                        }
                        None => Msg::NeedsPick { exe, list },
                    }
                }
            }
        });
    }

    fn search(&self, exe: String, query: String) {
        let key = self.store.sgdb_key.clone();
        self.send_later(async move {
            match Fetcher::new(&key) {
                Err(e) => Msg::Failed(e.to_string()),
                Ok(f) => match f.search(&query).await {
                    Ok(list) => Msg::Candidates { exe, list },
                    Err(e) => Msg::Failed(format!("Search failed: {e}")),
                },
            }
        });
    }

    fn use_candidate(&self, exe: String, c: Candidate) {
        let key = self.store.sgdb_key.clone();
        self.send_later(async move {
            match Fetcher::new(&key) {
                Err(e) => Msg::Failed(e.to_string()),
                Ok(f) => {
                    let (meta, art) = f.fill(&c).await;
                    Msg::Filled { exe, meta, art, lock: true }
                }
            }
        });
    }

    fn load_options(&self, exe: String, c: Candidate, kind: ArtKind) {
        let key = self.store.sgdb_key.clone();
        self.send_later(async move {
            match Fetcher::new(&key) {
                Err(e) => Msg::Failed(e.to_string()),
                Ok(f) => {
                    let mut c = c;
                    if c.sgdb_id.is_none() {
                        c.sgdb_id = f.sgdb_for(&c.title).await;
                    }
                    Msg::Options { exe, kind, list: f.art_options(&c, kind).await }
                }
            }
        });
    }

    fn scan(&mut self, label: &'static str, job: impl FnOnce() -> Vec<Found> + Send + 'static) {
        self.busy = true;
        let (tx, egui) = (self.tx.clone(), self.egui.clone());
        self.rt.spawn_blocking(move || {
            let _ = tx.send(Msg::Found { label, list: job() });
            egui.request_repaint();
        });
    }

    /// Add what a scan found, skipping what's already here, and look each new
    /// game up. Returns how many were added.
    fn add_found(&mut self, list: Vec<Found>) -> usize {
        let now = unix_now();
        let mut added = 0;
        for f in list {
            let exe = f.exe.display().to_string();
            let mut g = LocalGame::new(f.name.clone(), exe.clone(), f.source, now);
            g.art.cover = f.cover_url;
            if self.store.add(g).is_ok() {
                added += 1;
                self.lookup(exe, f.name);
            }
        }
        added
    }

    fn handle(&mut self, msg: Msg, ctx: &mut Ctx) {
        match msg {
            Msg::Found { label, list } => {
                self.busy = false;
                let found = list.len();
                let added = self.add_found(list);
                ctx.toast(match (found, added) {
                    (0, _) => format!("No {label} games found"),
                    (_, 0) => format!("Everything from {label} is already in your library"),
                    (_, n) => format!("Added {n} game{} from {label}", if n == 1 { "" } else { "s" }),
                });
            }
            Msg::Filled { exe, meta, art, lock } => {
                if let Some(i) = self.index_of(&exe) {
                    let g = &mut self.store.games[i];
                    // A hand-picked look is only replaced by another hand-pick.
                    if lock || !g.art_locked {
                        for (slot, new) in [(&mut g.art.hero, art.hero), (&mut g.art.cover, art.cover), (&mut g.art.icon, art.icon), (&mut g.art.logo, art.logo)] {
                            if new.is_some() {
                                *slot = new;
                            }
                        }
                        if let Some(m) = meta.filter(|m| !m.is_empty()) {
                            g.meta = m;
                        }
                        g.art_locked |= lock;
                    }
                }
            }
            Msg::NeedsPick { exe, list } => {
                if self.selected.as_deref() == Some(exe.as_str()) {
                    self.finder.candidates = list;
                } else {
                    self.pending.insert(exe, list);
                }
            }
            Msg::Candidates { exe, list } => {
                if self.selected.as_deref() == Some(exe.as_str()) {
                    if list.is_empty() {
                        ctx.toast("Nothing found. Try a shorter name.");
                    }
                    self.finder.candidates = list;
                }
            }
            Msg::Options { exe, kind, list } => {
                if self.selected.as_deref() == Some(exe.as_str()) {
                    if list.is_empty() {
                        ctx.toast(format!("No {} pictures found", kind.label().to_lowercase()));
                    }
                    self.finder.options[kind_index(kind)] = list;
                    self.finder.showing = Some(kind);
                }
            }
            Msg::Failed(e) => {
                // Offline, every lookup fails: say so once, not once per game.
                if self.last_failure.is_none_or(|t| t.elapsed() > Duration::from_secs(20)) {
                    self.last_failure = Some(Instant::now());
                    ctx.toast(format!("{e} (art and details can be fetched later)"));
                }
            }
        }
    }

    // ---- playing ----

    fn launch(&mut self, i: usize, extra_args: Option<String>, ctx: &mut Ctx) {
        if let Some(r) = &self.running {
            let name = self.index_of(&r.exe).map(|i| self.store.games[i].name.clone()).unwrap_or_default();
            ctx.toast(format!("{name} is already running"));
            return;
        }
        let g = self.store.games[i].clone();
        let exe = std::path::PathBuf::from(&g.exe);
        if !exe.is_file() {
            ctx.toast(format!("{} was not found at {}", g.name, g.exe));
            return;
        }
        let mut cmd = Proc::new(&exe);
        cmd.args(text::split_args(&g.args));
        if let Some(extra) = extra_args {
            cmd.args(text::split_args(&extra));
        }
        if let Some(dir) = exe.parent() {
            cmd.current_dir(dir);
        }
        match cmd.spawn() {
            Ok(child) => {
                self.running = Some(Running { exe: g.exe.clone(), child, started: Instant::now(), started_unix: unix_now() });
                // Friends see "Playing <name>".
                ctx.send(Command::SetLocalPlaying { game_id: g.game_id(), name: g.name.clone(), status: Some(g.status.clone()).filter(|s| !s.trim().is_empty()) });
            }
            Err(e) => ctx.toast(format!("Couldn't start {}: {e}", g.name)),
        }
    }

    fn finish(&mut self, ctx: &mut Ctx) {
        let Some(r) = self.running.take() else { return };
        let secs = r.started.elapsed().as_secs();
        if let Some(i) = self.index_of(&r.exe) {
            self.store.record_session(i, r.started_unix, secs);
            ctx.toast(format!("Played {} for {}", self.store.games[i].name, format_playtime(secs)));
        }
        ctx.send(Command::ClearLocalPlaying);
    }

    // ---- pages ----

    fn library(&mut self, ui: &mut Ui, t: &Theme, ctx: &mut Ctx) {
        ui.label(w::text_bold("Local games", 28.0, t.text));
        ui.label(w::text("Games that aren't on Steam: add them once, play them here, and your friends still see what you're playing.", 14.0, t.text_dim));
        ui.add_space(14.0);
        ui.horizontal_wrapped(|ui| {
            if w::button(ui, t, Some(icons::PLUS), "Add program", w::Kind::Primary, w::Size::Md).clicked() {
                if let Some(path) = rfd::FileDialog::new().add_filter("Programs", &["exe", "bat", "cmd"]).pick_file() {
                    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    let folder = path.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    // "Game.exe" in a folder called "My Game" is called "My Game".
                    let name = if text::normalize(&folder).len() >= 3 && !text::normalize(&stem).is_empty() && text::normalize(&folder).contains(&text::normalize(&stem)) { text::display_name(&folder) } else { text::display_name(&stem) };
                    let found = Found { name, exe: path, source: Source::Manual, cover_url: None };
                    let n = self.add_found(vec![found]);
                    if n == 0 {
                        ctx.toast("That program is already in your library");
                    }
                }
            }
            if w::button(ui, t, Some(icons::FOLDER), "Scan folder…", w::Kind::Secondary, w::Size::Md).clicked() {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    self.store.games_folder = dir.display().to_string();
                    self.scan("folder", move || scan::scan_folder(&dir));
                }
            }
            if !self.store.games_folder.is_empty() && w::button(ui, t, Some(icons::DOWNLOAD), "Rescan", w::Kind::Ghost, w::Size::Md).clicked() {
                let dir = std::path::PathBuf::from(self.store.games_folder.clone());
                self.scan("folder", move || scan::scan_folder(&dir));
            }
            if w::button(ui, t, Some(icons::DOWNLOAD), "Import from itch.io", w::Kind::Secondary, w::Size::Md).clicked() {
                self.scan("itch.io", scan::scan_itch);
            }
            if w::button(ui, t, Some(icons::DOWNLOAD), "Import Minecraft", w::Kind::Secondary, w::Size::Md).clicked() {
                self.scan("Minecraft", scan::scan_minecraft);
            }
            if self.busy {
                ui.add(egui::Spinner::new());
            }
        });
        ui.add_space(10.0);
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter your games").desired_width(280.0).margin(vec2(10.0, 8.0))).with_edit_menu();
        ui.add_space(18.0);

        if self.store.games.is_empty() {
            w::card(t).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(w::text_semibold("Your library is empty", 16.0, t.text));
                ui.label(w::text("Add a program, scan the folder you keep your games in, or import your itch.io and Minecraft games. Covers and descriptions are fetched automatically.", 13.5, t.text_dim));
            });
            return;
        }

        let want = text::normalize(&self.filter);
        let mut order: Vec<usize> = (0..self.store.games.len()).filter(|&i| want.is_empty() || text::normalize(&self.store.games[i].name).contains(&want)).collect();
        order.sort_by_key(|&i| self.store.games[i].name.to_lowercase());
        let mut open = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(16.0, 16.0);
            for i in order {
                let g = &self.store.games[i];
                let playing = self.running.as_ref().is_some_and(|r| r.exe == g.exe);
                if tile(ui, t, g, playing) {
                    open = Some(g.exe.clone());
                }
            }
        });
        if let Some(exe) = open {
            self.select(exe);
        }
    }

    fn detail(&mut self, ui: &mut Ui, t: &Theme, ctx: &mut Ctx, i: usize) {
        let g = self.store.games[i].clone();
        let running_this = self.running.as_ref().is_some_and(|r| r.exe == g.exe);
        let mut back = false;
        let mut play = false;
        let mut stop = false;

        if w::button(ui, t, None, "‹ Local games", w::Kind::Ghost, w::Size::Sm).clicked() {
            back = true;
        }
        ui.add_space(10.0);

        // Hero banner with the logo (or name) on a dark strip.
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 280.0), Sense::hover());
        let r = t.corner();
        let urls: Vec<&str> = [g.art.hero.as_deref(), g.art.cover.as_deref()].into_iter().flatten().collect();
        if !w::paint_cover_first(ui, rect, &urls, r) {
            ui.painter().rect_filled(rect, r, t.surface);
        }
        let strip = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 92.0), rect.max);
        ui.painter().rect_filled(strip, CornerRadius { nw: 0, ne: 0, sw: r.sw, se: r.se }, Color32::from_black_alpha(150));
        let logo = Rect::from_min_size(pos2(strip.left() + 24.0, strip.top() + 14.0), vec2(280.0, 64.0));
        if !g.art.logo.as_deref().is_some_and(|u| w::paint_cover(ui, logo, u, CornerRadius::ZERO)) {
            ui.painter().text(pos2(strip.left() + 24.0, strip.center().y), Align2::LEFT_CENTER, &g.name, w::font_bold(30.0), Color32::WHITE);
        }

        // Play bar.
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            if running_this {
                if w::button(ui, t, Some(icons::X), "Stop", w::Kind::Danger, w::Size::Lg).clicked() {
                    stop = true;
                }
            } else if w::button(ui, t, Some(icons::PLAY), "Play", w::Kind::Play, w::Size::Lg).clicked() {
                play = true;
            }
            ui.add_space(16.0);
            stat(ui, t, "PLAY TIME", &format_playtime(g.playtime_secs));
            stat(ui, t, "LAST PLAYED", &g.last_played.map(|l| ago(unix_now(), l)).unwrap_or_else(|| "Never".into()));
            if running_this {
                stat(ui, t, "STATUS", "Playing now — friends can see it");
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                w::pill(ui, source_label(g.source), t.text_dim, t.raised);
            });
        });

        // Feature bar.
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            for (tab, icon, label) in [(Tab::About, icons::INFO, "About"), (Tab::Notes, icons::NOTE, "Notes"), (Tab::Lan, icons::USERS, "LAN play"), (Tab::Properties, icons::GEAR, "Properties")] {
                let kind = if self.tab == tab { w::Kind::Primary } else { w::Kind::Ghost };
                if w::button(ui, t, Some(icon), label, kind, w::Size::Sm).clicked() {
                    self.tab = tab;
                }
            }
        });
        ui.add_space(12.0);
        match self.tab {
            Tab::About => self.tab_about(ui, t, i),
            Tab::Notes => self.tab_notes(ui, t, i),
            Tab::Lan => self.tab_lan(ui, t, ctx, i),
            Tab::Properties => self.tab_properties(ui, t, ctx, i),
        }

        if play {
            self.launch(i, None, ctx);
        }
        if stop {
            if let Some(r) = &mut self.running {
                let _ = r.child.kill();
            }
        }
        if back {
            self.selected = None;
        }
    }

    fn tab_about(&mut self, ui: &mut Ui, t: &Theme, i: usize) {
        let g = self.store.games[i].clone();
        w::card(t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if g.meta.is_empty() {
                ui.label(w::text("No details yet. They're fetched from the internet when you're online.", 13.5, t.text_dim));
                if self.finder.candidates.len() > 1 || self.pending.contains_key(&g.exe) {
                    ui.label(w::text("The automatic match wasn't certain. Pick the right game under Properties.", 13.5, t.text));
                }
                if w::button(ui, t, Some(icons::SEARCH), "Find details & art", w::Kind::Secondary, w::Size::Sm).clicked() {
                    self.tab = Tab::Properties;
                }
                return;
            }
            if !g.meta.description.is_empty() {
                ui.label(w::text(&g.meta.description, 14.5, t.text));
                ui.add_space(10.0);
            }
            for (label, value) in [
                ("Developer", g.meta.developers.join(", ")),
                ("Publisher", g.meta.publishers.join(", ")),
                ("Released", g.meta.release_date.clone()),
                ("Genres", g.meta.genres.join(", ")),
            ] {
                if !value.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_sized([100.0, 18.0], egui::Label::new(w::text_medium(label, 13.0, t.text_faint)));
                        ui.label(w::text(value, 13.5, t.text));
                    });
                }
            }
            if !g.meta.features.is_empty() {
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    for f in &g.meta.features {
                        w::pill(ui, f, t.text_dim, t.raised);
                    }
                });
            }
            if !g.meta.website.is_empty() {
                ui.add_space(8.0);
                if ui.link(g.meta.website.as_str()).clicked() {
                    let _ = open::that(&g.meta.website);
                }
            }
        });
    }

    fn tab_notes(&mut self, ui: &mut Ui, t: &Theme, i: usize) {
        w::card(t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(w::text("Your notes for this game: saves, builds, where you stopped.", 13.0, t.text_dim));
            ui.add(egui::TextEdit::multiline(&mut self.store.games[i].notes).desired_rows(10).desired_width(f32::INFINITY).margin(vec2(10.0, 8.0))).with_edit_menu();
        });
    }

    fn tab_lan(&mut self, ui: &mut Ui, t: &Theme, ctx: &mut Ctx, i: usize) {
        self.lan.ensure();
        if self.lan.nick.is_empty() {
            self.lan.nick = ctx.state.persona_name.clone();
        }
        let g = self.store.games[i].clone();
        let mut join: Option<(std::net::IpAddr, u16)> = None;
        w::card(t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(w::text_semibold("Play on your local network", 16.0, t.text));
            ui.label(w::text("Finds friends on the same Wi-Fi or cable. The game's own multiplayer does the playing; this finds who is hosting and starts your game pointed at them. Windows may ask to allow network access the first time.", 13.0, t.text_dim));
            if let Some(e) = &self.lan.error {
                ui.colored_label(Color32::from_rgb(230, 90, 90), e.as_str());
                return;
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.label(w::text_medium("Your name on the network", 13.0, t.text_dim));
                ui.add(egui::TextEdit::singleline(&mut self.lan.nick).desired_width(180.0).margin(vec2(10.0, 6.0))).with_edit_menu();
            });
            ui.add_space(6.0);
            ui.label(w::text_semibold("Host", 14.0, t.text));
            ui.horizontal(|ui| {
                ui.label(w::text("Game port", 13.0, t.text_dim));
                let mut port = if g.host_port == 0 { 25565 } else { g.host_port };
                if ui.add(egui::DragValue::new(&mut port).range(1..=65535)).changed() {
                    self.store.games[i].host_port = port;
                }
                let hosting_this = self.lan.hosting.as_ref().is_some_and(|a| a.game == g.name);
                if hosting_this {
                    if w::button(ui, t, Some(icons::X), "Stop hosting", w::Kind::Danger, w::Size::Sm).clicked() {
                        self.lan.hosting = None;
                    }
                    ui.label(w::text("Announcing to your network", 13.0, t.text_dim));
                } else if w::button(ui, t, Some(icons::USERS), "Host this game", w::Kind::Secondary, w::Size::Sm).clicked() {
                    self.lan.hosting = Some(lan::Announce { instance: self.lan.instance, nick: self.lan.nick.clone(), game: g.name.clone(), port, players: 1, max_players: 0 });
                    self.lan.last_announce = None;
                }
            });
            ui.add_space(8.0);
            ui.label(w::text_semibold("Join", 14.0, t.text));
            ui.horizontal(|ui| {
                ui.label(w::text("Launch options to join", 13.0, t.text_dim));
                ui.add(egui::TextEdit::singleline(&mut self.store.games[i].join_args).hint_text("--connect {ip}:{port}").desired_width(280.0).margin(vec2(10.0, 6.0))).with_edit_menu();
            });
            ui.label(w::text("{ip} and {port} are filled in from the host you pick. Check the game's manual for its connect option.", 12.0, t.text_faint));
            ui.add_space(8.0);

            let now = Instant::now();
            let want = text::normalize(&g.name);
            let rows: Vec<(std::net::IpAddr, lan::Announce)> = self.lan.peers.live(now).into_iter().map(|h| (h.ip, h.announce.clone())).collect();
            let (mine, others): (Vec<_>, Vec<_>) = rows.into_iter().partition(|(_, a)| text::normalize(&a.game) == want);
            if mine.is_empty() {
                ui.label(w::text(format!("Nobody on your network is hosting {} right now.", g.name), 13.5, t.text_dim));
            }
            for (ip, a) in &mine {
                ui.horizontal(|ui| {
                    ui.label(w::text_medium(&a.nick, 14.0, t.text));
                    ui.label(w::text(format!("{ip}:{}", a.port), 13.0, t.text_faint));
                    if w::button(ui, t, Some(icons::PLAY), "Join", w::Kind::Primary, w::Size::Sm).clicked() {
                        join = Some((*ip, a.port));
                    }
                });
            }
            if !others.is_empty() {
                ui.add_space(8.0);
                w::caption(ui, t, "Other games on your network");
                for (ip, a) in &others {
                    ui.label(w::text(format!("{} — {} ({ip}:{})", a.nick, a.game, a.port), 13.0, t.text_dim));
                }
            }
        });
        if let Some((ip, port)) = join {
            if g.join_args.trim().is_empty() {
                ctx.toast("Set the launch options to join first, e.g. --connect {ip}:{port}");
            } else {
                self.launch(i, Some(lan::fill_template(&g.join_args, ip, port)), ctx);
            }
        }
    }

    fn tab_properties(&mut self, ui: &mut Ui, t: &Theme, ctx: &mut Ctx, i: usize) {
        let g = self.store.games[i].clone();
        let running_this = self.running.as_ref().is_some_and(|r| r.exe == g.exe);
        let mut remove = false;
        let mut look_again = false;
        let mut chosen: Option<Candidate> = None;
        let mut search = false;
        let mut browse: Option<ArtKind> = None;
        let mut set_art: Option<(ArtKind, String)> = None;
        let mut status_changed = false;

        w::card(t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(w::text_semibold("Game", 16.0, t.text));
            field(ui, t, "Name", |ui| {
                // The name is part of the game's id, so it stays put while playing.
                ui.add_enabled(!running_this, egui::TextEdit::singleline(&mut self.store.games[i].name).desired_width(360.0).margin(vec2(10.0, 6.0)));
            });
            field(ui, t, "Program", |ui| {
                ui.label(w::text(&g.exe, 13.0, t.text_faint).monospace());
            });
            field(ui, t, "Launch options", |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.store.games[i].args).hint_text("-windowed").desired_width(360.0).margin(vec2(10.0, 6.0))).with_edit_menu();
            });
            field(ui, t, "Status for friends", |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.store.games[i].status).hint_text("e.g. Speedrunning").desired_width(360.0).margin(vec2(10.0, 6.0))).with_edit_menu();
                status_changed = r.lost_focus() && running_this;
            });
            ui.horizontal(|ui| {
                if let Some(dir) = std::path::Path::new(&g.exe).parent() {
                    if w::button(ui, t, Some(icons::FOLDER), "Browse local files", w::Kind::Secondary, w::Size::Sm).clicked() {
                        local::open_folder(dir);
                    }
                }
                if ui.add_enabled_ui(!running_this, |ui| w::button(ui, t, Some(icons::TRASH), "Remove from library", w::Kind::Danger, w::Size::Sm)).inner.clicked() {
                    remove = true;
                }
            });
        });
        ui.add_space(12.0);

        w::card(t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(w::text_semibold("Art & details", 16.0, t.text));
            ui.label(w::text(if g.art_locked { "You chose this look by hand, so it won't change on its own." } else { "Picked automatically. If it's the wrong game, search and choose the right one." }, 13.0, t.text_dim));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.finder.query).hint_text("Game title").desired_width(300.0).margin(vec2(10.0, 6.0))).with_edit_menu();
                if w::button(ui, t, Some(icons::SEARCH), "Search", w::Kind::Primary, w::Size::Sm).clicked() {
                    search = true;
                }
                if g.art_locked && w::button(ui, t, None, "Match automatically again", w::Kind::Ghost, w::Size::Sm).clicked() {
                    look_again = true;
                }
            });
            for c in self.finder.candidates.iter().take(8) {
                ui.horizontal(|ui| {
                    let (thumb, _) = ui.allocate_exact_size(vec2(80.0, 30.0), Sense::hover());
                    if !c.thumb.as_deref().is_some_and(|u| w::paint_cover(ui, thumb, u, t.corner_small())) {
                        ui.painter().rect_filled(thumb, t.corner_small(), t.raised);
                    }
                    ui.label(w::text_medium(&c.title, 14.0, t.text));
                    ui.label(w::text(if c.steam_app_id.is_some() { "Steam store" } else { "SteamGridDB" }, 12.0, t.text_faint));
                    if w::button(ui, t, Some(icons::CHECK), "Use this", w::Kind::Secondary, w::Size::Sm).clicked() {
                        chosen = Some(c.clone());
                    }
                });
            }
            ui.add_space(10.0);
            ui.label(w::text_semibold("Pictures", 14.0, t.text));
            if self.store.sgdb_key.trim().is_empty() {
                ui.label(w::text("Add a free SteamGridDB key in Settings to choose from many more community pictures.", 12.5, t.text_faint));
            }
            for kind in ArtKind::ALL {
                ui.horizontal(|ui| {
                    let current = match kind {
                        ArtKind::Hero => &g.art.hero,
                        ArtKind::Cover => &g.art.cover,
                        ArtKind::Icon => &g.art.icon,
                        ArtKind::Logo => &g.art.logo,
                    };
                    let size = thumb_size(kind);
                    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                    if !current.as_deref().is_some_and(|u| w::paint_cover(ui, rect, u, t.corner_small())) {
                        ui.painter().rect_filled(rect, t.corner_small(), t.raised);
                    }
                    ui.label(w::text_medium(kind.label(), 14.0, t.text));
                    if w::button(ui, t, Some(icons::IMAGE), "Choose…", w::Kind::Secondary, w::Size::Sm).clicked() {
                        browse = Some(kind);
                    }
                });
                if self.finder.showing == Some(kind) {
                    ui.horizontal_wrapped(|ui| {
                        for o in &self.finder.options[kind_index(kind)] {
                            let size = thumb_size(kind);
                            let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                            if !w::paint_cover(ui, rect, &o.thumb, t.corner_small()) {
                                w::skeleton(ui, t, rect, t.corner_small());
                            }
                            if resp.on_hover_text(o.source).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                set_art = Some((kind, o.url.clone()));
                            }
                        }
                    });
                }
            }
        });

        // Apply what was clicked, now that nothing is borrowed.
        if status_changed {
            if let Some(i) = self.running.as_ref().and_then(|r| self.index_of(&r.exe)) {
                let g = &self.store.games[i];
                ctx.send(Command::SetLocalPlaying { game_id: g.game_id(), name: g.name.clone(), status: Some(g.status.clone()).filter(|s| !s.trim().is_empty()) });
            }
        }
        if search && !self.finder.query.trim().is_empty() {
            self.search(g.exe.clone(), self.finder.query.trim().to_string());
        }
        if let Some(c) = chosen {
            self.finder.chosen = Some(c.clone());
            self.finder.options = Default::default();
            self.finder.showing = None;
            self.use_candidate(g.exe.clone(), c);
            ctx.toast("Fetching details and art…");
        }
        if look_again {
            self.store.games[i].art_locked = false;
            self.lookup(g.exe.clone(), g.name.clone());
        }
        if let Some(kind) = browse {
            let c = self.finder.chosen.clone().unwrap_or(Candidate { title: g.name.clone(), steam_app_id: g.meta.steam_app_id, sgdb_id: None, thumb: None });
            self.load_options(g.exe.clone(), c, kind);
        }
        if let Some((kind, url)) = set_art {
            let game = &mut self.store.games[i];
            match kind {
                ArtKind::Hero => game.art.hero = Some(url),
                ArtKind::Cover => game.art.cover = Some(url),
                ArtKind::Icon => game.art.icon = Some(url),
                ArtKind::Logo => game.art.logo = Some(url),
            }
            game.art_locked = true;
        }
        if remove {
            self.store.games.remove(i);
            self.selected = None;
        }
    }
}

impl Extension for LocalGames {
    fn manifest(&self) -> Manifest {
        Manifest { id: "localgames", name: "Local games", author: "SteamInRust", description: "Games that aren't on Steam: library, art, play time, friends' presence and LAN play", ..Manifest::DEFAULT }
    }

    fn pages(&self) -> Vec<PageSpec> {
        vec![PageSpec::new("localgames", "Local games", icons::FOLDER).main().order(25)]
    }

    fn page(&mut self, _page: &str, ui: &mut Ui, ctx: &mut Ctx) {
        let t = ctx.theme.clone();
        egui::ScrollArea::vertical().id_salt("local-games").auto_shrink([false, false]).show(ui, |ui| {
            w::centered(ui, 1400.0, 28.0, |ui| {
                ui.add_space(24.0);
                let open = self.selected.as_deref().and_then(|e| self.index_of(e));
                match open {
                    Some(i) => self.detail(ui, &t, ctx, i),
                    None => {
                        self.selected = None;
                        self.library(ui, &t, ctx);
                    }
                }
                ui.add_space(40.0);
            });
        });
    }

    fn settings(&mut self, ui: &mut Ui, ctx: &mut Ctx) -> bool {
        let t = ctx.theme.clone();
        ui.label(w::text("A free SteamGridDB key (steamgriddb.com, under Preferences > API) lets you pick from many more banners, covers, icons and logos. It stays on this PC and is only sent to SteamGridDB.", 13.0, t.text_dim));
        field(ui, &t, "SteamGridDB key", |ui| {
            ui.add(egui::TextEdit::singleline(&mut self.store.sgdb_key).password(true).desired_width(320.0).margin(vec2(10.0, 6.0))).with_edit_menu();
        });
        field(ui, &t, "Games folder", |ui| {
            ui.add(egui::TextEdit::singleline(&mut self.store.games_folder).hint_text("D:\\Games").desired_width(320.0).margin(vec2(10.0, 6.0))).with_edit_menu();
        });
        true
    }

    fn tick(&mut self, ctx: &mut Ctx) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg, ctx);
        }
        let exited = match &mut self.running {
            Some(r) => !matches!(r.child.try_wait(), Ok(None)),
            None => false,
        };
        if exited {
            self.finish(ctx);
        } else if self.running.is_some() {
            // Wake up now and then to notice the game closing.
            self.egui.request_repaint_after(Duration::from_secs(2));
        }
        self.lan.tick();
        if self.lan.beacon.is_some() {
            self.egui.request_repaint_after(Duration::from_secs(1));
        }
    }

    fn save(&self) -> Option<String> {
        self.store.to_json()
    }

    fn load(&mut self, data: &str) {
        self.store = Store::from_json(data);
    }
}

// ---- small helpers ----

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn ago(now: u64, then: u64) -> String {
    match now.saturating_sub(then) {
        0..=59 => "Just now".into(),
        s @ 60..=3599 => format!("{} min ago", s / 60),
        s @ 3600..=86399 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86400),
    }
}

fn source_label(s: Source) -> &'static str {
    match s {
        Source::Manual => "Local game",
        Source::Folder => "From a games folder",
        Source::Itch => "itch.io",
        Source::Minecraft => "Minecraft",
    }
}

fn kind_index(kind: ArtKind) -> usize {
    ArtKind::ALL.iter().position(|k| *k == kind).unwrap_or(0)
}

fn thumb_size(kind: ArtKind) -> egui::Vec2 {
    match kind {
        ArtKind::Hero => vec2(176.0, 56.0),
        ArtKind::Cover => vec2(84.0, 126.0),
        ArtKind::Icon => vec2(48.0, 48.0),
        ArtKind::Logo => vec2(140.0, 48.0),
    }
}

fn stat(ui: &mut Ui, t: &Theme, label: &str, value: &str) {
    ui.vertical(|ui| {
        ui.label(w::text_medium(label, 10.5, t.text_faint));
        ui.label(w::text_semibold(value, 14.0, t.text));
    });
    ui.add_space(14.0);
}

/// A labelled row in a card.
fn field(ui: &mut Ui, t: &Theme, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized([150.0, 20.0], egui::Label::new(w::text_medium(label, 13.0, t.text_dim)));
        add(ui);
    });
}

/// One game in the grid: its cover with the name and play time under it.
/// True when clicked.
fn tile(ui: &mut Ui, t: &Theme, g: &LocalGame, playing: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(168.0, 300.0), Sense::click());
    let img = Rect::from_min_size(rect.min, vec2(rect.width(), 252.0));
    let r = t.corner();
    let urls: Vec<&str> = [g.art.cover.as_deref(), g.art.hero.as_deref(), g.art.icon.as_deref()].into_iter().flatten().collect();
    if !w::paint_cover_first(ui, img, &urls, r) {
        ui.painter().rect_filled(img, r, t.surface);
        let initial = g.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
        ui.painter().text(img.center(), Align2::CENTER_CENTER, initial, w::font_medium(44.0), t.text_faint);
    }
    if resp.hovered() {
        ui.painter().rect_stroke(img, r, egui::Stroke::new(2.0, t.accent), egui::StrokeKind::Inside);
    }
    if playing {
        ui.painter().text(img.left_top() + vec2(8.0, 8.0), Align2::LEFT_TOP, "● Playing", w::font_semibold(12.0), Color32::WHITE);
    }
    let p = ui.painter();
    p.text(rect.left_top() + vec2(2.0, 260.0), Align2::LEFT_TOP, &g.name, w::font_medium(13.5), t.text);
    p.text(rect.left_top() + vec2(2.0, 280.0), Align2::LEFT_TOP, format_playtime(g.playtime_secs), w::font(12.0), t.text_faint);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}
