//! Picks up the colors and font of the terminal emulator configured on this
//! machine (Ghostty or Konsole), so soi looks like the user's usual terminal.
//! Color schemes can also be chosen by name: Konsole schemes and Ghostty
//! themes installed on the system, plus a few built-in ones.

use std::fs;
use std::path::PathBuf;

use gtk::{gdk, pango};

#[derive(Default)]
pub struct TerminalTheme {
    pub font: Option<pango::FontDescription>,
    pub foreground: Option<gdk::RGBA>,
    pub background: Option<gdk::RGBA>,
    /// Either empty (xterm's colors) or the 16 ANSI colors.
    pub palette: Vec<gdk::RGBA>,
    /// Background opacity, 0.0..=1.0.
    pub opacity: Option<f64>,
}

impl TerminalTheme {
    /// Ghostty if it is configured, otherwise Konsole's default profile,
    /// otherwise default colors with the system monospace font.
    pub fn detect() -> Self {
        ghostty()
            .filter(|theme| !theme.is_empty())
            .or_else(konsole)
            .unwrap_or_default()
    }

    fn is_empty(&self) -> bool {
        self.font.is_none()
            && self.foreground.is_none()
            && self.background.is_none()
            && self.palette.is_empty()
            && self.opacity.is_none()
    }

    pub fn foreground(&self) -> gdk::RGBA {
        self.foreground
            .unwrap_or_else(|| gdk::RGBA::parse("#d0d0d0").expect("valid color"))
    }

    pub fn background(&self) -> gdk::RGBA {
        self.background
            .unwrap_or_else(|| gdk::RGBA::parse("#1e1e1e").expect("valid color"))
    }

    /// The 16 ANSI colors: the theme's, or xterm's when it has none.
    pub fn ansi_colors(&self) -> [gdk::RGBA; 16] {
        std::array::from_fn(|i| {
            self.palette
                .get(i)
                .copied()
                .unwrap_or_else(|| xterm_color(i))
        })
    }

    /// Replaces the colors (and opacity) with those of `other`, keeping the font.
    pub fn set_colors_from(&mut self, other: TerminalTheme) {
        self.foreground = other.foreground;
        self.background = other.background;
        self.palette = other.palette;
        self.opacity = other.opacity;
    }
}

fn config_dir() -> Option<PathBuf> {
    Some(crate::dirs::config_home())
}

fn data_dirs() -> Vec<PathBuf> {
    crate::platform::current().data_dirs()
}

/// The text of Ghostty's config files, in the order Ghostty reads them.
fn ghostty_config() -> String {
    crate::platform::current()
        .ghostty_config_dirs()
        .iter()
        .flat_map(|dir| ["config", "config.ghostty"].map(|name| dir.join(name)))
        .filter_map(|path| fs::read_to_string(path).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Ghostty: `key = value` lines, with an optional `theme` file in the same format.

fn ghostty() -> Option<TerminalTheme> {
    let pairs = key_values(&ghostty_config());

    let mut theme = TerminalTheme::default();
    let mut palette: [Option<gdk::RGBA>; 16] = Default::default();
    // The theme is the base; settings in the config itself override it.
    if let Some((_, name)) = pairs.iter().rev().find(|(k, _)| k == "theme")
        && let Some(text) = ghostty_theme_file(&ghostty_theme_name(name))
    {
        apply_ghostty(&key_values(&text), &mut theme, &mut palette);
    }
    apply_ghostty(&pairs, &mut theme, &mut palette);
    set_palette(&mut theme, palette);
    Some(theme)
}

fn ghostty_theme_dirs() -> Vec<PathBuf> {
    config_dir()
        .map(|d| d.join("ghostty/themes"))
        .into_iter()
        .chain(data_dirs().into_iter().map(|d| d.join("ghostty/themes")))
        .collect()
}

fn ghostty_theme_file(name: &str) -> Option<String> {
    ghostty_theme_dirs()
        .into_iter()
        .find_map(|dir| fs::read_to_string(dir.join(name)).ok())
}

/// Colors from a Ghostty-format theme (`background = …`, `palette = N=…`).
fn ghostty_colors(text: &str) -> TerminalTheme {
    let mut theme = TerminalTheme::default();
    let mut palette: [Option<gdk::RGBA>; 16] = Default::default();
    apply_ghostty(&key_values(text), &mut theme, &mut palette);
    set_palette(&mut theme, palette);
    theme.font = None;
    theme
}

/// Unset entries fall back to xterm's standard colors.
fn set_palette(theme: &mut TerminalTheme, palette: [Option<gdk::RGBA>; 16]) {
    if palette.iter().any(Option::is_some) {
        theme.palette = palette
            .into_iter()
            .enumerate()
            .map(|(i, color)| color.unwrap_or_else(|| xterm_color(i)))
            .collect();
    }
}

/// `light:A,dark:B` picks the dark variant.
fn ghostty_theme_name(value: &str) -> String {
    value
        .split(',')
        .find_map(|part| part.trim().strip_prefix("dark:"))
        .unwrap_or(value)
        .trim()
        .to_string()
}

fn apply_ghostty(
    pairs: &[(String, String)],
    theme: &mut TerminalTheme,
    palette: &mut [Option<gdk::RGBA>; 16],
) {
    let mut family = None;
    let mut size = None;
    for (key, value) in pairs {
        match key.as_str() {
            "foreground" => theme.foreground = parse_color(value),
            "background" => theme.background = parse_color(value),
            "background-opacity" => theme.opacity = value.parse().ok(),
            "palette" => {
                if let Some((index, color)) = value.split_once('=')
                    && let (Ok(i @ 0..16), Some(color)) =
                        (index.trim().parse::<usize>(), parse_color(color))
                {
                    palette[i] = Some(color);
                }
            }
            // Repeated `font-family` lines add fallbacks; the first is the main font.
            "font-family" if family.is_none() && !value.is_empty() => family = Some(value.clone()),
            "font-size" => size = value.parse::<f64>().ok(),
            _ => {}
        }
    }
    if family.is_some() || size.is_some() {
        let font = theme.font.get_or_insert_with(|| {
            // Ghostty's default size
            let family = crate::platform::current().monospace_family();
            pango::FontDescription::from_string(&format!("{family} 13"))
        });
        if let Some(family) = family {
            font.set_family(&family);
        }
        if let Some(size) = size {
            font.set_size((size * pango::SCALE as f64).round() as i32);
        }
    }
}

pub(crate) fn key_values(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        .collect()
}

fn parse_color(value: &str) -> Option<gdk::RGBA> {
    let value = value.trim();
    gdk::RGBA::parse(value)
        .or_else(|_| gdk::RGBA::parse(format!("#{value}")))
        .ok()
}

fn xterm_color(i: usize) -> gdk::RGBA {
    const XTERM: [&str; 16] = [
        "#000000", "#cd0000", "#00cd00", "#cdcd00", "#0000ee", "#cd00cd", "#00cdcd", "#e5e5e5",
        "#7f7f7f", "#ff0000", "#00ff00", "#ffff00", "#5c5cff", "#ff00ff", "#00ffff", "#ffffff",
    ];
    gdk::RGBA::parse(XTERM[i]).expect("valid color")
}

// ---------------------------------------------------------------------------
// Konsole: konsolerc names the default profile, which names a color scheme.
// All three are INI files.

/// The command the user's terminal starts (Ghostty's `command`, or the
/// Konsole default profile's `Command`), if set and runnable. Users often
/// set their shell there rather than as their login shell.
pub fn terminal_command() -> Option<String> {
    let ghostty = || {
        key_values(&ghostty_config())
            .into_iter()
            .filter(|(key, _)| key == "command")
            .map(|(_, value)| value)
            .next_back()
    };
    let konsole = || {
        let rc = fs::read_to_string(config_dir()?.join("konsolerc")).ok()?;
        let profile = konsole_file(&ini_get(&rc, "Desktop Entry", "DefaultProfile")?)?;
        ini_get(&profile, "General", "Command")
    };
    ghostty()
        .or_else(konsole)
        .filter(|command| command.split_whitespace().next().is_some_and(is_runnable))
}

/// Whether a program exists: a path to an executable, or a name on $PATH.
fn is_runnable(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let executable = |path: &std::path::Path| {
        fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if program.contains('/') {
        return executable(std::path::Path::new(program));
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| executable(&dir.join(program))))
}

fn konsole() -> Option<TerminalTheme> {
    let rc = fs::read_to_string(config_dir()?.join("konsolerc")).ok()?;
    let profile = konsole_file(&ini_get(&rc, "Desktop Entry", "DefaultProfile")?)?;
    let scheme_name =
        ini_get(&profile, "Appearance", "ColorScheme").unwrap_or_else(|| "Breeze".into());

    let mut theme = konsole_scheme(&scheme_name).unwrap_or_default();
    theme.font = ini_get(&profile, "Appearance", "Font").and_then(|f| qt_font(&f));
    Some(theme)
}

/// Colors of an installed Konsole color scheme, by file name without extension.
fn konsole_scheme(name: &str) -> Option<TerminalTheme> {
    let scheme = konsole_file(&format!("{name}.colorscheme"))?;
    let color = |section: &str| ini_get(&scheme, section, "Color").and_then(|c| konsole_color(&c));
    let normal = (0..8).map(|i| color(&format!("Color{i}")));
    let bright = (0..8).map(|i| color(&format!("Color{i}Intense")));
    Some(TerminalTheme {
        font: None,
        foreground: color("Foreground"),
        background: color("Background"),
        palette: normal
            .chain(bright)
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default(),
        opacity: ini_get(&scheme, "General", "Opacity").and_then(|o| o.parse().ok()),
    })
}

fn konsole_file(name: &str) -> Option<String> {
    data_dirs()
        .into_iter()
        .find_map(|dir| fs::read_to_string(dir.join("konsole").join(name)).ok())
}

fn ini_get(text: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in text.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_section = name == section;
        } else if in_section
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// `r,g,b` with 0..=255 components.
fn konsole_color(value: &str) -> Option<gdk::RGBA> {
    let mut parts = value.split(',').map(|p| p.trim().parse::<u8>().ok());
    let (r, g, b) = (parts.next()??, parts.next()??, parts.next()??);
    Some(gdk::RGBA::new(
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        1.0,
    ))
}

/// Parses a Qt font string: `family,pointSize,pixelSize,styleHint,weight,italic,...`.
fn qt_font(spec: &str) -> Option<pango::FontDescription> {
    let fields: Vec<&str> = spec.split(',').map(str::trim).collect();
    let family = fields.first().filter(|f| !f.is_empty())?;
    let mut font = pango::FontDescription::new();
    font.set_family(family);
    if let Some(size) = fields
        .get(1)
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|s| *s > 0.0)
    {
        font.set_size((size * pango::SCALE as f64).round() as i32);
    }
    if let Some(weight) = fields.get(4).and_then(|s| s.parse::<i32>().ok()) {
        // Qt 5 strings (10-11 fields) use weights 0..=99; Qt 6 uses CSS weights.
        let css = if fields.len() <= 11 {
            match weight {
                i32::MIN..=6 => 100,
                7..=18 => 200,
                19..=37 => 300,
                38..=53 => 400,
                54..=60 => 500,
                61..=69 => 600,
                70..=78 => 700,
                79..=84 => 800,
                85.. => 900,
            }
        } else {
            weight
        };
        font.set_weight(match css {
            i32::MIN..=150 => pango::Weight::Thin,
            151..=250 => pango::Weight::Ultralight,
            251..=350 => pango::Weight::Light,
            351..=450 => pango::Weight::Normal,
            451..=550 => pango::Weight::Medium,
            551..=650 => pango::Weight::Semibold,
            651..=750 => pango::Weight::Bold,
            751..=850 => pango::Weight::Ultrabold,
            851.. => pango::Weight::Heavy,
        });
    }
    if fields.get(5) == Some(&"1") {
        font.set_style(pango::Style::Italic);
    }
    Some(font)
}

// ---------------------------------------------------------------------------
// Named color schemes

/// A selectable color scheme. `id` is what the settings file stores:
/// `builtin:<name>`, `konsole:<scheme file>` or `ghostty:<theme file>`.
pub struct SchemeChoice {
    pub id: String,
    pub label: String,
}

/// Built-in schemes: name, background, foreground, 16 ANSI colors.
const BUILTIN: &[(&str, &str, &str, [&str; 16])] = &[
    (
        "Catppuccin Mocha",
        "#1e1e2e",
        "#cdd6f4",
        [
            "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
            "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
        ],
    ),
    (
        "Catppuccin Latte",
        "#eff1f5",
        "#4c4f69",
        [
            "#5c5f77", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb", "#179299", "#acb0be",
            "#6c6f85", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb", "#179299", "#bcc0cc",
        ],
    ),
    (
        "Dracula",
        "#282a36",
        "#f8f8f2",
        [
            "#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2",
            "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df", "#a4ffff", "#ffffff",
        ],
    ),
    (
        "GitHub Dark",
        "#0d1117",
        "#e6edf3",
        [
            "#484f58", "#ff7b72", "#3fb950", "#d29922", "#58a6ff", "#bc8cff", "#39c5cf", "#b1bac4",
            "#6e7681", "#ffa198", "#56d364", "#e3b341", "#79c0ff", "#d2a8ff", "#56d4dd", "#ffffff",
        ],
    ),
    (
        "GitHub Light",
        "#ffffff",
        "#1f2328",
        [
            "#24292f", "#cf222e", "#116329", "#4d2d00", "#0969da", "#8250df", "#1b7c83", "#6e7781",
            "#57606a", "#a40e26", "#1a7f37", "#633c01", "#218bff", "#a475f9", "#3192aa", "#8c959f",
        ],
    ),
    (
        "Gruvbox Dark",
        "#282828",
        "#ebdbb2",
        [
            "#282828", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286", "#689d6a", "#a89984",
            "#928374", "#fb4934", "#b8bb26", "#fabd2f", "#83a598", "#d3869b", "#8ec07c", "#ebdbb2",
        ],
    ),
    (
        "Nord",
        "#2e3440",
        "#d8dee9",
        [
            "#3b4252", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead", "#88c0d0", "#e5e9f0",
            "#4c566a", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead", "#8fbcbb", "#eceff4",
        ],
    ),
    (
        "One Dark",
        "#282c34",
        "#abb2bf",
        [
            "#282c34", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf",
            "#5c6370", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#ffffff",
        ],
    ),
    (
        "Solarized Dark",
        "#002b36",
        "#839496",
        [
            "#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198", "#eee8d5",
            "#002b36", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4", "#93a1a1", "#fdf6e3",
        ],
    ),
    (
        "Tokyo Night",
        "#1a1b26",
        "#c0caf5",
        [
            "#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6",
            "#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5",
        ],
    ),
];

/// Every scheme that can be picked: built-in ones, then installed Konsole
/// schemes and Ghostty themes.
pub fn available_schemes() -> Vec<SchemeChoice> {
    let mut choices: Vec<SchemeChoice> = BUILTIN
        .iter()
        .map(|(name, ..)| SchemeChoice {
            id: format!("builtin:{name}"),
            label: name.to_string(),
        })
        .collect();
    let mut installed =
        |dirs: Vec<PathBuf>, prefix: &str, source: &str, extension: Option<&str>| {
            let mut names: Vec<String> = dirs
                .iter()
                .filter_map(|dir| fs::read_dir(dir).ok())
                .flatten()
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    match extension {
                        Some(ext) if path.extension()? != ext => None,
                        Some(_) => Some(path.file_stem()?.to_string_lossy().into_owned()),
                        None => Some(path.file_name()?.to_string_lossy().into_owned()),
                    }
                })
                .collect();
            names.sort_by_key(|n| n.to_lowercase());
            names.dedup();
            choices.extend(names.into_iter().map(|name| SchemeChoice {
                id: format!("{prefix}:{name}"),
                label: format!("{name} ({source})"),
            }));
        };
    installed(
        data_dirs().into_iter().map(|d| d.join("konsole")).collect(),
        "konsole",
        "Konsole",
        Some("colorscheme"),
    );
    installed(ghostty_theme_dirs(), "ghostty", "Ghostty", None);
    choices
}

/// Colors of the scheme with the given id (see [`SchemeChoice`]).
pub fn named_scheme(id: &str) -> Option<TerminalTheme> {
    let (source, name) = id.split_once(':')?;
    match source {
        "builtin" => {
            let (_, bg, fg, palette) = BUILTIN.iter().find(|(n, ..)| *n == name)?;
            Some(TerminalTheme {
                foreground: parse_color(fg),
                background: parse_color(bg),
                palette: palette.iter().filter_map(|c| parse_color(c)).collect(),
                ..Default::default()
            })
        }
        "konsole" => konsole_scheme(name),
        "ghostty" => ghostty_theme_file(name).map(|text| ghostty_colors(&text)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qt6_font() {
        let font =
            qt_font("JetBrainsMono Nerd Font,12,-1,5,700,0,0,0,0,0,0,0,0,0,0,1,Bold").unwrap();
        assert_eq!(font.family().unwrap(), "JetBrainsMono Nerd Font");
        assert_eq!(font.size(), 12 * pango::SCALE);
        assert_eq!(font.weight(), pango::Weight::Bold);
    }

    #[test]
    fn parses_qt5_font() {
        let font = qt_font("Hack,10,-1,5,50,0,0,0,0,0").unwrap();
        assert_eq!(font.weight(), pango::Weight::Normal);
    }

    #[test]
    fn reads_ini_sections() {
        let text = "[Background]\nColor=30,30,46\n\n[Foreground]\nColor=205,214,244\n";
        assert_eq!(
            ini_get(text, "Foreground", "Color").as_deref(),
            Some("205,214,244")
        );
        let bg = konsole_color(&ini_get(text, "Background", "Color").unwrap()).unwrap();
        assert!((bg.red() - 30.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn builtin_schemes_are_complete() {
        for choice in available_schemes()
            .iter()
            .filter(|c| c.id.starts_with("builtin:"))
        {
            let theme = named_scheme(&choice.id).unwrap();
            assert_eq!(theme.palette.len(), 16, "{}", choice.label);
            assert!(theme.foreground.is_some() && theme.background.is_some());
        }
    }

    #[test]
    fn parses_ghostty_theme_text() {
        let theme =
            ghostty_colors("background = 1e1e2e\nforeground = #cdd6f4\npalette = 1=#f38ba8\n");
        assert_eq!(theme.background.unwrap().to_str(), "rgb(30,30,46)");
        assert_eq!(theme.palette.len(), 16);
        assert_eq!(theme.palette[1].to_str(), "rgb(243,139,168)");
    }

    #[test]
    fn checks_that_shells_exist() {
        assert!(is_runnable("sh"));
        assert!(is_runnable("/bin/sh"));
        assert!(!is_runnable("no-such-shell-anywhere"));
        assert!(!is_runnable("/etc/passwd"), "not executable");
    }

    #[test]
    #[ignore]
    fn print_terminal_command() {
        println!("terminal command: {:?}", terminal_command());
    }

    #[test]
    fn picks_dark_ghostty_theme() {
        assert_eq!(ghostty_theme_name("light:Latte,dark:Mocha"), "Mocha");
        assert_eq!(ghostty_theme_name("catppuccin-mocha"), "catppuccin-mocha");
    }
}
