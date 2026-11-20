# Local games

Games that aren't on Steam get their own page in SteamInRust: **Local games**
(top navigation, between Library and Community).

## What it does

| | |
| --- | --- |
| **Add games** | One program, a whole games folder (each subfolder is a game), the itch.io app's library, or the Minecraft launcher. The right `.exe` is picked for you (uninstallers, crash handlers and redistributables are skipped). |
| **Art and details** | When a game is added, its banner (hero), cover, icon, logo, description, developer, genres and features are looked up on the internet. A clear match is applied automatically. If the match isn't clear, nothing is guessed: open the game's **Properties** tab, search, and choose the right one. Every picture can be swapped by hand, and a hand-picked look is never overwritten. |
| **Steam-style game page** | Hero banner, play bar (Play/Stop, play time, last played), and a feature bar underneath: **About**, **Notes**, **LAN play**, **Properties**. |
| **Friends see what you play** | While a local game runs, Steam friends see "Playing *name*", the same as for a shortcut added in the official client (the game gets the same shortcut-style id Steam would give it). You can set a status line per game. |
| **Play time** | Counted per game, kept on this PC. |
| **LAN play** | Hosts announce themselves over UDP on the local network; the LAN tab lists who is hosting your game and starts it with the host's address filled into your launch options (`--connect {ip}:{port}`). Discovery only: the game's own multiplayer does the playing. |
| **Offline** | Everything works offline. Art and details already seen are served from the image cache; lookups that fail offline are retried when you search again or re-add the game. |

## Settings

*Settings > Local games*: an optional free [SteamGridDB](https://www.steamgriddb.com)
API key (adds many community banners, covers, icons and logos; indie and itch.io
games usually have nothing else) and your games folder.

## Where things are

```
crates/localgames/     the logic, no UI
  id.rs                Steam's non-Steam-game id (CRC-32 based)
  exe.rs               choosing a game's main program
  text.rs              name matching, launch-option splitting, HTML cleanup
  lan.rs               LAN announcements (UDP), host table
  scan.rs              games folder, itch.io, Minecraft
  fetch.rs             Steam store + SteamGridDB lookups
  library.rs           the saved list
crates/client/         Command::SetLocalPlaying / ClearLocalPlaying
apps/desktop/src/builtin/local_games.rs    the page
```

`id`, `exe`, `text` and `lan` use only the standard library on purpose.

## Testing

```
cargo test -p localgames
```

## Known limits

- **Rich presence.** Steam shows "Playing *name*" for sure. A custom status
  line is uploaded as rich presence, but whether a friend's client shows it for
  a non-Steam game is up to their client; it has not been checked against one.
- **Launcher-style games.** Play time and presence follow the process that was
  started. If a game's `.exe` is only a launcher that starts the real game and
  exits, tracking ends when the launcher does. Minecraft's launcher stays open
  by default; the Microsoft Store edition isn't imported.
- **Overlay.** For these games the Shift+Tab overlay is the always-on-top
  window (works over windowed and borderless games), not the in-game D3D11
  overlay, which needs the game to load SteamInRust's `steamclient`.
- **Steam game ids.** The shortcut-style id is computed from the program path and
  name, so renaming a game or moving its `.exe` changes it. Friends see the new
  name; nothing else is affected.
- **Strings** are English only so far (not yet in `locales/ru.toml`).
