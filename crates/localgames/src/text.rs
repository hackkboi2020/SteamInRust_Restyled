//! Small text helpers: matching a game's name against search results and
//! cleaning descriptions fetched from the web.

/// Lowercase letters and digits only, so "The Witness™" and "the witness"
/// compare equal.
pub fn normalize(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// A folder name turned into a display name: separators become spaces and
/// version or build tags are dropped ("My_Game-v1.2.3" -> "My Game").
pub fn display_name(folder: &str) -> String {
    let folder = cut_version(folder);
    let spaced: String = folder.chars().map(|c| if matches!(c, '_' | '.') { ' ' } else { c }).collect();
    let mut words: Vec<&str> = Vec::new();
    for w in spaced.split_whitespace() {
        let lower = w.to_ascii_lowercase();
        let looks_like_version = lower.strip_prefix('v').is_some_and(|r| r.chars().next().is_some_and(|c| c.is_ascii_digit()));
        let is_tag = matches!(lower.as_str(), "win64" | "win32" | "x64" | "x86" | "windows" | "portable" | "setup" | "build");
        if looks_like_version || is_tag {
            continue;
        }
        words.push(w.trim_matches('-'));
    }
    let name = words.into_iter().filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
    if name.is_empty() { folder.trim().to_string() } else { name }
}

/// Everything before a "-v1.2"-style suffix ("My_Game-v1.2.3" -> "My_Game").
fn cut_version(s: &str) -> &str {
    let b = s.as_bytes();
    for i in 0..b.len().saturating_sub(2) {
        if matches!(b[i], b'-' | b'_' | b' ' | b'.') && matches!(b[i + 1], b'v' | b'V') && b[i + 2].is_ascii_digit() {
            return &s[..i];
        }
    }
    s
}

/// Which of `titles` is the game called `query`? Only a confident match is
/// returned: an exact (normalized) match, else one title that contains the
/// whole query or is contained by it. Anything unclear returns `None`, so
/// the person picks instead of getting wrong art.
pub fn best_match(query: &str, titles: &[&str]) -> Option<usize> {
    let q = normalize(query);
    if q.len() < 2 {
        return None;
    }
    if let Some(i) = titles.iter().position(|t| normalize(t) == q) {
        return Some(i);
    }
    let close: Vec<usize> = titles
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            let t = normalize(t);
            t.len() >= 3 && (t.starts_with(&q) || q.starts_with(&t))
        })
        .map(|(i, _)| i)
        .collect();
    // Only when exactly one result fits: two sequels both fit "Portal".
    if close.len() == 1 { Some(close[0]) } else { None }
}

/// Split launch options into arguments: spaces separate them, double quotes
/// keep spaces together (`--name "Two Words"` is two arguments).
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(cur);
    }
    out
}

/// Plain text from the small bit of HTML store descriptions contain.
pub fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut tag: Option<String> = None;
    for c in s.chars() {
        match (&mut tag, c) {
            (None, '<') => tag = Some(String::new()),
            (Some(name), '>') => {
                // Line-breaking tags separate words; inline ones (<b>) don't.
                let n = name.trim_start_matches('/').to_ascii_lowercase();
                if ["br", "p", "li", "div", "h1", "h2", "h3", "ul", "ol"].iter().any(|b| n == *b || n.starts_with(&format!("{b} ")) || n.starts_with(&format!("{b}/"))) {
                    out.push(' ');
                }
                tag = None;
            }
            (Some(name), c) => name.push(c),
            (None, c) => out.push(c),
        }
    }
    let out = out
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ignores_case_and_symbols() {
        assert_eq!(normalize("The Witness™"), "thewitness");
        assert_eq!(normalize("HADES"), normalize("hades"));
    }

    #[test]
    fn display_name_drops_separators_and_versions() {
        assert_eq!(display_name("My_Game-v1.2.3"), "My Game");
        assert_eq!(display_name("Celeste_win64"), "Celeste");
        assert_eq!(display_name("Dead.Cells"), "Dead Cells");
        assert_eq!(display_name("v1"), "v1");
    }

    #[test]
    fn exact_match_wins_even_with_sequels_listed() {
        let titles = ["Portal 2", "Portal", "Portal Stories: Mel"];
        assert_eq!(best_match("portal", &titles), Some(1));
    }

    #[test]
    fn ambiguous_or_unrelated_results_are_not_guessed() {
        assert_eq!(best_match("Portal", &["Portal 2", "Portal Stories: Mel"]), None);
        assert_eq!(best_match("Hades", &["Minecraft"]), None);
        assert_eq!(best_match("x", &["X"]), None);
    }

    #[test]
    fn a_single_close_title_is_accepted() {
        assert_eq!(best_match("Hades", &["Hades II", "Minecraft"]), Some(0));
    }

    #[test]
    fn launch_options_split_like_a_command_line() {
        assert_eq!(split_args("-windowed  -w 1280"), ["-windowed", "-w", "1280"]);
        assert_eq!(split_args(r#"--name "Two Words" -x"#), ["--name", "Two Words", "-x"]);
        assert_eq!(split_args(r#""" a"#), ["", "a"]);
        assert!(split_args("   ").is_empty());
        assert!(split_args("").is_empty());
    }

    #[test]
    fn strip_html_removes_tags_and_decodes_entities() {
        assert_eq!(strip_html("Fight &amp; <b>escape</b><br>the &quot;Underworld&quot;"), "Fight & escape the \"Underworld\"");
        assert_eq!(strip_html("<p>One</p><p>Two</p>"), "One Two");
        assert_eq!(strip_html("a <b>bold</b>."), "a bold.");
        assert_eq!(strip_html("1 < 2"), "1");
    }
}
