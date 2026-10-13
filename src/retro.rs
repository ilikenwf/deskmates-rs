//! Reusable Win95/BonziBuddy-era visual theme for the forked Settings
//! window — a port of bonzibuddy-rs `src/retro.rs`.
//!
//! Like bonzibuddy-rs, this repo has no egui: the settings window is a
//! hand-drawn winit + pixels UI painting into a raw RGBA byte buffer via
//! the crate-root [`crate::fill_rect`] / [`crate::draw_text`] helpers.
//! [`apply_retro_theme`] is the reusable entry point: it paints the window
//! base (button-face gray) and hands back the [`Theme`] (display scale +
//! MS-Sans-Serif-style font size) that every widget primitive below
//! consumes.
//!
//! The palette and bevel geometry follow the classic Win95 `DrawEdge`
//! styles: raised (button face) and sunken (client field) 2px bevels,
//! etched group boxes, chunky 13px checkboxes and a sunken status bar.
//! Panel/window fill = FACE (#C0C0C0), edit-field fill = WHITE
//! (COLOR_WINDOW), selection accent = classic navy, 11-12px "MS Sans
//! Serif" (Liberation Sans is the metric stand-in). The original
//! BonziBuddy options dialog was a VB6 property sheet on a Win95/98
//! desktop, so this flat-gray bevel theme is period-correct.
//!
//! This theme is applied ONLY to the Settings window
//! ([`crate::ask_ui`] in settings mode); the pet window and the Ask dialog
//! keep their own look.

use crate::{draw_text, fill_rect, line_height, text_width};
use ab_glyph::{FontArc, PxScale};

pub type Rgb = [u8; 4];

/// Classic Win95 system colors (COLOR_BTNFACE family).
pub const FACE: Rgb = [0xC0, 0xC0, 0xC0, 255];
pub const HILIGHT: Rgb = [0xFF, 0xFF, 0xFF, 255];
pub const LIGHT: Rgb = [0xDF, 0xDF, 0xDF, 255];
pub const SHADOW: Rgb = [0x80, 0x80, 0x80, 255];
pub const DARK: Rgb = [0x00, 0x00, 0x00, 255];
pub const WHITE: Rgb = [0xFF, 0xFF, 0xFF, 255];
/// COLOR_WINDOW: edit-field background.
pub const WINDOW: Rgb = [0xFF, 0xFF, 0xFF, 255];
pub const TEXT: Rgb = [0x00, 0x00, 0x00, 255];
/// Disabled label: gray glyph with a white emboss one pixel down-right.
pub const DISABLED: Rgb = [0x80, 0x80, 0x80, 255];
/// COLOR_HIGHLIGHT: classic navy selection accent.
pub const SELECTION: Rgb = [0x00, 0x00, 0x80, 255];

/// Per-window theme state: display scale + MS-Sans-Serif-style font size
/// (8 pt ≈ 11 px at 96 dpi; Liberation Sans is the metric stand-in).
pub struct Theme {
    pub s: f32,
    pub font: f32,
}

impl Theme {
    pub fn scale(&self) -> PxScale {
        let px = self.font.max(10.0);
        PxScale { x: px, y: px }
    }
    /// Line height at the theme font size, for layout math.
    pub fn lh(&self, font: &FontArc) -> f32 {
        line_height(font, self.scale())
    }
    /// Chunky control metric (checkbox box size).
    pub fn box_px(&self) -> i32 {
        (13.0 * self.s).round() as i32
    }
}

/// Paint the window base in button-face gray and return the theme to draw
/// the widgets with. The reusable "install the retro look" entry point.
pub fn apply_retro_theme(c: &mut [u8], cw: u32, ch: u32, s: f32) -> Theme {
    fill_rect(c, cw, ch, 0, 0, cw as i32, ch as i32, FACE);
    Theme { s, font: 12.0 * s }
}

// ---------------------------------------------------------------------------
// low-level geometry
// ---------------------------------------------------------------------------

fn blend(c: &mut [u8], cw: u32, ch: u32, x: i32, y: i32, col: Rgb) {
    fill_rect(c, cw, ch, x, y, 1, 1, col);
}

fn edge(c: &mut [u8], cw: u32, ch: u32, x: i32, y: i32, w: i32, h: i32, tl: Rgb, br: Rgb) {
    fill_rect(c, cw, ch, x, y, w, 1, tl);
    fill_rect(c, cw, ch, x, y + h - 1, w, 1, br);
    fill_rect(c, cw, ch, x, y, 1, h, tl);
    fill_rect(c, cw, ch, x + w - 1, y, 1, h, br);
}

/// Raised 2px bevel (button face): white/light top-left, black/shadow
/// bottom-right, face fill.
pub fn raised(c: &mut [u8], cw: u32, ch: u32, x: i32, y: i32, w: i32, h: i32) {
    fill_rect(c, cw, ch, x, y, w, h, FACE);
    edge(c, cw, ch, x, y, w, h, HILIGHT, DARK);
    edge(c, cw, ch, x + 1, y + 1, w - 2, h - 2, LIGHT, SHADOW);
}

/// Sunken 2px bevel (client edge): shadow/black top-left, white/light
/// bottom-right.
pub fn sunken(c: &mut [u8], cw: u32, ch: u32, x: i32, y: i32, w: i32, h: i32, bg: Rgb) {
    fill_rect(c, cw, ch, x, y, w, h, bg);
    edge(c, cw, ch, x, y, w, h, SHADOW, WHITE);
    edge(c, cw, ch, x + 1, y + 1, w - 2, h - 2, DARK, LIGHT);
}

// ---------------------------------------------------------------------------
// text
// ---------------------------------------------------------------------------

pub fn text(c: &mut [u8], cw: u32, ch: u32, font: &FontArc, t: &Theme, txt: &str, x: f32, y: f32, color: Rgb) {
    draw_text(c, cw, ch, font, txt, x, y, t.scale(), color);
}

pub fn text_w(font: &FontArc, t: &Theme, txt: &str) -> f32 {
    text_width(font, txt, t.scale())
}

/// Draw text; when `disabled`, use the Win95 embossed-gray treatment.
pub fn label(c: &mut [u8], cw: u32, ch: u32, font: &FontArc, t: &Theme, txt: &str, x: f32, y: f32, disabled: bool) {
    if disabled {
        text(c, cw, ch, font, t, txt, x + t.s, y + t.s, WHITE);
        text(c, cw, ch, font, t, txt, x, y, DISABLED);
    } else {
        text(c, cw, ch, font, t, txt, x, y, TEXT);
    }
}

// ---------------------------------------------------------------------------
// widgets
// ---------------------------------------------------------------------------

/// Push button. `default` adds the 1px black default-frame; `pressed`
/// inverts the bevel and shifts the caption like a held Win95 button.
/// Returns the button rect (the face, not the default frame).
pub fn button(
    c: &mut [u8],
    cw: u32,
    ch: u32,
    font: &FontArc,
    t: &Theme,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    caption: &str,
    default: bool,
    pressed: bool,
    disabled: bool,
) -> (i32, i32, i32, i32) {
    if default && !pressed {
        // Black frame one pixel outside the button, face gap between.
        edge(c, cw, ch, x - 2, y - 2, w + 4, h + 4, DARK, DARK);
        fill_rect(c, cw, ch, x - 1, y - 1, (w + 2).max(0), (h + 2).max(0), FACE);
    }
    if pressed {
        sunken(c, cw, ch, x, y, w, h, FACE);
    } else {
        raised(c, cw, ch, x, y, w, h);
    }
    let off = if pressed { t.s } else { 0.0 };
    let tw = text_w(font, t, caption);
    let lh = line_height(font, t.scale());
    label(
        c,
        cw,
        ch,
        font,
        t,
        caption,
        x as f32 + (w as f32 - tw) / 2.0 + off,
        y as f32 + (h as f32 - lh) / 2.0 + lh + off,
        disabled,
    );
    (x, y, w, h)
}

/// Etched group box with a caption punched out of the top line.
pub fn group(
    c: &mut [u8],
    cw: u32,
    ch: u32,
    font: &FontArc,
    t: &Theme,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    caption: &str,
    disabled: bool,
) {
    edge(c, cw, ch, x, y, w, h, SHADOW, WHITE);
    edge(c, cw, ch, x + 1, y + 1, w - 2, h - 2, WHITE, SHADOW);
    if !caption.is_empty() {
        let tw = text_w(font, t, caption);
        fill_rect(c, cw, ch, x + 6, y - (t.font * 0.45) as i32, (tw + 6.0) as i32, (t.font * 1.1) as i32, FACE);
        label(c, cw, ch, font, t, caption, x as f32 + 9.0, y as f32 + t.font * 0.35, disabled);
    }
}

/// Sunken edit field (COLOR_WINDOW background, or face gray when disabled).
pub fn field(c: &mut [u8], cw: u32, ch: u32, x: i32, y: i32, w: i32, h: i32, disabled: bool) {
    sunken(c, cw, ch, x, y, w, h, if disabled { FACE } else { WINDOW });
}

/// Chunky 13px checkbox with a black check mark.
pub fn checkbox(
    c: &mut [u8],
    cw: u32,
    ch: u32,
    font: &FontArc,
    t: &Theme,
    x: i32,
    y: i32,
    caption: &str,
    checked: bool,
    disabled: bool,
) {
    let b = t.box_px();
    sunken(c, cw, ch, x, y, b, b, if disabled { FACE } else { WINDOW });
    if checked {
        let u = b as f32 / 13.0;
        let pts: [(f32, f32); 3] = [(3.0, 6.5), (5.5, 9.5), (10.0, 3.0)];
        for seg in pts.windows(2) {
            let (x0, y0) = (x as f32 + seg[0].0 * u, y as f32 + seg[0].1 * u);
            let (x1, y1) = (x as f32 + seg[1].0 * u, y as f32 + seg[1].1 * u);
            let steps = ((x1 - x0).abs() + (y1 - y0).abs()).max(1.0);
            for i in 0..=steps as i32 {
                let f = i as f32 / steps;
                let (px, py) = (x0 + (x1 - x0) * f, y0 + (y1 - y0) * f);
                let col = if disabled { DISABLED } else { DARK };
                blend(c, cw, ch, px as i32, py as i32, col);
                blend(c, cw, ch, px as i32 + 1, py as i32, col);
            }
        }
    }
    label(c, cw, ch, font, t, caption, (x + b + 5) as f32, y as f32 + t.font * 0.95, disabled);
}

/// Sunken status bar strip with left-aligned text.
pub fn statusbar(
    c: &mut [u8],
    cw: u32,
    ch: u32,
    font: &FontArc,
    t: &Theme,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    txt: &str,
) {
    sunken(c, cw, ch, x, y, w, h, FACE);
    let lh = line_height(font, t.scale());
    text(c, cw, ch, font, t, txt, (x + 6) as f32, y as f32 + (h as f32 - lh) / 2.0 + lh, TEXT);
}
