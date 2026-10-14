//! Forked Ask / Settings window: a normal, WM-managed winit window with real
//! keyboard focus. The pet itself is override-redirect (unmanaged), so the WM
//! never gives it keyboard focus — this child process window is managed, gets
//! focus normally, and implements full text editing (caret, arrows, Home/End,
//! Delete, Backspace).
//!
//! Run as: `deskmates-rs --ask-ui` or `deskmates-rs --settings-ui`.
//! Ask writes the question to the ask file; Settings writes the config file.
//! The pet notices either via mtime polling and reloads.

use crate::assets_root;
use crate::retro;
use crate::{draw_text, fill_rect, line_height, stroke_rect};
use ab_glyph::FontArc;
use ab_glyph::PxScale;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Event, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes};

// ---------------------------------------------------------------------------
// System clipboard for the text fields. Three backends, tried in order,
// each optional — the dialog must NEVER crash because the clipboard is
// broken:
//
// 1. arboard — kept alive in a process-wide slot so we remain the X11
//    CLIPBOARD owner after set_text (a dropped Clipboard stops serving
//    selection requests). Built `default-features = false`, so its
//    Linux backend is X11-only: on Wayland Clipboard::new() fails and
//    we fall through. DESKMATES_CLIP_NO_ARBOARD=1 skips it (tests).
// 2. wl-copy / wl-paste — the reliable Wayland route; wl-copy forks and
//    owns the selection after we exit.
// 3. xclip — X11 fallback; `xclip -i` likewise forks and keeps serving.
//
// Every copy also lands in an internal buffer, so Ctrl+V always works
// inside the dialog even when all three system routes are missing.
// ---------------------------------------------------------------------------

/// Internal last-resort buffer (also the source for Ctrl+V when the
/// system clipboard reads empty).
static CLIP_INTERNAL: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Live arboard handle (lazily — construction touches the X server,
/// never at load time).
static CLIP_ARBOARD: std::sync::LazyLock<std::sync::Mutex<Option<arboard::Clipboard>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// Spawn `cmd` with `text` on stdin; true if it ran successfully.
fn clip_pipe_in(cmd: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut si) = child.stdin.take() {
        let _ = std::io::Write::write_all(&mut si, text.as_bytes());
    }
    // wl-copy (default mode) and xclip -i both fork to keep serving the
    // selection after their parent exits, so wait() returns promptly.
    child.wait().ok().is_some_and(|s| s.success())
}

fn clip_pipe_out(cmd: &str, args: &[&str]) -> Option<String> {
    std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Copy `text` to the system clipboard. True when a *system* backend
/// took it (the internal buffer is updated either way).
fn clip_set(text: &str) -> bool {
    if let Ok(mut g) = CLIP_INTERNAL.lock() {
        *g = text.to_string();
    }
    if std::env::var_os("DESKMATES_CLIP_NO_ARBOARD").is_none() {
        if let Ok(mut guard) = CLIP_ARBOARD.lock() {
            if guard.is_none() {
                *guard = arboard::Clipboard::new().ok();
            }
            if let Some(cb) = guard.as_mut() {
                if cb.set_text(text.to_string()).is_ok() {
                    return true;
                }
            }
        }
    }
    if clip_pipe_in("wl-copy", &[], text) {
        return true;
    }
    clip_pipe_in("xclip", &["-selection", "clipboard", "-i"], text)
}

/// Read the system clipboard: arboard -> wl-paste -> xclip -> internal.
fn clip_get() -> String {
    if std::env::var_os("DESKMATES_CLIP_NO_ARBOARD").is_none() {
        if let Ok(mut guard) = CLIP_ARBOARD.lock() {
            if guard.is_none() {
                *guard = arboard::Clipboard::new().ok();
            }
            if let Some(cb) = guard.as_mut() {
                if let Ok(t) = cb.get_text() {
                    if !t.is_empty() {
                        return t;
                    }
                }
            }
        }
    }
    if let Some(t) = clip_pipe_out("wl-paste", &["--no-newline"]) {
        if !t.is_empty() {
            return t;
        }
    }
    if let Some(t) = clip_pipe_out("xclip", &["-selection", "clipboard", "-o"]) {
        if !t.is_empty() {
            return t;
        }
    }
    CLIP_INTERNAL.lock().map(|g| g.clone()).unwrap_or_default()
}

const W: u32 = 460;
const H: u32 = 330;
const FIELDS: [&str; 3] = ["API endpoint (blank = offline)", "API key", "Model"];

/// Startup parent pid. The forked dialog is a direct child of the pet
/// process; when the pet dies (Ctrl+C, menu Quit, kill -9) init/systemd
/// reaps us and our parent pid changes — the signal to self-exit rather
/// than linger as an orphan window.
#[cfg(unix)]
fn startup_parent() -> u32 {
    unsafe { libc::getppid() as u32 }
}

#[cfg(not(unix))]
fn startup_parent() -> u32 {
    0
}

/// True once the pet that forked us has died (we were reparented).
/// Windows limitation: detecting that without polling needs a process
/// handle wait (OpenProcess + WaitForSingleObject), and windows-sys is
/// only a transitive dependency here with the wrong feature set — so on
/// Windows the forked dialog can outlive the pet.
#[cfg(unix)]
fn parent_died(start: u32) -> bool {
    (unsafe { libc::getppid() as u32 }) != start
}

#[cfg(not(unix))]
fn parent_died(_start: u32) -> bool {
    false
}

struct Ui {
    font: FontArc,
    fields: [String; 3],
    sel: usize,
    caret: usize,
    window: Option<Arc<Window>>,
    pixels: Option<pixels::Pixels<'static>>,
    size: (u32, u32),
    rects: Vec<(i32, i32, i32, i32)>,
    save_rect: (i32, i32, i32, i32),
    status: String,
    mouse_pos: (f64, f64),
    /// Ask mode: one question field + Ask button (writes the ask file).
    ask: bool,
    /// Settings mode: adults-only content checkbox.
    adult: bool,
    adult_rect: (i32, i32, i32, i32),
    /// Settings mode: prefer-adult rotation bias checkbox.
    prefer_adult: bool,
    prefer_rect: (i32, i32, i32, i32),
    /// Settings mode: roaming (walk/fence window movement) checkbox.
    walk: bool,
    walk_rect: (i32, i32, i32, i32),
    /// Selection anchor (char index) for shift-selection / Ctrl+A.
    anchor: Option<usize>,
    /// Live modifier state (winit 0.30: delivered via ModifiersChanged).
    mods: winit::event::Modifiers,
}

impl Ui {
    fn field_text(&self, i: usize) -> String {
        let s = &self.fields[i];
        if !self.ask && i == 1 && !s.is_empty() {
            // Never echo the key back on screen.
            format!("•••••••• ({} chars)", s.chars().count())
        } else if s.is_empty() {
            "…".into()
        } else {
            s.clone()
        }
    }

    fn insert(&mut self, c: char) {
        self.delete_selection();
        let n = self.fields[self.sel].chars().count();
        if n < 300 {
            let mut ch: Vec<char> = self.fields[self.sel].chars().collect();
            ch.insert(self.caret.min(ch.len()), c);
            self.fields[self.sel] = ch.into_iter().collect();
            self.caret += 1;
        }
    }

    fn backspace(&mut self) {
        if self.sel_range().is_some() {
            self.delete_selection();
            return;
        }
        if self.caret > 0 {
            let mut ch: Vec<char> = self.fields[self.sel].chars().collect();
            ch.remove(self.caret - 1);
            self.fields[self.sel] = ch.into_iter().collect();
            self.caret -= 1;
        }
    }

    fn sel_range(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        if a == self.caret {
            None
        } else {
            Some((a.min(self.caret), a.max(self.caret)))
        }
    }

    /// Selected text, or the whole field when nothing is selected.
    fn copy_text(&self) -> String {
        match self.sel_range() {
            Some((a, b)) => self.fields[self.sel].chars().skip(a).take(b - a).collect(),
            None => self.fields[self.sel].clone(),
        }
    }

    fn delete_selection(&mut self) {
        if let Some((a, b)) = self.sel_range() {
            let mut ch: Vec<char> = self.fields[self.sel].chars().collect();
            ch.drain(a..b);
            self.fields[self.sel] = ch.into_iter().collect();
            self.caret = a;
        }
        self.anchor = None;
    }

    fn paste(&mut self, s: &str) {
        self.delete_selection();
        for c in s.chars().filter(|c| !c.is_control()).take(4096) {
            self.insert(c);
        }
    }

    fn move_caret(&mut self, to: usize, shift: bool) {
        if shift {
            if self.anchor.is_none() {
                self.anchor = Some(self.caret);
            }
        } else {
            self.anchor = None;
        }
        self.caret = to;
    }

    fn save(&mut self) {
        if self.ask {
            // Hand the question to the pet via the ask file (mtime-polled).
            let q = self.fields[0].trim().to_string();
            let path = crate::ask_path();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, &q);
            self.status = if q.is_empty() {
                "Nothing asked.".into()
            } else {
                "Question sent to Tahni!".into()
            };
            return;
        }
        let mut cfg = crate::load_config();
        cfg.agent_endpoint = self.fields[0].trim().to_string();
        cfg.agent_key = self.fields[1].trim().to_string();
        cfg.agent_model = self.fields[2].trim().to_string();
        cfg.adult = self.adult;
        cfg.prefer_adult = self.prefer_adult;
        cfg.walk = self.walk;
        crate::save_config(&cfg);
        self.status = "Saved. Tahni reloads this automatically.".to_string();
    }

    fn click(&mut self, x: f64, y: f64) {
        for (i, (rx, ry, rw, rh)) in self.rects.iter().enumerate() {
            if x >= *rx as f64 && x < (rx + rw) as f64 && y >= *ry as f64 && y < (ry + rh) as f64 {
                self.sel = i;
                self.caret = self.fields[i].chars().count();
                self.anchor = None;
                return;
            }
        }
        if !self.ask {
            let (rx, ry, rw, rh) = self.adult_rect;
            if x >= rx as f64 && x < (rx + rw) as f64 && y >= ry as f64 && y < (ry + rh) as f64 {
                self.adult = !self.adult;
                return;
            }
            let (rx, ry, rw, rh) = self.prefer_rect;
            if x >= rx as f64 && x < (rx + rw) as f64 && y >= ry as f64 && y < (ry + rh) as f64 {
                self.prefer_adult = !self.prefer_adult;
                return;
            }
        }
        let (rx, ry, rw, rh) = self.walk_rect;
        if rw > 0 && x >= rx as f64 && x < (rx + rw) as f64 && y >= ry as f64 && y < (ry + rh) as f64 {
            self.walk = !self.walk;
            return;
        }
        let (rx, ry, rw, rh) = self.save_rect;
        if x >= rx as f64 && x < (rx + rw) as f64 && y >= ry as f64 && y < (ry + rh) as f64 {
            self.save();
        }
    }

    fn build_pixels(w: u32, h: u32, window: &Arc<Window>) -> Option<pixels::Pixels<'static>> {
        let st = pixels::SurfaceTexture::new(w, h, window.clone());
        pixels::PixelsBuilder::new(w, h, st)
            .clear_color(pixels::wgpu::Color { r: 0.96, g: 0.96, b: 0.93, a: 1.0 })
            .build()
            .ok()
    }

    fn render(&mut self) {
        if self.ask {
            self.render_ask();
        } else {
            self.render_settings();
        }
    }

    /// Blit the RGBA scratch canvas to the swapchain.
    fn present(&mut self, canvas: Vec<u8>) {
        let Some(px) = &mut self.pixels else { return };
        let frame = px.frame_mut();
        for (dst, src) in frame.chunks_exact_mut(4).zip(canvas.chunks_exact(4)) {
            let a = src[3] as u16;
            dst[0] = (src[0] as u16 * a / 255) as u8;
            dst[1] = (src[1] as u16 * a / 255) as u8;
            dst[2] = (src[2] as u16 * a / 255) as u8;
            dst[3] = a as u8;
        }
        let _ = px.render();
    }

    /// Ask dialog: keeps its own cream/pink look; the retro theme is a
    /// settings-window-only treatment.
    fn render_ask(&mut self) {
        let Some(window) = &self.window else { return };
        let (w, h) = self.size;
        let mut canvas = vec![0u8; (w as usize) * (h as usize) * 4];
        fill_rect(&mut canvas, w, h, 0, 0, w as i32, h as i32, [0xF4, 0xF4, 0xEC, 255]);
        let s = window.scale_factor() as f32;
        let scale = PxScale { x: (15.0 * s).max(11.0), y: (15.0 * s).max(11.0) };
        let lh = line_height(&self.font, scale);
        let mut y = 14.0 * s + lh;
        draw_text(
            &mut canvas, w, h, &self.font, "Ask Tahni",
            14.0 * s, y,
            PxScale { x: scale.x * 1.15, y: scale.y * 1.15 },
            [0xB0, 0x30, 0x60, 255],
        );
        self.rects.clear();
        y += lh * 1.4;
        // The single question field.
        {
            let i = 0usize;
            draw_text(&mut canvas, w, h, &self.font, "Your question", 14.0 * s, y, scale, [0x33, 0x33, 0x33, 255]);
            y += lh * 0.95;
            let (bx, by, bw, bh) = (
                (14.0 * s) as i32,
                (y - lh * 0.55) as i32,
                (w as f32 - 28.0 * s) as i32,
                (lh * 1.5) as i32,
            );
            let fill = if i == self.sel { [0xFF, 0xFF, 0xFF, 255] } else { [0xEF, 0xEF, 0xE7, 255] };
            let border = if i == self.sel { [0xB0, 0x30, 0x60, 255] } else { [0x99, 0x99, 0x99, 255] };
            fill_rect(&mut canvas, w, h, bx, by, bw, bh, fill);
            stroke_rect(&mut canvas, w, h, bx, by, bw, bh, border);
            if i == self.sel {
                if let Some((a, b)) = self.sel_range() {
                    let x_at = |n: usize| -> f32 {
                        let pre = self.fields[i].chars().take(n).collect::<String>();
                        bx as f32 + 6.0 * s + crate::text_width(&self.font, &pre, scale)
                    };
                    fill_rect(
                        &mut canvas, w, h,
                        x_at(a) as i32, (y - lh * 0.35) as i32,
                        (x_at(b) - x_at(a)) as i32, (lh * 1.2) as i32,
                        [0xB0, 0x30, 0x60, 0x55],
                    );
                }
            }
            let shown = self.field_text(i);
            draw_text(&mut canvas, w, h, &self.font, &shown, bx as f32 + 6.0 * s, y + lh * 0.35, scale, [0, 0, 0, 255]);
            if i == self.sel && self.window.as_ref().is_some_and(|w| w.has_focus()) && self.frame_blink() {
                let caret_shown: String = self.fields[i].chars().take(self.caret).collect();
                let cx = bx as f32 + 6.0 * s
                    + crate::text_width(&self.font, &caret_shown, scale);
                fill_rect(&mut canvas, w, h, cx as i32, (y - lh * 0.35) as i32, (2.0 * s).max(1.0) as i32, (lh * 1.2) as i32, [0, 0, 0, 255]);
            }
            self.rects.push((bx, by, bw, bh));
            y += lh * 1.35;
        }
        // Ask button.
        let (bx, by, bw, bh) = ((14.0 * s) as i32, y as i32, (110.0 * s) as i32, (lh * 1.7) as i32);
        fill_rect(&mut canvas, w, h, bx, by, bw, bh, [0xF8, 0xE0, 0xE8, 255]);
        stroke_rect(&mut canvas, w, h, bx, by, bw, bh, [0xB0, 0x30, 0x60, 255]);
        draw_text(&mut canvas, w, h, &self.font, "Ask", bx as f32 + bw as f32 / 2.0 - 16.0 * s, by as f32 + bh as f32 * 0.68, scale, [0xB0, 0x30, 0x60, 255]);
        self.save_rect = (bx, by, bw, bh);
        draw_text(&mut canvas, w, h, &self.font, &self.status, bx as f32 + bw as f32 + 14.0 * s, by as f32 + bh as f32 * 0.68, scale, [0x20, 0x60, 0x20, 255]);
        draw_text(
            &mut canvas, w, h, &self.font, "Enter asks. Esc closes.",
            14.0 * s,
            (by + bh) as f32 + lh * 1.3,
            scale,
            [0x60, 0x60, 0x60, 255],
        );
        self.present(canvas);
    }

    /// Settings window: Win95 property-sheet look — a port of
    /// bonzibuddy-rs `retro.rs` (see that module's docs for the palette
    /// mapping). Same controls and order as before; only the paint is new.
    fn render_settings(&mut self) {
        let Some(window) = &self.window else { return };
        let (w, h) = self.size;
        let mut canvas = vec![0u8; (w as usize) * (h as usize) * 4];
        let s = window.scale_factor() as f32;
        let t = retro::apply_retro_theme(&mut canvas, w, h, s);
        let lh = t.lh(&self.font);
        self.rects.clear();

        let m = 8.0 * s; // window margin
        let ix = 16.0 * s; // inset inside the group borders
        let gw = (w as f32 - 2.0 * m) as i32;
        let fw = (w as f32 - 2.0 * (m + ix)) as i32;
        let fh = (24.0 * s).round() as i32;

        // Geometry pass: two etched group boxes, a Save button row and a
        // sunken status bar along the bottom edge.
        let g1_top = 12.0 * s;
        let mut field_rects = [(0i32, 0i32, 0i32, 0i32); 3];
        let mut y = g1_top + 14.0 * s;
        for i in 0..3 {
            y += lh + 3.0 * s; // label row
            field_rects[i] = ((m + ix) as i32, y as i32, fw, fh);
            y += fh as f32 + 7.0 * s;
        }
        let g1_bot = y + 2.0 * s;
        let g2_top = g1_bot + 8.0 * s;
        let cb1 = g2_top + 16.0 * s;
        let cb2 = cb1 + t.box_px() as f32 + 8.0 * s;
        let cb3 = cb2 + t.box_px() as f32 + 8.0 * s;
        let g2_bot = cb3 + t.box_px() as f32 + 7.0 * s;
        let btn_y = g2_bot + 10.0 * s;
        let sb_h = (22.0 * s) as i32;
        let sb_y = h as i32 - sb_h;

        // Frames first, contents on top.
        retro::group(&mut canvas, w, h, &self.font, &t, m as i32, g1_top as i32, gw, (g1_bot - g1_top) as i32, "Agent connection", false);
        retro::group(&mut canvas, w, h, &self.font, &t, m as i32, g2_top as i32, gw, (g2_bot - g2_top) as i32, "Pet behavior", false);

        for i in 0..3 {
            let (bx, by, bw, bh) = field_rects[i];
            retro::label(&mut canvas, w, h, &self.font, &t, FIELDS[i], bx as f32, by as f32 - 4.0 * s, false);
            retro::field(&mut canvas, w, h, bx, by, bw, bh, false);
            let shown = self.field_text(i);
            let tx = bx as f32 + 5.0 * s;
            let baseline = by as f32 + (bh as f32 - lh) / 2.0 + lh;
            let x_at = |n: usize| -> f32 {
                let pre: String = shown.chars().take(n).collect();
                tx + crate::text_width(&self.font, &pre, t.scale())
            };
            if i == self.sel {
                if let Some((a, b)) = self.sel_range() {
                    // Classic navy selection; the selected glyphs are
                    // redrawn white on top.
                    fill_rect(&mut canvas, w, h, x_at(a) as i32, by + (2.0 * s) as i32, (x_at(b) - x_at(a)).max(0.0) as i32, bh - (4.0 * s) as i32, retro::SELECTION);
                }
            }
            retro::text(&mut canvas, w, h, &self.font, &t, &shown, tx, baseline, retro::TEXT);
            if i == self.sel {
                if let Some((a, b)) = self.sel_range() {
                    let sub: String = shown.chars().skip(a).take(b - a).collect();
                    retro::text(&mut canvas, w, h, &self.font, &t, &sub, x_at(a), baseline, retro::WHITE);
                }
                if self.window.as_ref().is_some_and(|win| win.has_focus()) && self.frame_blink() {
                    fill_rect(&mut canvas, w, h, x_at(self.caret) as i32, by + (3.0 * s) as i32, (2.0 * s).max(1.0) as i32, bh - (6.0 * s) as i32, retro::DARK);
                }
            }
            self.rects.push((bx, by, bw, bh));
        }

        retro::checkbox(&mut canvas, w, h, &self.font, &t, (m + ix) as i32, cb1 as i32, "Show adults-only scenes (nudity)", self.adult, false);
        self.adult_rect = (m as i32, cb1 as i32 - (3.0 * s) as i32, gw, t.box_px() + (7.0 * s) as i32);
        retro::checkbox(&mut canvas, w, h, &self.font, &t, (m + ix) as i32, cb2 as i32, "Prefer adult scenes when unlocked", self.prefer_adult, !self.adult);
        self.prefer_rect = (m as i32, cb2 as i32 - (3.0 * s) as i32, gw, t.box_px() + (7.0 * s) as i32);
        retro::checkbox(&mut canvas, w, h, &self.font, &t, (m + ix) as i32, cb3 as i32, "Walk around the screen (pauses while you type)", self.walk, false);
        self.walk_rect = (m as i32, cb3 as i32 - (3.0 * s) as i32, gw, t.box_px() + (7.0 * s) as i32);

        self.save_rect = retro::button(&mut canvas, w, h, &self.font, &t, m as i32, btn_y as i32, (90.0 * s) as i32, (25.0 * s) as i32, "Save", true, false, false);
        let help = "Click a field to edit. Enter saves. Esc closes. Ctrl+A/C/V/X + Shift+Insert paste; shift to select.";
        let msg = if self.status.is_empty() { help } else { &self.status };
        retro::statusbar(&mut canvas, w, h, &self.font, &t, 0, sb_y, w as i32, sb_h, msg);
        self.present(canvas);
    }

    fn frame_blink(&self) -> bool {
        (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
            % 1000) < 550
    }
}

pub fn run(ask: bool) -> Result<(), Box<dyn std::error::Error>> {
    let el = EventLoop::new()?;
    let start_parent = startup_parent();
    let font = FontArc::try_from_vec(std::fs::read(assets_root().join("fonts/LiberationSans-Regular.ttf"))?)?;
    let cfg = if ask { crate::Config::default() } else { crate::load_config() };
    let mut ui = Ui {
        font,
        fields: if ask {
            [String::new(), String::new(), String::new()]
        } else {
            [cfg.agent_endpoint, cfg.agent_key, cfg.agent_model]
        },
        adult: cfg.adult,
        prefer_adult: cfg.prefer_adult,
        prefer_rect: (0, 0, 0, 0),
        walk: cfg.walk,
        walk_rect: (0, 0, 0, 0),
        adult_rect: (0, 0, 0, 0),
        sel: 0,
        caret: 0,
        anchor: None,
        mods: Default::default(),
        window: None,
        pixels: None,
        size: (W, if ask { 170 } else { H }),
        rects: Vec::new(),
        save_rect: (0, 0, 0, 0),
        status: String::new(),
        mouse_pos: (0.0, 0.0),
        ask,
    };
    ui.caret = ui.fields[0].chars().count();

    #[allow(deprecated)]
    el.run(move |event, el| match event {
        Event::NewEvents(winit::event::StartCause::Init) => {
            let Ok(window) = el.create_window(
                WindowAttributes::default()
                    .with_title(if ui.ask { "Ask Tahni" } else { "DeskMates settings" })
                    .with_inner_size(LogicalSize::new(W, if ui.ask { 170 } else { H }))
                    .with_resizable(false),
            ) else {
                el.exit();
                return;
            };
            if let Ok(img) = image::open(assets_root().join("tray.png")) {
                let img = img.to_rgba8();
                let (iw, ih) = img.dimensions();
                let _ = window.set_window_icon(winit::window::Icon::from_rgba(img.into_raw(), iw, ih).ok());
            }
            let window = Arc::new(window);
            // The surface MUST be sized to the window's *physical* inner
            // size: on a scaled display a logical-sized swapchain on a bigger
            // surface presents as a black box.
            let s = window.inner_size();
            let (pw, ph) = (s.width.max(1), s.height.max(1));
            ui.size = (pw, ph);
            match Ui::build_pixels(pw, ph, &window) {
                Some(p) => {
                    ui.pixels = Some(p);
                    ui.window = Some(window);
                }
                None => {
                    eprintln!("settings: renderer failed");
                    el.exit();
                }
            }
        }
        Event::WindowEvent { event, .. } => match event {
            WindowEvent::Resized(sz) => {
                if sz.width > 0 && sz.height > 0 && (sz.width, sz.height) != ui.size {
                    ui.size = (sz.width, sz.height);
                    if let Some(w) = ui.window.clone() {
                        if let Some(p) = Ui::build_pixels(sz.width, sz.height, &w) {
                            ui.pixels = Some(p);
                        }
                    }
                }
            }
            WindowEvent::Focused(_) | WindowEvent::RedrawRequested => {
                ui.render();
                el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(250)));
            }
            WindowEvent::CursorMoved { position, .. } => {
                ui.mouse_pos = (position.x, position.y);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let (x, y) = ui.mouse_pos;
                ui.click(x, y);
                ui.window.as_ref().map(|w| w.request_redraw());
            }
            WindowEvent::ModifiersChanged(m) => ui.mods = m,
            WindowEvent::KeyboardInput { event: KeyEvent { state: ElementState::Pressed, logical_key, text, .. }, .. } => {
                let shift = ui.mods.state().shift_key();
                let ctrl = ui.mods.state().control_key();
                if ctrl {
                    match text.and_then(|t| t.chars().next()).map(|c| c.to_ascii_lowercase()) {
                        Some('a') => {
                            ui.anchor = Some(0);
                            ui.caret = ui.fields[ui.sel].chars().count();
                        }
                        Some('c') => {
                            let t = ui.copy_text();
                            clip_set(&t);
                        }
                        Some('x') => {
                            let t = ui.copy_text();
                            clip_set(&t);
                            ui.delete_selection();
                        }
                        Some('v') => {
                            let t = clip_get();
                            ui.paste(&t);
                        }
                        _ => {}
                    }
                } else {
                    match &logical_key {
                        Key::Named(NamedKey::Escape) => el.exit(),
                        Key::Named(NamedKey::Enter) => {
                            ui.save();
                            el.exit();
                        }
                        Key::Named(NamedKey::Tab) => {
                            ui.sel = (ui.sel + 1) % if ui.ask { 1 } else { 3 };
                            ui.caret = ui.fields[ui.sel].chars().count();
                            ui.anchor = None;
                        }
                        Key::Named(NamedKey::Backspace) => ui.backspace(),
                        Key::Named(NamedKey::Delete) => {
                            if ui.sel_range().is_some() {
                                ui.delete_selection();
                            } else {
                                let mut ch: Vec<char> = ui.fields[ui.sel].chars().collect();
                                if ui.caret < ch.len() {
                                    ch.remove(ui.caret);
                                    ui.fields[ui.sel] = ch.into_iter().collect();
                                }
                            }
                        }
                        Key::Named(NamedKey::ArrowLeft) => {
                            let t = ui.caret.saturating_sub(1);
                            ui.move_caret(t, shift);
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            let t = (ui.caret + 1).min(ui.fields[ui.sel].chars().count());
                            ui.move_caret(t, shift);
                        }
                        Key::Named(NamedKey::Home) => ui.move_caret(0, shift),
                        Key::Named(NamedKey::End) => {
                            let t = ui.fields[ui.sel].chars().count();
                            ui.move_caret(t, shift);
                        }
                        // Shift+Insert pastes (classic X convention).
                        Key::Named(NamedKey::Insert) if shift => {
                            let t = clip_get();
                            ui.paste(&t);
                        }
                        _ => {
                            if let Some(c) = text
                                .and_then(|t| t.chars().next())
                                .filter(|c| c.is_ascii_graphic() || *c == ' ')
                            {
                                ui.insert(c);
                            }
                        }
                    }
                }
                ui.window.as_ref().map(|w| w.request_redraw());
            }
            WindowEvent::CloseRequested => el.exit(),
            _ => {}
        },
        Event::AboutToWait => {
            // Orphan guard: the pet forked us; if it died we were
            // reparented — close instead of lingering over a dead desktop.
            if parent_died(start_parent) {
                el.exit();
                return;
            }
            // Keep a slow tick so the guard above runs even while the
            // window sits idle (no input events to wake us).
            el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500)));
        }
        _ => {}
    })?;
    Ok(())
}

#[cfg(test)]
mod clip_tests {
    use super::{clip_get, clip_set};

    fn which(bin: &str) -> bool {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .any(|d| d.join(bin).is_file())
    }

    /// One sequential test: the scenarios mutate process-global env
    /// (PATH, the arboard kill-switch), so they must not race.
    #[test]
    fn clipboard_backends() {
        std::env::set_var("DESKMATES_CLIP_NO_ARBOARD", "1");

        // Internal-only fallback: with no binaries on PATH nothing
        // crashes and the buffer still round-trips (never-crash rule).
        let old_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", "/nonexistent");
        assert!(!clip_set("internal-only"));
        assert_eq!(clip_get(), "internal-only");
        std::env::set_var("PATH", old_path);

        // Real xclip fallback (needs an X display; skipped without).
        if std::env::var_os("DISPLAY").is_some() && which("xclip") {
            let probe = format!("deskmates-clip-{}", std::process::id());
            assert!(clip_set(&probe), "xclip -i should succeed");
            assert_eq!(clip_get(), probe);
        } else {
            eprintln!("skip xclip round-trip: no DISPLAY or no xclip");
        }

        std::env::remove_var("DESKMATES_CLIP_NO_ARBOARD");
    }
}
