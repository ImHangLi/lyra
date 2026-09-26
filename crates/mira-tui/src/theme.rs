//! The TUI palette and state glyphs. Colors come from the product illustrations (terracotta
//! roofs, sky, leaves, warm stone). No single tone reads well as text on both light and
//! dark terminals, so each tone has a light and a dark variant, chosen from the terminal
//! background (see [`Background`]). When the background is unknown, a middle set that
//! passes as marks on both is used, and colored words fall back to the default foreground.
//! Truecolor terminals get the exact tones, 256-color terminals a checked table entry,
//! 16-color terminals a named color, and `NO_COLOR` gets none. Secondary text uses the DIM
//! modifier. State is never shown by color alone: see [`Mark`].

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
        let v = u32::from_str_radix(p, 16).ok()?;
        if !(1..=4).contains(&n) {
            return None;
        }
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

/// WCAG 2.2 contrast ratio of two colors, from 1 to 21.
#[cfg(test)]
pub fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    contrast_l(luminance(a), luminance(b))
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
    /// Dark text on chip backgrounds.
    Ink,
}

impl Tone {
    pub const ALL: [Tone; 7] = [
        Tone::Accent,
        Tone::AccentDeep,
        Tone::Sky,
        Tone::Leaf,
        Tone::Amber,
        Tone::Rose,
        Tone::Ink,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// The truecolor value on `bg`.
    pub fn rgb(self, bg: Background) -> (u8, u8, u8) {
        use Background::*;
        match (self, bg) {
            (Tone::Ink, _) => (0x1C, 0x1C, 0x1C),
            (Tone::Accent, Light) => (0xBA, 0x3B, 0x0C),
            (Tone::AccentDeep, Light) => (0xA8, 0x4C, 0x28),
            (Tone::Sky, Light) => (0x30, 0x6A, 0xA0),
            (Tone::Leaf, Light) => (0x3A, 0x73, 0x43),
            (Tone::Amber, Light) => (0x88, 0x60, 0x18),
            (Tone::Rose, Light) => (0xC2, 0x2E, 0x2A),
            (Tone::Accent, Dark) => (0xF2, 0x6B, 0x3A),
            (Tone::AccentDeep, Dark) => (0xD7, 0x7A, 0x56),
            (Tone::Sky, Dark) => (0x5C, 0x97, 0xCE),
            (Tone::Leaf, Dark) => (0x52, 0xA3, 0x5E),
            (Tone::Amber, Dark) => (0xD9, 0x9A, 0x2B),
            (Tone::Rose, Dark) => (0xE0, 0x71, 0x6D),
            // Marks that pass 3:1 on light and dark backgrounds alike.
            (Tone::Accent, Unknown) => (0xEC, 0x4A, 0x10),
            (Tone::AccentDeep, Unknown) => (0xC8, 0x5A, 0x30),
            (Tone::Sky, Unknown) => (0x3B, 0x82, 0xC4),
            (Tone::Leaf, Unknown) => (0x4A, 0x93, 0x55),
            (Tone::Amber, Unknown) => (0xAE, 0x7A, 0x1F),
            (Tone::Rose, Unknown) => (0xD9, 0x53, 0x4F),
        }
    }

    /// The xterm 256-color entry on `bg`: the nearest entry of the 6x6x6 cube or the gray
    /// ramp that keeps the contrast of the truecolor value (the tests check each one).
    pub fn indexed(self, bg: Background) -> u8 {
        use Background::*;
        match (self, bg) {
            (Tone::Ink, _) => 234,
            (Tone::Accent | Tone::AccentDeep, Light) => 94,
            (Tone::Sky, Light) => 25,
            (Tone::Leaf, Light) => 22,
            (Tone::Amber, Light) => 58,
            (Tone::Rose, Light) => 124,
            (Tone::Accent, Dark) => 209,
            (Tone::AccentDeep, Dark) => 173,
            (Tone::Sky, Dark) => 68,
            (Tone::Leaf, Dark) => 71,
            (Tone::Amber, Dark) => 172,
            (Tone::Rose, Dark) => 167,
            (Tone::Accent | Tone::AccentDeep, Unknown) => 166,
            (Tone::Sky, Unknown) => 67,
            (Tone::Leaf, Unknown) => 65,
            (Tone::Amber, Unknown) => 130,
            (Tone::Rose, Unknown) => 131,
        }
    }

    /// The 16-color entry.
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

    /// A chip background. Chips carry their own contrast (dark ink on a mid tone), so they
    /// are the same on every background.
    pub fn chip_rgb(self) -> (u8, u8, u8) {
        match self {
            Tone::Sky => (0x5B, 0x9B, 0xD5),
            t => t.rgb(Background::Dark),
        }
    }

    pub fn chip_indexed(self) -> u8 {
        match self {
            Tone::Sky => 68,
            t => t.indexed(Background::Dark),
        }
    }
}

/// The RGB value of an xterm 256-color index from 16 up.
#[cfg(test)]
pub fn index_rgb(i: u8) -> (u8, u8, u8) {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    if i >= 232 {
        let v = 8 + 10 * (i - 232);
        return (v, v, v);
    }
    let i = usize::from(i.saturating_sub(16));
    (STEPS[i / 36], STEPS[(i / 6) % 6], STEPS[i % 6])
}

pub struct Theme {
    pub mode: ColorMode,
    pub bg: Background,
    fg: [Option<Color>; 7],
    chip_bg: [Option<Color>; 7],
}

impl Theme {
    pub fn new(mode: ColorMode, bg: Background) -> Self {
        let pick = |tone: Tone, chip: bool| -> Option<Color> {
            match mode {
                ColorMode::None => None,
                ColorMode::Basic => Some(tone.basic()),
                ColorMode::Indexed if chip => Some(Color::Indexed(tone.chip_indexed())),
                ColorMode::Indexed => Some(Color::Indexed(tone.indexed(bg))),
                ColorMode::TrueColor if chip => {
                    let (r, g, b) = tone.chip_rgb();
                    Some(Color::Rgb(r, g, b))
                }
                ColorMode::TrueColor => {
                    let (r, g, b) = tone.rgb(bg);
                    Some(Color::Rgb(r, g, b))
                }
            }
        };
        Self {
            mode,
            bg,
            fg: Tone::ALL.map(|t| pick(t, false)),
            chip_bg: Tone::ALL.map(|t| pick(t, true)),
        }
    }

    pub fn color(&self, tone: Tone) -> Option<Color> {
        self.fg[tone.index()]
    }

    /// A mark, a border, or a cursor in `tone`.
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
        match (self.chip_bg[tone.index()], self.color(Tone::Ink)) {
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
    use super::{Background, Tone, contrast, index_rgb};

    const DARK_BGS: [(u8, u8, u8); 2] = [(0x1E, 0x1E, 0x1E), (0, 0, 0)];
    const LIGHT_BGS: [(u8, u8, u8); 2] = [(0xFF, 0xFF, 0xFF), (0xE6, 0xE6, 0xE6)];
    const TEXT: f64 = 4.5;
    const MARK: f64 = 3.0;
    const COLORED: [Tone; 6] = [
        Tone::Accent,
        Tone::AccentDeep,
        Tone::Sky,
        Tone::Leaf,
        Tone::Amber,
        Tone::Rose,
    ];

    fn check(tone: Tone, rgb: (u8, u8, u8), bgs: &[(u8, u8, u8)], min: f64, what: &str) {
        for bg in bgs {
            let r = contrast(rgb, *bg);
            assert!(
                r >= min,
                "{what} {tone:?} {rgb:02X?} on {bg:02X?}: {r:.2} < {min}"
            );
        }
    }

    #[test]
    fn the_contrast_formula_matches_wcag() {
        assert!((contrast((0, 0, 0), (0xFF, 0xFF, 0xFF)) - 21.0).abs() < 1e-9);
        assert!((contrast((0x77, 0x77, 0x77), (0xFF, 0xFF, 0xFF)) - 4.48).abs() < 0.01);
    }

    #[test]
    fn every_tone_passes_as_text_on_its_background() {
        for tone in COLORED {
            for (bg, bgs) in [(Background::Light, LIGHT_BGS), (Background::Dark, DARK_BGS)] {
                check(tone, tone.rgb(bg), &bgs, TEXT, "truecolor text");
                let idx = index_rgb(tone.indexed(bg));
                check(tone, idx, &bgs, TEXT, "256-color text");
            }
        }
    }

    #[test]
    fn the_unknown_background_set_passes_as_marks_on_both() {
        let all = [LIGHT_BGS, DARK_BGS].concat();
        for tone in COLORED {
            check(
                tone,
                tone.rgb(Background::Unknown),
                &all,
                MARK,
                "truecolor mark",
            );
            let idx = index_rgb(tone.indexed(Background::Unknown));
            check(tone, idx, &all, MARK, "256-color mark");
        }
    }

    #[test]
    fn chip_text_passes_on_every_chip() {
        let ink = Tone::Ink.rgb(Background::Dark);
        for tone in [Tone::Accent, Tone::Sky] {
            let r = contrast(tone.chip_rgb(), ink);
            assert!(r >= TEXT, "chip {tone:?}: {r:.2}");
            let r = contrast(index_rgb(tone.chip_indexed()), index_rgb(234));
            assert!(r >= TEXT, "256-color chip {tone:?}: {r:.2}");
        }
        let sky = contrast(Tone::Sky.chip_rgb(), ink);
        assert!((sky - 5.76).abs() < 0.01, "{sky:.2}");
    }

    #[test]
    fn index_rgb_reads_the_cube_and_the_gray_ramp() {
        assert_eq!(index_rgb(16), (0, 0, 0));
        assert_eq!(index_rgb(196), (0xFF, 0, 0));
        assert_eq!(index_rgb(234), (0x1C, 0x1C, 0x1C));
        assert_eq!(index_rgb(209), (0xFF, 0x87, 0x5F));
    }
}
