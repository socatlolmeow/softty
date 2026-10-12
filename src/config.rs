use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Deserializer};
use xkbcommon::xkb;
use xkbcommon::xkb::keysyms::{KEY_C, KEY_End, KEY_Home, KEY_Insert, KEY_N, KEY_Next, KEY_Prior, KEY_R, KEY_V, KEY_X, KEY_Z};

pub const SHIFT: u8 = 1;
pub const ALT: u8 = 2;
pub const CTRL: u8 = 4;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorStyle {
    Block,
    Beam,
    Underline,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Copy,
    Paste,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ScrollLineUp,
    ScrollLineDown,
    ScrollPageUp,
    ScrollPageDown,
    ScrollTop,
    ScrollBottom,
    Search,
    PrevPrompt,
    NextPrompt,
    NewWindow,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub term: String,
    pub window: Window,
    pub font: Font,
    pub mouse: Mouse,
    pub scrollback: Scrollback,
    pub colors: Colors,
    pub cursor: Cursor,
    pub shortcuts: Vec<Shortcut>,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Window {
    pub title: String,
    pub app_id: String,
    pub cols: usize,
    pub rows: usize,
    pub border: usize,
    pub alpha: f32,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Font {
    pub family: String,
    pub size: f32,
    pub zoom_step: f32,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Mouse {
    pub word_delims: String,
    pub multi_click_ms: u64,
    pub scroll_lines: usize,
    pub url_launcher: String,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scrollback {
    pub lines: usize,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Colors {
    #[serde(deserialize_with = "color")]
    pub foreground: u32,
    #[serde(deserialize_with = "color")]
    pub background: u32,
    #[serde(deserialize_with = "palette")]
    pub palette: [u32; 16],
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Cursor {
    pub style: CursorStyle,
    #[serde(deserialize_with = "color")]
    pub color: u32,
    #[serde(deserialize_with = "color")]
    pub text: u32,
    pub thickness: usize,
}

#[derive(Deserialize)]
#[serde(try_from = "RawShortcut")]
pub struct Shortcut {
    pub mods: u8,
    pub sym: u32,
    pub action: Action,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawShortcut {
    #[serde(default)]
    mods: Vec<String>,
    key: String,
    action: Action,
}

impl TryFrom<RawShortcut> for Shortcut {
    type Error = String;

    fn try_from(raw: RawShortcut) -> Result<Shortcut, String> {
        let mut mods = 0;
        for m in &raw.mods {
            mods |= match m.to_ascii_lowercase().as_str() {
                "shift" => SHIFT,
                "alt" => ALT,
                "ctrl" | "control" => CTRL,
                _ => return Err(format!("unknown modifier {m:?}, expected \"shift\", \"alt\" or \"ctrl\"")),
            };
        }
        // Letters arrive as upper case keysyms while shift is held, lower case otherwise.
        let name = if raw.key.len() == 1 && raw.key.as_bytes()[0].is_ascii_alphabetic() {
            if mods & SHIFT != 0 { raw.key.to_ascii_uppercase() } else { raw.key.to_ascii_lowercase() }
        } else {
            raw.key.clone()
        };
        let sym = xkb::keysym_from_name(&name, xkb::KEYSYM_NO_FLAGS).raw();
        if sym == 0 {
            return Err(format!("unknown key {:?}, use an xkb keysym name such as \"C\", \"Insert\" or \"Page_Up\"", raw.key));
        }
        Ok(Shortcut { mods, sym, action: raw.action })
    }
}

fn parse_color(text: &str) -> Result<u32, String> {
    let hex = text.trim().trim_start_matches('#').trim_start_matches("0x");
    match u32::from_str_radix(hex, 16) {
        Ok(value) if hex.len() == 6 => Ok(value),
        _ => Err(format!("invalid color {text:?}, expected \"#rrggbb\"")),
    }
}

fn color<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    parse_color(&String::deserialize(d)?).map_err(serde::de::Error::custom)
}

fn palette<'de, D: Deserializer<'de>>(d: D) -> Result<[u32; 16], D::Error> {
    let list = Vec::<String>::deserialize(d)?;
    if list.len() != 16 {
        return Err(serde::de::Error::custom(format!("palette needs exactly 16 colors, got {}", list.len())));
    }
    let mut out = [0; 16];
    for (slot, text) in out.iter_mut().zip(&list) {
        *slot = parse_color(text).map_err(serde::de::Error::custom)?;
    }
    Ok(out)
}

impl Default for Config {
    fn default() -> Config {
        Config {
            term: "xterm-256color".into(),
            window: Window::default(),
            font: Font::default(),
            mouse: Mouse::default(),
            scrollback: Scrollback::default(),
            colors: Colors::default(),
            cursor: Cursor::default(),
            shortcuts: vec![
                Shortcut { mods: CTRL | SHIFT, sym: KEY_C, action: Action::Copy },
                Shortcut { mods: CTRL | SHIFT, sym: KEY_V, action: Action::Paste },
                Shortcut { mods: CTRL, sym: KEY_Insert, action: Action::Copy },
                Shortcut { mods: SHIFT, sym: KEY_Insert, action: Action::Paste },
                Shortcut { mods: CTRL | SHIFT, sym: KEY_Prior, action: Action::ZoomIn },
                Shortcut { mods: CTRL | SHIFT, sym: KEY_Next, action: Action::ZoomOut },
                Shortcut { mods: CTRL | SHIFT, sym: KEY_Home, action: Action::ZoomReset },
                Shortcut { mods: SHIFT, sym: KEY_Prior, action: Action::ScrollPageUp },
                Shortcut { mods: SHIFT, sym: KEY_Next, action: Action::ScrollPageDown },
                Shortcut { mods: SHIFT, sym: KEY_Home, action: Action::ScrollTop },
                Shortcut { mods: SHIFT, sym: KEY_End, action: Action::ScrollBottom },
                Shortcut { mods: CTRL | SHIFT, sym: KEY_R, action: Action::Search },
                Shortcut { mods: CTRL | SHIFT, sym: KEY_Z, action: Action::PrevPrompt },
                Shortcut { mods: CTRL | SHIFT, sym: KEY_X, action: Action::NextPrompt },
                Shortcut { mods: CTRL | SHIFT, sym: KEY_N, action: Action::NewWindow },
            ],
        }
    }
}

impl Default for Window {
    fn default() -> Window {
        Window { title: "softty".into(), app_id: "softty".into(), cols: 80, rows: 24, border: 10, alpha: 0.5 }
    }
}

impl Default for Font {
    fn default() -> Font {
        Font { family: "Iosevka Nerd Font".into(), size: 24.0, zoom_step: 2.0 }
    }
}

impl Default for Mouse {
    fn default() -> Mouse {
        Mouse { word_delims: " `'\"()[]{}<>|&;,".into(), multi_click_ms: 300, scroll_lines: 3, url_launcher: "xdg-open".into() }
    }
}

impl Default for Scrollback {
    fn default() -> Scrollback {
        Scrollback { lines: 10_000 }
    }
}

impl Default for Colors {
    fn default() -> Colors {
        Colors {
            foreground: 0xa9b1d6,
            background: 0x1a1b26,
            palette: [
                0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6,
                0x414868, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
            ],
        }
    }
}

impl Default for Cursor {
    fn default() -> Cursor {
        Cursor { style: CursorStyle::Block, color: 0x7aa2f7, text: 0x1a1b26, thickness: 2 }
    }
}

impl Config {
    /// Returns the key that is wrong and what is wrong with it.
    fn check(&self) -> Result<(), (&'static str, &'static str)> {
        if self.window.cols == 0 {
            return Err(("cols", "window.cols must be at least 1"));
        }
        if self.window.rows == 0 {
            return Err(("rows", "window.rows must be at least 1"));
        }
        if !(self.font.size > 0.0) {
            return Err(("size", "font.size must be greater than 0"));
        }
        Ok(())
    }
}

/// Written to the config path on first run. Every value in it is a default.
const TEMPLATE: &str = r##"# softty configuration
# Created on first run at ~/.config/softty/config.toml (or $XDG_CONFIG_HOME/softty/config.toml).
# Every key is optional; anything you leave out uses the default shown here.
# Colors are "#rrggbb" strings.

# $TERM exported to the shell
term = "xterm-256color"

[window]
title = "softty"
app_id = "softty"
cols = 80          # initial size in cells
rows = 24
border = 10        # padding around the grid, in pixels
alpha = 0.5         # background opacity, 0.0 (clear) to 1.0 (opaque)

[font]
family = "Iosevka Nerd Font"   # anything fontconfig can match
size = 24.0
zoom_step = 2.0

[mouse]
word_delims = " `'\"()[]{}<>|&;,"   # characters that end a word on double-click
multi_click_ms = 300                 # max gap between clicks for double/triple click
scroll_lines = 3                     # lines per wheel notch (history, or arrow keys on the alternate screen)
url_launcher = "xdg-open"

[scrollback]
lines = 10000                        # history kept per terminal, 0 turns scrollback off

[colors]
foreground = "#a9b1d6"
background = "#1a1b26"
# 16 ANSI colors: 8 normal, then 8 bright
palette = [
  "#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6",
  "#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5",
]

[cursor]
style = "block"            # "block", "beam" or "underline"
color = "#7aa2f7"
text = "#1a1b26"       # glyph color under a block cursor
thickness = 2          # width of beam / height of underline, in pixels

# Shortcuts. Setting any [[shortcuts]] replaces the whole default list.
# mods: "shift", "alt", "ctrl". key: an xkb keysym name (C, Insert, Page_Up, F5, ...).
# action: copy, paste, zoom_in, zoom_out, zoom_reset,
#         scroll_line_up, scroll_line_down, scroll_page_up, scroll_page_down, scroll_top, scroll_bottom
# The scroll_* actions are passed on to the program instead on the alternate screen (vim, less, ...).
[[shortcuts]]
mods = ["ctrl", "shift"]
key = "C"
action = "copy"

[[shortcuts]]
mods = ["ctrl", "shift"]
key = "V"
action = "paste"

[[shortcuts]]
mods = ["ctrl"]
key = "Insert"
action = "copy"

[[shortcuts]]
mods = ["shift"]
key = "Insert"
action = "paste"

[[shortcuts]]
mods = ["ctrl", "shift"]
key = "Prior"
action = "zoom_in"

[[shortcuts]]
mods = ["ctrl", "shift"]
key = "Next"
action = "zoom_out"

[[shortcuts]]
mods = ["ctrl", "shift"]
key = "Home"
action = "zoom_reset"

[[shortcuts]]
mods = ["shift"]
key = "Prior"          # Page Up
action = "scroll_page_up"

[[shortcuts]]
mods = ["shift"]
key = "Next"           # Page Down
action = "scroll_page_down"

[[shortcuts]]
mods = ["shift"]
key = "Home"
action = "scroll_top"

[[shortcuts]]
mods = ["shift"]
key = "End"
action = "scroll_bottom"

[[shortcuts]]
mods = ["ctrl", "shift"]
key = "R"
action = "search"

[[shortcuts]]
mods = ["ctrl", "shift"]
key = "Z"
action = "prev_prompt"

[[shortcuts]]
mods = ["ctrl", "shift"]
key = "X"
action = "next_prompt"

[[shortcuts]]
mods = ["ctrl", "shift"]
key = "N"
action = "new_window"
"##;

static CONFIG: OnceLock<Config> = OnceLock::new();

/// The active configuration. Falls back to the defaults if `load` has not run.
pub fn get() -> &'static Config {
    CONFIG.get_or_init(Config::default)
}

/// `$XDG_CONFIG_HOME/softty/config.toml`, which is `~/.config/softty/config.toml` by default.
fn path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("softty").join("config.toml"))
}

/// Reads the config file once at startup. A missing file is created from the template.
/// If the file is unreadable or invalid the defaults are used too, and the
/// returned message says what is wrong and where, ready to show to the user.
pub fn load() -> Option<String> {
    let (config, error) = match path().map(|p| read(&p)) {
        Some(Err(message)) => (Config::default(), Some(message)),
        Some(Ok(config)) => (config, None),
        None => (Config::default(), None),
    };
    let _ = CONFIG.set(config);
    error
}

fn read(path: &Path) -> Result<Config, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            create_default(path);
            return Ok(Config::default());
        }
        Err(e) => return Err(format!("{}: cannot read file: {e}", path.display())),
    };
    let config: Config = toml::from_str(&text)
        .map_err(|e| report(path, &text, e.span().map(|s| s.start), e.message()))?;
    config
        .check()
        .map_err(|(key, message)| report(path, &text, find_key(&text, key), message))?;
    Ok(config)
}

fn create_default(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| file.write_all(TEMPLATE.as_bytes()));
    match created {
        Err(e) if e.kind() != ErrorKind::AlreadyExists => eprintln!("softty: cannot create {}: {e}", path.display()),
        _ => {}
    }
}

/// Byte offset of the first line that assigns `key`.
fn find_key(text: &str, key: &str) -> Option<usize> {
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let indent = line.len() - line.trim_start().len();
        if let Some(after) = line[indent..].strip_prefix(key) {
            if after.trim_start().starts_with('=') {
                return Some(start + indent);
            }
        }
        start += line.len();
    }
    None
}

/// Formats `path:line:col: message`, followed by the offending line and a caret under the spot.
fn report(path: &Path, text: &str, offset: Option<usize>, message: &str) -> String {
    let Some(offset) = offset else { return format!("{}: {message}", path.display()) };
    let offset = offset.min(text.len());
    let line_no = text[..offset].matches('\n').count() + 1;
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let col = text[line_start..offset].chars().count() + 1;
    let source = text[line_start..].lines().next().unwrap_or("");
    let gutter = line_no.to_string();
    let pad = " ".repeat(gutter.len());
    format!(
        "{}:{line_no}:{col}: {message}\n {gutter} | {source}\n {pad} | {}^",
        path.display(),
        " ".repeat(col - 1)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_points_at_the_offending_spot() {
        let text = "a = 1\nbad = 2\n";
        let message = report(Path::new("c.toml"), text, Some(6), "oops");
        assert_eq!(message, "c.toml:2:1: oops\n 2 | bad = 2\n   | ^");
    }

    #[test]
    fn template_is_valid_and_matches_defaults() {
        let config: Config = toml::from_str(TEMPLATE).unwrap();
        let default = Config::default();
        assert!(config.check().is_ok());
        assert_eq!(config.colors.palette, default.colors.palette);
        assert_eq!(config.mouse.word_delims, default.mouse.word_delims);
        assert_eq!(config.shortcuts.len(), default.shortcuts.len());
    }

    #[test]
    fn find_key_skips_other_keys() {
        let text = "[window]\ncolumns = 1\n  cols = 0\n";
        assert_eq!(find_key(text, "cols"), Some(text.find("cols").unwrap()));
    }
}
