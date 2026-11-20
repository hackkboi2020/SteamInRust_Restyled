//! Looking up a game on the internet: its description and details, and
//! pictures to choose from.
//!
//! Two sources:
//! - Steam's public store (no account or key): search, details, and the
//!   official art every store page has.
//! - SteamGridDB (needs a free API key the person adds in settings): lots of
//!   community-made heroes, covers, icons and logos, and the only place most
//!   indie or itch.io games have any.
//!
//! Nothing here is required to play: with no internet every call fails with
//! an error and the library keeps what it has.

use crate::library::{Art, Meta};
use crate::text::strip_html;
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network: {0}")]
    Http(#[from] reqwest::Error),
    #[error("unexpected reply from {0}")]
    Shape(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

const STEAM_CDN: &str = "https://cdn.akamai.steamstatic.com/steam/apps";
const SGDB: &str = "https://www.steamgriddb.com/api/v2";

/// A game that might be the one the person means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub title: String,
    pub steam_app_id: Option<u32>,
    pub sgdb_id: Option<u64>,
    /// Small picture for the list.
    pub thumb: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtKind {
    Hero,
    Cover,
    Icon,
    Logo,
}

impl ArtKind {
    pub const ALL: [ArtKind; 4] = [ArtKind::Hero, ArtKind::Cover, ArtKind::Icon, ArtKind::Logo];

    pub fn label(self) -> &'static str {
        match self {
            ArtKind::Hero => "Hero",
            ArtKind::Cover => "Cover",
            ArtKind::Icon => "Icon",
            ArtKind::Logo => "Logo",
        }
    }

    fn sgdb_path(self) -> &'static str {
        match self {
            ArtKind::Hero => "heroes",
            ArtKind::Cover => "grids",
            ArtKind::Icon => "icons",
            ArtKind::Logo => "logos",
        }
    }
}

/// One picture the person can choose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtOption {
    pub url: String,
    pub thumb: String,
    pub source: &'static str,
}

#[derive(Clone)]
pub struct Fetcher {
    http: reqwest::Client,
    sgdb_key: Option<String>,
}

impl Fetcher {
    pub fn new(sgdb_key: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("SteamInRust/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .build()?;
        let key = sgdb_key.trim();
        Ok(Self { http, sgdb_key: (!key.is_empty()).then(|| key.to_string()) })
    }

    /// Games matching `query`, Steam's store first, then SteamGridDB's.
    /// A source that fails is skipped; it is an error only if both do.
    pub async fn search(&self, query: &str) -> Result<Vec<Candidate>> {
        let steam = self.steam_search(query).await;
        let grid = match &self.sgdb_key {
            Some(_) => self.sgdb_search(query).await,
            None => Ok(Vec::new()),
        };
        match (steam, grid) {
            (Err(e), Err(_)) => Err(e),
            (steam, grid) => {
                let mut out = steam.unwrap_or_default();
                for c in grid.unwrap_or_default() {
                    if !out.iter().any(|o| crate::text::normalize(&o.title) == crate::text::normalize(&c.title)) {
                        out.push(c);
                    }
                }
                Ok(out)
            }
        }
    }

    async fn steam_search(&self, query: &str) -> Result<Vec<Candidate>> {
        let v: Value = self
            .http
            .get("https://store.steampowered.com/api/storesearch/")
            .query(&[("term", query), ("cc", "us"), ("l", "english")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        parse_steam_search(&v).ok_or(Error::Shape("Steam store search"))
    }

    async fn sgdb_search(&self, query: &str) -> Result<Vec<Candidate>> {
        let v = self.sgdb_get(&format!("{SGDB}/search/autocomplete/{}", encode_path(query))).await?;
        parse_sgdb_search(&v).ok_or(Error::Shape("SteamGridDB search"))
    }

    async fn sgdb_get(&self, url: &str) -> Result<Value> {
        let key = self.sgdb_key.as_deref().unwrap_or_default();
        Ok(self.http.get(url).bearer_auth(key).send().await?.error_for_status()?.json().await?)
    }

    /// Description, developer, genres and so on from Steam's store page.
    pub async fn metadata(&self, steam_app_id: u32) -> Result<Meta> {
        let v: Value = self
            .http
            .get("https://store.steampowered.com/api/appdetails")
            .query(&[("appids", steam_app_id.to_string()), ("l", "english".into())])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        parse_app_details(&v, steam_app_id).ok_or(Error::Shape("Steam app details"))
    }

    /// Does this address answer? Steam moved some games' art to new
    /// addresses, so an official picture is only offered if it is really there.
    async fn reachable(&self, url: &str) -> bool {
        self.http.head(url).send().await.is_ok_and(|r| r.status().is_success())
    }

    /// Everything useful about a chosen game in one go: its details and the
    /// first picture of each kind.
    pub async fn fill(&self, c: &Candidate) -> (Option<Meta>, Art) {
        let mut c = c.clone();
        if c.sgdb_id.is_none() {
            c.sgdb_id = self.sgdb_for(&c.title).await;
        }
        let meta = match c.steam_app_id {
            Some(id) => self.metadata(id).await.ok(),
            None => None,
        };
        let mut art = Art::default();
        for kind in ArtKind::ALL {
            let first = self.art_options(&c, kind).await.into_iter().next().map(|o| o.url);
            match kind {
                ArtKind::Hero => art.hero = first,
                ArtKind::Cover => art.cover = first,
                ArtKind::Icon => art.icon = first,
                ArtKind::Logo => art.logo = first,
            }
        }
        (meta, art)
    }

    /// Pictures of one kind for a game, official ones first.
    pub async fn art_options(&self, c: &Candidate, kind: ArtKind) -> Vec<ArtOption> {
        let mut out = Vec::new();
        if let Some(official) = c.steam_app_id.and_then(|id| steam_art(id, kind)) {
            if self.reachable(&official.url).await {
                out.push(official);
            }
        }
        if let (Some(_), Some(gid)) = (&self.sgdb_key, c.sgdb_id) {
            if let Ok(v) = self.sgdb_get(&format!("{SGDB}/{}/game/{gid}", kind.sgdb_path())).await {
                out.extend(parse_sgdb_art(&v));
            }
        }
        out
    }

    /// SteamGridDB's own id for a game we only know from Steam (so its art
    /// can be offered too). `None` without a key or when not found.
    pub async fn sgdb_for(&self, title: &str) -> Option<u64> {
        self.sgdb_key.as_ref()?;
        self.sgdb_search(title).await.ok()?.into_iter().find(|c| crate::text::normalize(&c.title) == crate::text::normalize(title))?.sgdb_id
    }
}

/// The official pictures every Steam store page has.
pub fn steam_art(app_id: u32, kind: ArtKind) -> Option<ArtOption> {
    let file = match kind {
        ArtKind::Hero => "library_hero.jpg",
        ArtKind::Cover => "library_600x900.jpg",
        ArtKind::Logo => "logo.png",
        // The store has no square icon; the header is the closest official one.
        ArtKind::Icon => return None,
    };
    let url = format!("{STEAM_CDN}/{app_id}/{file}");
    Some(ArtOption { thumb: url.clone(), url, source: "Steam" })
}

/// Percent-encode a path segment (a game title in a URL).
fn encode_path(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn parse_steam_search(v: &Value) -> Option<Vec<Candidate>> {
    let items = v.get("items")?.as_array()?;
    Some(
        items
            .iter()
            .filter(|i| i.get("type").and_then(Value::as_str).is_none_or(|t| t == "app"))
            .filter_map(|i| {
                let id = u32::try_from(i.get("id")?.as_u64()?).ok()?;
                Some(Candidate {
                    title: i.get("name")?.as_str()?.to_string(),
                    steam_app_id: Some(id),
                    sgdb_id: None,
                    thumb: i.get("tiny_image").and_then(Value::as_str).map(String::from),
                })
            })
            .take(10)
            .collect(),
    )
}

fn parse_sgdb_search(v: &Value) -> Option<Vec<Candidate>> {
    if v.get("success")?.as_bool()? != true {
        return None;
    }
    Some(
        v.get("data")?
            .as_array()?
            .iter()
            .filter_map(|g| Some(Candidate { title: g.get("name")?.as_str()?.to_string(), steam_app_id: None, sgdb_id: g.get("id")?.as_u64(), thumb: None }))
            .take(10)
            .collect(),
    )
}

fn parse_sgdb_art(v: &Value) -> Vec<ArtOption> {
    v.get("data")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    let url = a.get("url")?.as_str()?;
                    let thumb = a.get("thumb").and_then(Value::as_str).unwrap_or(url);
                    // https only: never fetch pictures over plain http.
                    (url.starts_with("https://")).then(|| ArtOption { url: url.to_string(), thumb: thumb.to_string(), source: "SteamGridDB" })
                })
                .take(24)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_app_details(v: &Value, app_id: u32) -> Option<Meta> {
    let entry = v.get(app_id.to_string())?;
    if entry.get("success")?.as_bool()? != true {
        return None;
    }
    let d = entry.get("data")?;
    let strings = |key: &str| -> Vec<String> { d.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default() };
    let described = |key: &str| -> Vec<String> {
        d.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(|g| g.get("description")?.as_str().map(String::from)).collect()).unwrap_or_default()
    };
    Some(Meta {
        description: strip_html(d.get("short_description").and_then(Value::as_str).unwrap_or_default()),
        developers: strings("developers"),
        publishers: strings("publishers"),
        release_date: d.get("release_date").and_then(|r| r.get("date")).and_then(Value::as_str).unwrap_or_default().to_string(),
        genres: described("genres"),
        features: described("categories"),
        website: d.get("website").and_then(Value::as_str).filter(|w| w.starts_with("https://") || w.starts_with("http://")).unwrap_or_default().to_string(),
        steam_app_id: Some(app_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn steam_search_results_are_read() {
        let v = json!({"total": 2, "items": [
            {"type": "app", "name": "Hades", "id": 1145360, "tiny_image": "https://x/t.jpg"},
            {"type": "sub", "name": "Hades Bundle", "id": 5}
        ]});
        let found = parse_steam_search(&v).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "Hades");
        assert_eq!(found[0].steam_app_id, Some(1145360));
        assert!(parse_steam_search(&json!({"nope": 1})).is_none());
    }

    #[test]
    fn app_details_are_read_and_cleaned() {
        let v = json!({"1145360": {"success": true, "data": {
            "short_description": "Defy the god of the dead &amp; <b>escape</b>.",
            "developers": ["Supergiant Games"], "publishers": ["Supergiant Games"],
            "release_date": {"coming_soon": false, "date": "17 Sep, 2020"},
            "genres": [{"id": "1", "description": "Action"}],
            "categories": [{"id": 2, "description": "Single-player"}],
            "website": "https://www.supergiantgames.com/"
        }}});
        let m = parse_app_details(&v, 1145360).unwrap();
        assert_eq!(m.description, "Defy the god of the dead & escape.");
        assert_eq!(m.developers, ["Supergiant Games"]);
        assert_eq!(m.genres, ["Action"]);
        assert_eq!(m.features, ["Single-player"]);
        assert_eq!(m.steam_app_id, Some(1145360));
    }

    #[test]
    fn a_failed_details_reply_is_not_data() {
        assert!(parse_app_details(&json!({"9": {"success": false}}), 9).is_none());
        assert!(parse_app_details(&json!({}), 9).is_none());
    }

    #[test]
    fn sgdb_art_keeps_only_https_pictures() {
        let v = json!({"success": true, "data": [
            {"id": 1, "url": "https://cdn2.steamgriddb.com/a.png", "thumb": "https://cdn2.steamgriddb.com/t.png"},
            {"id": 2, "url": "http://insecure/a.png"},
            {"id": 3}
        ]});
        let art = parse_sgdb_art(&v);
        assert_eq!(art.len(), 1);
        assert_eq!(art[0].source, "SteamGridDB");
    }

    #[test]
    fn titles_are_encoded_for_the_url_path() {
        assert_eq!(encode_path("Dead Cells"), "Dead%20Cells");
        assert_eq!(encode_path("a/b?c"), "a%2Fb%3Fc");
    }

    #[test]
    fn official_steam_art_has_no_icon() {
        assert!(steam_art(10, ArtKind::Icon).is_none());
        assert!(steam_art(10, ArtKind::Hero).unwrap().url.ends_with("/10/library_hero.jpg"));
    }
}
