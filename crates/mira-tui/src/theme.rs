//! The TUI palette and state glyphs. Colors come from the product illustrations (terracotta
//! roofs, sky, leaves, warm stone). The tones depend on the terminal background (see
//! [`Background`]). When the background is unknown, a middle set that passes as marks on
//! light and dark alike is used, and colored words fall back to the default foreground.
//! Truecolor terminals get the exact tones, other terminals the nearest 256-color or
//! 16-color entry, and `NO_COLOR` gets none. Secondary text uses the DIM modifier. State
//! is never shown by color alone: see [`Mark`].

use ratatui::style::{Color, Modifier, Style};

/// How many colors the terminal can show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorMode {
    None,
    Basic,
    Indexed,
    TrueColor,
}

impl ColorMode {
    /// Reads `NO_COLOR`, `COLORTERM`, and `TERM` once at start.
    pub fn detect() -> Self {
        let get = |k: &str| std::env::var(k).unwrap_or_default().to_lowercase();
        if std::env::var_os("NO_COLOR").is_some() {
            return Self::None;
        }
        let colorterm = get("COLORTERM");
        if colorterm == "truecolor" || colorterm == "24bit" {
            return Self::TrueColor;
        }
        let term = get("TERM");
        if term.contains("256") || term.contains("direct") {
            Self::Indexed
        } else {
            Self::Basic
        }
    }

    pub fn enabled(self) -> bool {
        self != Self::None
    }
}

/// The terminal background, as far as Mira can tell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Background {
    Light,
    Dark,
    Unknown,
}

impl Background {
    /// `MIRA_THEME=light|dark`; any other value means "detect".
    pub fn from_override(v: Option<&str>) -> Option<Self> {
        match v?.trim().to_ascii_lowercase().as_str() {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    /// Light when black text has more contrast on the color than white text has.
    pub fn from_rgb(rgb: (u8, u8, u8)) -> Self {
        let l = luminance(rgb);
        if contrast_l(l, 0.0) > contrast_l(l, 1.0) {
            Self::Light
        } else {
            Self::Dark
        }
    }

    /// `COLORFGBG` (`fg;bg` or `fg;other;bg`): the last field is an ANSI color index.
    pub fn from_colorfgbg(v: Option<&str>) -> Option<Self> {
        let bg: u8 = v?.rsplit(';').next()?.trim().parse().ok()?;
        match bg {
            7 | 9..=15 => Some(Self::Light),
            0..=6 | 8 => Some(Self::Dark),
            _ => None,
        }
    }
}

/// Parses an OSC 11 reply (`ESC ] 11 ; rgb:RRRR/GGGG/BBBB` ended by BEL or ST) found
/// anywhere in `buf`. Each channel has 1 to 4 hex digits.
pub fn parse_osc11(buf: &[u8]) -> Option<(u8, u8, u8)> {
    let start = buf.windows(5).position(|w| w == b"\x1b]11;")? + 5;
    let rest = &buf[start..];
    let end = rest.iter().position(|&b| b == 0x07 || b == 0x1b)?;
    let body = std::str::from_utf8(&rest[..end]).ok()?;
    let spec = body
        .strip_prefix("rgb:")
        .or_else(|| body.strip_prefix("rgba:"))?;
    let mut parts = spec.split('/').map(|p| {
        let n = p.len();
        if !(1..=4).contains(&n) {
            return None;
        }
        let v = u32::from_str_radix(p, 16).ok()?;
        let max = (1u32 << (4 * n)) - 1;
        u8::try_from((v * 255 + max / 2) / max).ok()
    });
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// WCAG 2.2 relative luminance of an sRGB color.
pub fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
    let lin = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

fn contrast_l(a: f64, b: f64) -> f64 {
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

/// A named tone of the palette.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    /// Terracotta: brand, selection, key chips, the active tab.
    Accent,
    /// A deeper terracotta for group labels.
    AccentDeep,
    /// Sky: branch, info, links.
    Sky,
    /// Leaf: running, succeeded.
    Leaf,
    /// Amber: starting, stopping, stale, background.
    Amber,
    /// Rose: failed, errors.
    Rose,
    /// Dark text on accent backgrounds.
    Ink,
}

impl Tone {
    fn rgb(self, bg: Background) -> (u8, u8, u8) {
        match (self, bg) {
            // Marks that pass 3:1 on light and dark backgrounds alike.
            (Tone::Accent, Background::Unknown) => (0xEC, 0x4A, 0x10),
            (Tone::Leaf, Background::Unknown) => (0x4A, 0x93, 0x55),
            (Tone::Amber, Background::Unknown) => (0xAE, 0x7A, 0x1F),
            (Tone::Accent, _) => (0xF2, 0x6B, 0x3A),
            (Tone::AccentDeep, _) => (0xC8, 0x5A, 0x30),
            (Tone::Sky, _) => (0x3B, 0x82, 0xC4),
            (Tone::Leaf, _) => (0x4F, 0x9D, 0x5B),
            (Tone::Amber, _) => (0xD9, 0x9A, 0x2B),
            (Tone::Rose, _) => (0xD9, 0x53, 0x4F),
            (Tone::Ink, _) => (0x1C, 0x1C, 0x1C),
        }
    }

    fn basic(self) -> Color {
        match self {
            Tone::Accent | Tone::AccentDeep => Color::LightRed,
            Tone::Sky => Color::Blue,
            Tone::Leaf => Color::Green,
            Tone::Amber => Color::Yellow,
            Tone::Rose => Color::Red,
            Tone::Ink => Color::Black,
        }
    }
}

/// The nearest xterm 256-color index (the 6x6x6 cube or the gray ramp) to an RGB color.
pub fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let dist = |a: (u8, u8, u8)| {
        let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2);
        d(a.0, r) + d(a.1, g) + d(a.2, b)
    };
    let step = |v: u8| {
        (0..6)
            .min_by_key(|&i| (i32::from(STEPS[i]) - i32::from(v)).abs())
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (step(r), step(g), step(b));
    let cube = (STEPS[ri], STEPS[gi], STEPS[bi]);
    let cube_index = 16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8;
    let avg = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    let gray_i = (avg.saturating_sub(3) / 10).min(23) as u8;
    let gray_v = 8 + 10 * gray_i;
    if dist((gray_v, gray_v, gray_v)) < dist(cube) {
        232 + gray_i
    } else {
        cube_index
    }
}

pub struct Theme {
    pub mode: ColorMode,
    pub bg: Background,
}

impl Theme {
    pub fn new(mode: ColorMode, bg: Background) -> Self {
        Self { mode, bg }
    }

    pub fn color(&self, tone: Tone) -> Option<Color> {
        let (r, g, b) = tone.rgb(self.bg);
        match self.mode {
            ColorMode::None => None,
            ColorMode::Basic => Some(tone.basic()),
            ColorMode::Indexed => Some(Color::Indexed(nearest_256(r, g, b))),
            ColorMode::TrueColor => Some(Color::Rgb(r, g, b)),
        }
    }

    pub fn fg(&self, tone: Tone) -> Style {
        self.color(tone)
            .map_or(Style::default(), |c| Style::default().fg(c))
    }

    /// Words in `tone`. Without a known background the middle tones are too weak for
    /// text, so words use the default foreground.
    pub fn word(&self, tone: Tone) -> Style {
        match (self.mode, self.bg) {
            (ColorMode::Indexed | ColorMode::TrueColor, Background::Unknown) => Style::default(),
            _ => self.fg(tone),
        }
    }

    pub fn bold(&self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    pub fn dim(&self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    /// A filled chip: `tone` background with dark text. Without color: reversed.
    pub fn chip(&self, tone: Tone) -> Style {
        match (self.color(tone), self.color(Tone::Ink)) {
            (Some(bg), Some(ink)) => Style::default().bg(bg).fg(ink).add_modifier(Modifier::BOLD),
            _ => Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
        }
    }

    /// The selected row of a focused list.
    pub fn selected(&self) -> Style {
        self.chip(Tone::Accent)
    }

    /// Panel borders: accent when the panel has the focus, dim otherwise.
    pub fn border(&self, focus: bool) -> Style {
        if focus && self.mode.enabled() {
            self.fg(Tone::Accent)
        } else if focus {
            self.bold()
        } else {
            self.dim()
        }
    }
}

/// A state as a glyph, a word, and a tone; the glyph and word carry it without color.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mark {
    pub glyph: &'static str,
    pub word: &'static str,
    /// `None` draws the mark dim.
    pub tone: Option<Tone>,
}

impl Mark {
    pub const fn new(glyph: &'static str, word: &'static str, tone: Option<Tone>) -> Self {
        Self { glyph, word, tone }
    }

    /// The glyph's style.
    pub fn style(&self, t: &Theme) -> Style {
        self.tone.map_or(t.dim(), |c| t.fg(c))
    }

    /// The word's style.
    pub fn word_style(&self, t: &Theme) -> Style {
        self.tone.map_or(t.dim(), |c| t.word(c))
    }
}

pub const RUNNING: Mark = Mark::new("●", "running", Some(Tone::Leaf));
pub const OK: Mark = Mark::new("✓", "ok", Some(Tone::Leaf));
pub const FAILED: Mark = Mark::new("✗", "failed", Some(Tone::Rose));
pub const STARTING: Mark = Mark::new("◐", "starting", Some(Tone::Amber));
pub const STOPPING: Mark = Mark::new("◐", "stopping", Some(Tone::Amber));
pub const NOT_RUN: Mark = Mark::new("○", "not run yet", None);
pub const DISABLED: Mark = Mark::new("⏸", "disabled", None);
pub const STOPPED: Mark = Mark::new("■", "stopped", None);

#[cfg(test)]
mod tests {
    use super::nearest_256;

    #[test]
    fn nearest_256_picks_the_cube_or_the_gray_ramp() {
        assert_eq!(nearest_256(255, 0, 0), 196);
        assert_eq!(nearest_256(0, 0, 0), 16);
        // Terracotta maps to an orange cube entry, not a gray.
        assert_eq!(nearest_256(0xF2, 0x6B, 0x3A), 203);
        // Near-black ink maps to the gray ramp.
        assert_eq!(nearest_256(0x1C, 0x1C, 0x1C), 234);
    }
}
