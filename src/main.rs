//! deskmates-rs — a safe, offline, cross-platform reimplementation of the
//! late-1990s Oska "DeskMates" desktop companions (Tahni first), using the
//! original animation containers extracted from the archived installers.
//!
//! What the original did that this does NOT:
//!   - gate content behind a paid unlock key (`isUnlocked`, `DoBrowserUnlock`
//!     → https://secure.oska.com/store/tahni.php) — every scene plays free
//!   - demo-mode restriction (`DemoList.txt`, `*_demo` sequence variants)
//!   - "visit us" browser launches (http://www.oska.com/dialog/visit_us.php)
//!   - registry writes (HKCU/HKLM `Software\Oska Educational Systems\...`),
//!     ActiveX/CLSID registration (WebAbout.dll)
//!
//! This binary contains zero network code. All content is local, all scenes
//! are unlocked.

mod adult;
mod audio;
mod agent;
mod ask_ui;
mod faz;
mod pet;
mod scenes;

use ab_glyph::{point, Font, FontArc, PxScale, ScaleFont};
use image::RgbaImage;
use pet::Pet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::dpi::{LogicalSize, PhysicalPosition, Position};
use winit::event::{ElementState, Event, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::EventLoop;
use winit::event_loop::ControlFlow;
use winit::window::{Icon, Window, WindowAttributes, WindowLevel};
#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
use winit::window::CursorGrabMode;

const ABOUT_FMT: &str = "DeskMates-rs v0.1\n{CHAR} the Deskmate, safely reimplemented.\nNo phone-home. No unlock keys. No spyware.\n100% offline. Original animation art.\nClick her to make her react!";

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    pub scale: f64,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub character: String,
    pub agent_endpoint: String,
    pub agent_key: String,
    pub agent_model: String,
    /// Show adults-only (nudity) scenes; hidden from all random pools unless true.
    pub adult: bool,
    /// Scene names played during inter-scene gaps instead of blanking
    /// (classify with --scenes / --scene NAME).
    #[serde(default)]
    pub interstitial: Vec<String>,
    /// Pet may roam: walk/fence scenes move the window (settings checkbox).
    #[serde(default = "default_true")]
    pub walk: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { scale: 1.0, x: None, y: None, character: "tahni".into(), agent_endpoint: String::new(), agent_key: String::new(), agent_model: String::new(), adult: false, interstitial: Vec::new(), walk: true }
    }
}

fn default_true() -> bool {
    true
}

fn config_path() -> Option<std::path::PathBuf> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let key = if cfg!(windows) { "APPDATA" } else { "HOME" };
            std::env::var_os(key).map(|h| {
                let mut p = std::path::PathBuf::from(h);
                if !cfg!(windows) {
                    p.push(".config");
                }
                p
            })
        })?;
    Some(dir.join("deskmates-rs.json"))
}

fn load_config() -> Config {
    let mut cfg: Config = config_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if !(cfg.scale.is_finite() && (0.5..=4.0).contains(&cfg.scale)) {
        cfg.scale = 1.0;
    }
    cfg
}

fn save_config(cfg: &Config) {
    if let Some(p) = config_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(b) = serde_json::to_vec_pretty(cfg) {
            let _ = std::fs::write(p, b);
        }
    }
}

/// Question handoff file written by the forked Ask window (mtime-polled by
/// the pet loop).
pub fn ask_path() -> std::path::PathBuf {
    config_path()
        .map(|p| p.with_extension("ask"))
        .unwrap_or_else(|| std::path::PathBuf::from("deskmates-rs.ask"))
}

fn ask_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(ask_path()).ok().and_then(|m| m.modified().ok())
}

fn cfg_mtime() -> Option<std::time::SystemTime> {
    config_path().and_then(|p| std::fs::metadata(p).ok().and_then(|m| m.modified().ok()))
}

/// Fork a WM-managed child window (Ask / Settings): the pet window is
/// override-redirect and never gets keyboard focus; the child does.
fn fork_child(arg: &str) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg(arg).spawn();
    }
}

fn save_state(window: Option<&Window>, scale: f64, character: &str) {
    let pos = window.and_then(|w| w.outer_position().ok());
    let mut cfg = load_config();
    cfg.scale = scale.clamp(0.5, 4.0);
    cfg.x = pos.map(|p| p.x);
    cfg.y = pos.map(|p| p.y);
    cfg.character = character.into();
    if let Some(p) = config_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(b) = serde_json::to_vec_pretty(&cfg) {
            let _ = std::fs::write(p, b);
        }
    }
}

/// X11: clip the window to the opaque pixels of the sprite (bounding +
/// input shape) so the desktop shows through transparent areas even when no
/// compositor is running, and clicks on empty areas fall through to it.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod xshape {
    use x11rb::connection::Connection;
    use x11rb::protocol::shape::{ConnectionExt as ShapeExt, SK, SO};
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as XprotoExt, CreateGCAux, ImageFormat};
    use x11rb::rust_connection::RustConnection;

    /// Persistent X connection dedicated to applying window shapes. winit
    /// 0.30 does not expose the XID, so the window is found by WM_CLASS.
    pub struct Shaper {
        conn: Option<(RustConnection, usize)>,
        class: String,
        window: Option<u32>,
        since_resolve: u32,
        skip_done: bool,
        startup_ticks: u32,
    }

    impl Shaper {
        pub fn new() -> Shaper {
            Shaper {
                conn: x11rb::connect(None).ok(),
                class: std::env::current_exe()
                    .ok()
                    .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
                    .unwrap_or_default(),
                window: None,
                since_resolve: 0,
                skip_done: false,
                startup_ticks: 0,
            }
        }

        /// Locate our top-level window by WM_CLASS instance name.
        fn resolve(&mut self) {
            self.window = None;
            self.since_resolve = 0;
            self.skip_done = false;
            let Some((conn, screen)) = &self.conn else { return };
            let root = conn.setup().roots[*screen].root;
            let tree = conn
                .query_tree(root)
                .ok()
                .and_then(|c| c.reply().ok());
            let Some(tree) = tree else { return };
            for win in tree.children {
                let prop = conn
                    .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 64)
                    .ok()
                    .and_then(|c| c.reply().ok());
                let Some(prop) = prop else { continue };
                let instance: Vec<u8> = prop
                    .value
                    .iter()
                    .copied()
                    .take_while(|&c| c != 0)
                    .collect();
                if instance == self.class.as_bytes() {
                    // The tray-icon crate keeps a hidden 1x1 window with the
                    // same WM_CLASS; the pet is the only one with real size.
                    let big = conn
                        .get_geometry(win)
                        .ok()
                        .and_then(|c| c.reply().ok())
                        .map_or(false, |g| g.width >= 32 && g.height >= 32);
                    if big {
                        self.window = Some(win);
                        return;
                    }
                }
            }
        }

        /// Global pointer position and whether button 1 is held. Polled
        /// during a drag: an X pointer grab on our side connection would
        /// steal motion events from winit's connection (they'd queue
        /// unread), and owner_events only delivers while the pointer is
        /// still over the tiny window — polling works everywhere.
        pub fn query_pointer(&self) -> Option<(i32, i32, bool)> {
            let (conn, screen) = self.conn.as_ref()?;
            let root = conn.setup().roots[*screen].root;
            let r = conn.query_pointer(root).ok()?.reply().ok()?;
            Some((
                r.root_x as i32,
                r.root_y as i32,
                r.mask.contains(x11rb::protocol::xproto::KeyButMask::BUTTON1),
            ))
        }
        /// Drop the pet window from the taskbar/dash: add
        /// `_NET_WM_STATE_SKIP_TASKBAR` (property + client message, so WMs
        /// that poll and WMs that listen both pick it up). Works on plain
        /// X11 and XWayland — KWin reads EWMH on XWayland windows; native
        /// Wayland has no such protocol, one more reason the app forces the
        /// X11 backend. The tray icon is the entry point instead.
        pub fn set_skip_taskbar(&mut self) {
            let Some((conn, screen)) = &self.conn else { return };
            let Some(win) = self.window else { return };
            let root = conn.setup().roots[*screen].root;
            use x11rb::protocol::xproto::{ClientMessageEvent, EventMask, PropMode};
            use x11rb::wrapper::ConnectionExt as PropExt;
            let r = (|| -> Result<(), x11rb::errors::ReplyOrIdError> {
                let state = conn.intern_atom(false, b"_NET_WM_STATE")?.reply()?.atom;
                let skip = conn.intern_atom(false, b"_NET_WM_STATE_SKIP_TASKBAR")?.reply()?.atom;
                conn.change_property32(PropMode::REPLACE, win, state, state, &[skip])?;
                let msg = ClientMessageEvent::new(32, win, state, [1u32, skip, 1, 0, 0]);
                conn.send_event(
                    false,
                    root,
                    EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
                    msg,
                )?;
                conn.flush()?;
                Ok(())
            })();
            let _ = r;
            self.skip_done = true;
        }
        /// Apply the skip-taskbar atom as soon as the window is mapped,
        /// without waiting for the first rendered frame: slow scene-pack
        /// parses (dm05: ~2 min debug FAZ) would otherwise leave the window
        /// listed in the WM window list — title and all — for the whole parse.
        /// Retries at ~8x the event-loop cadence until resolved.
        pub fn ensure_skip_taskbar(&mut self) {
            if self.skip_done {
                return;
            }
            self.startup_ticks += 1;
            if self.window.is_none() && self.startup_ticks % 8 == 0 {
                self.resolve();
            }
            if self.window.is_some() {
                self.set_skip_taskbar();
            }
        }
        /// Clip the window (bounding + input) to the opaque pixels of the
        /// straight-alpha RGBA `canvas` (stride 4, `w`x`h`).
        pub fn apply(&mut self, w: u32, h: u32, canvas: &[u8]) {
            self.since_resolve += 1;
            if self.window.is_none() || self.since_resolve > 120 {
                self.resolve();
                if self.window.is_some() {
                    self.set_skip_taskbar();
                }
            }
            let (Some((conn, _)), Some(win)) = (self.conn.as_ref(), self.window) else {
                return;
            };
            if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 {
                return;
            }
            let (w16, h16) = (w as u16, h as u16);
            // put_image XY_BITMAP rows are padded to 4 bytes.
            let bpl = ((w as usize + 7) / 8 + 3) & !3;
            let mut mask = vec![0u8; bpl * h as usize];
            for y in 0..h as usize {
                for x in 0..w as usize {
                    if canvas[(y * w as usize + x) * 4 + 3] > 0 {
                        mask[y * bpl + x / 8] |= 0x80 >> (x % 8);
                    }
                }
            }
            let Ok(pixmap) = conn.generate_id() else { return };
            if conn.create_pixmap(1, pixmap, win, w16, h16).is_err() {
                return;
            }
            let Ok(gc) = conn.generate_id() else {
                let _ = conn.free_pixmap(pixmap);
                return;
            };
            // XY_BITMAP put_image paints 1-bits with the GC foreground and
            // 0-bits with the background: fg=1/bg=0 makes the pixmap == mask.
            if conn
                .create_gc(gc, pixmap, &CreateGCAux::new().foreground(1).background(0))
                .is_err()
            {
                let _ = conn.free_pixmap(pixmap);
                return;
            }
            if conn
                .put_image(ImageFormat::XY_BITMAP, pixmap, gc, w16, h16, 0, 0, 0, 1, &mask)
                .is_ok()
            {
                let _ = conn.shape_mask(SO::SET, SK::BOUNDING, win, 0, 0, pixmap);
                let _ = conn.shape_mask(SO::SET, SK::INPUT, win, 0, 0, pixmap);
            }
            let _ = conn.free_gc(gc);
            let _ = conn.free_pixmap(pixmap);
            let _ = conn.flush();
        }
    }
}


fn assets_root() -> std::path::PathBuf {
    // ./assets next to the binary or in the cwd, like bonzibuddy-rs.
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    for base in [std::path::PathBuf::from("assets"), exe_dir.join("assets")] {
        if base.join("characters").is_dir() {
            return base;
        }
    }
    std::path::PathBuf::from("assets")
}

/// All playable characters shipped under assets/characters/.
const CHARACTERS: &[&str] = &["tahni", "fatb", "maeka", "kahli", "teecee", "johlee", "oska"];

fn display_name(character: &str) -> String {
    match character {
        "fatb" => "FatB".to_string(),
        "teecee" => "TeeCee".to_string(),
        "sns" => "SnS".to_string(),
        other => {
            let mut c = other.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => other.to_string(),
            }
        }
    }
}

fn make_tray(display: &str) -> Option<tray_icon::TrayIcon> {
    use tray_icon::menu::{Menu, MenuItem, Submenu};
    use tray_icon::TrayIconBuilder;
    #[cfg(target_os = "linux")]
    {
        if let Err(e) = gtk::init() {
            eprintln!("tray: gtk init failed: {e}");
            return None;
        }
    }
    let img = image::open(assets_root().join("tray.png")).ok()?.to_rgba8();
    let (tw, th) = (img.width(), img.height());
    let icon = tray_icon::Icon::from_rgba(img.into_raw(), tw, th).ok()?;
    let menu = Menu::new();
    let toggle = MenuItem::with_id("toggle", "Show / Hide", true, None);
    // Full action mirror of the in-window right-click menu: the tray is the
    // hitbox-proof entry point (the pet window is tiny and shaped).
    let mv = MenuItem::with_id("move", "Move", true, None);
    let touch = MenuItem::with_id("touch", "Touch", true, None);
    let ask = MenuItem::with_id("ask", "Ask…", true, None);
    let next = MenuItem::with_id("next", "Next scene", true, None);
    let settings = MenuItem::with_id("settings", "Settings…", true, None);
    let about = MenuItem::with_id("about", "About", true, None);
    let chars = Submenu::new("Character", true);
    for c in CHARACTERS {
        let item = MenuItem::with_id(format!("char:{c}"), display_name(c), true, None);
        chars.append_items(&[&item]).ok();
    }
    let quit = MenuItem::with_id("quit", "Quit", true, None);
    menu.append_items(&[&toggle, &mv, &touch, &ask, &next, &chars, &settings, &about, &quit]).ok()?;
    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(format!("{display} the Deskmate"))
        .with_icon(icon)
        .build()
        .map_err(|e| eprintln!("tray: build failed: {e} (Plasma: check tray Settings > Entries - new items can be hidden by default)")).ok()
}

/// Left-button press state for drag-to-move. On X11 the drag is driven by
/// polling the global pointer (see `Shaper::query_pointer`): `off` is the
/// press point in physical pixels relative to the window origin, so the
/// window position is simply global - off. Elsewhere winit's confined grab
/// keeps motion flowing and deltas are applied per event.
struct Press {
    start: (f64, f64),
    last: (f64, f64),
    win: (i32, i32),
    off: (i32, i32),
    /// Global pointer at press time (X11 drag-start threshold).
    gstart: (i32, i32),
    dragging: bool,
}

struct MenuItem {
    label: &'static str,
}

const MENU: &[MenuItem] = &[
    MenuItem { label: "Touch" },
    // Plain "Ask…" for every character (the tray mirrors it).
    MenuItem { label: "Ask…" },
    MenuItem { label: "Next scene" },
    MenuItem { label: "Next character" },
    MenuItem { label: "Settings…" },
    MenuItem { label: "Hide" },
    MenuItem { label: "About" },
    MenuItem { label: "Quit" },
];

struct App {
    pet: Pet,
    scenes: Arc<scenes::Scenes>,
    font: FontArc,
    scale: f64,
    fw: u32,
    fh: u32,
    /// Scene canvas size (unscaled) the window currently matches.
    cur_dims: (u32, u32),
    matte: bool,
    character: String,
    display: String,
    pixels: Option<pixels::Pixels<'static>>,
    window: Option<Arc<Window>>,
    size: (u32, u32),
    mouse: (f64, f64),
    press: Option<Press>,
    menu_open: bool,
    menu_rects: Vec<(usize, (i32, i32, i32, i32))>,
    about_until: Option<Instant>,
    agent: agent::AgentCfg,
    agent_tx: std::sync::mpsc::Sender<agent::Msg>,
    agent_rx: std::sync::mpsc::Receiver<agent::Msg>,
    /// Speech bubble: text shown until the given instant.
    bubble: Option<(String, Instant)>,
    ask_mtime: Option<std::time::SystemTime>,
    cfg_mtime: Option<std::time::SystemTime>,
    /// Config toggle: pet may roam (walk/fence scenes move the window).
    walk_enabled: bool,
    /// User-idle state (MIT-SCREEN-SAVER): the original gated its actions
    /// on user inactivity; we gate roaming on it too.
    user_idle: bool,
    last_idle_query: Instant,
    tray: Option<tray_icon::TrayIcon>,
    last_move: Instant,
    /// Fractional window x for smooth walking.
    walk_x: f64,
    /// Last scene whose frames were pre-decoded (jank-free playback).
    preloaded: String,
    /// Wave archive playback for ` !name` script tokens.
    audio: crate::audio::Audio,
    /// X11 window shaper (transparent clipping); unused on other platforms.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    shaper: xshape::Shaper,
    /// Window auto-hidden during a blank inter-scene gap.
    hidden_by_blank: bool,
    /// Window position to return to after a fence slide (rest anchor).
    fence_rest: Option<(i32, i32)>,
    /// When the window was first seen fully off-screen mid-scene with a
    /// known home position (see `ensure_visible`).
    off_since: Option<Instant>,
    /// Last render drew any visible content (sprite in bounds, matte).
    drew_content: bool,
    /// Suppress auto-hide until this instant (user just chose Show).
    blank_hide_after: Option<Instant>,
    /// Tray "Move" armed at this instant: the window follows the global
    /// pointer until the next click lands it (None = not moving).
    move_mode: Option<Instant>,
}

/// EWMH `_NET_WORKAREA` rectangles `(x, y, w, h)` published by the WM
/// (KDE/GNOME publish them on X and under XWayland), cached per process.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
static NET_WORKAREAS: std::sync::LazyLock<Vec<(i32, i32, i32, i32)>> = std::sync::LazyLock::new(|| {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::ConnectionExt as _;
        let Ok((conn, _)) = x11rb::connect(None) else {
            return Vec::new();
        };
        let root = conn.setup().roots[0].root;
        let Some(atom) = conn.intern_atom(false, b"_NET_WORKAREA").ok().and_then(|c| c.reply().ok()) else {
            return Vec::new();
        };
        let Some(prop) = conn
            .get_property(false, root, atom.atom, x11rb::protocol::xproto::AtomEnum::CARDINAL, 0, 64)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            return Vec::new();
        };
        let vals: Vec<u32> = prop.value32().map(|it| it.collect()).unwrap_or_default();
        vals.chunks_exact(4)
            .map(|c| (c[0] as i32, c[1] as i32, c[2] as i32, c[3] as i32))
            .collect()
});

/// Monitor work area (taskbar/docks excluded) as `(x, y, w, h)` — the
/// clone of the original's `SPI_GETWORKAREA` (PLAYER_RE §2). winit 0.30
/// exposes no work area, so on X we take the WM's `_NET_WORKAREA` rect
/// that overlaps the monitor most; without a WM (Xvfb, bare X) the full
/// monitor is the work area.
fn work_area(m: &winit::monitor::MonitorHandle) -> (i32, i32, i32, i32) {
    let o = m.position();
    let s = m.size();
    let full = (o.x, o.y, s.width as i32, s.height as i32);
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let mut best: Option<((i32, i32, i32, i32), i64)> = None;
        for &r in NET_WORKAREAS.iter() {
            let ix = (full.0 + full.2).min(r.0 + r.2) - full.0.max(r.0);
            let iy = (full.1 + full.3).min(r.1 + r.3) - full.1.max(r.1);
            if ix <= 0 || iy <= 0 {
                continue;
            }
            let area = ix as i64 * iy as i64;
            if best.map_or(true, |(_, a)| area > a) {
                best = Some((r, area));
            }
        }
        if let Some((r, _)) = best {
            return r;
        }
    }
    full
}

/// PLAYER_RE §2 "Rest position": clamp a requested rest point so the
/// window stays inside the work area; if less than 2/3 of the sprite
/// would be visible there, re-center it in the work area instead (the
/// original's home-anchor behavior).
fn clamp_rest(x: i32, y: i32, ww: i32, wh: i32, wa: (i32, i32, i32, i32)) -> (i32, i32) {
    let (wx, wy, waw, wah) = wa;
    let (lox, hix) = (wx.min(wx + waw - ww), wx.max(wx + waw - ww));
    let (loy, hiy) = (wy.min(wy + wah - wh), wy.max(wy + wah - wh));
    let (cx, cy) = (x.clamp(lox, hix), y.clamp(loy, hiy));
    let vis_w = (cx + ww).min(wx + waw) - cx.max(wx);
    let vis_h = (cy + wh).min(wy + wah) - cy.max(wy);
    if vis_w * 3 < ww * 2 || vis_h * 3 < wh * 2 {
        (wx + (waw - ww) / 2, wy + (wah - wh) / 2)
    } else {
        (cx, cy)
    }
}

impl App {
    fn win_logical(&self) -> (u32, u32) {
        ((self.fw as f64 * self.scale) as u32, (self.fh as f64 * self.scale) as u32)
    }

    /// Faithful to the original: the window IS the current scene's
    /// canvas (the ActiveX buddy had no fixed box; each scene carries
    /// its own canvas size and frames are authored against it). Resize
    /// to the scene canvas on scene change, feet anchored in place.
    fn sync_scene_window(&mut self) {
        let Some(w) = self.window.clone() else { return };
        let Some(sp) = self.pet.sprite() else { return };
        let Some(scn) = self.scenes.scenes.get(&sp.scene) else { return };
        if scn.w == self.cur_dims.0 && scn.h == self.cur_dims.1 {
            return;
        }
        self.cur_dims = (scn.w, scn.h);
        anchored_resize(
            &w,
            (scn.w as f64 * self.scale) as u32,
            (scn.h as f64 * self.scale) as u32,
        );
        w.request_redraw();
    }

    /// Hot-swap the playable character: reload scenes, rebuild pet, resize
    /// the window, persist choice, refresh tray tooltip.
    fn switch_character(&mut self, name: &str) {
        if name == self.character || !CHARACTERS.contains(&name) {
            return;
        }
        let Ok(sc) = scenes::Scenes::load(
            &assets_root().join("characters").join(name),
        ) else {
            return;
        };
        let (fw, fh) = sc.frame_size();
        self.scenes = Arc::new(sc);
        self.fw = fw;
        self.cur_dims = (fw, fh);
        self.character = name.to_string();
        self.display = display_name(name);
        self.pet = Pet::new(self.scenes.clone(), load_config().adult);
        // Rebind sound playback to the new character's sounds dir. The
        // WAV cache lives inside Audio keyed by bare name, so rebuilding
        // it here also stops same-named waves (LAND, SWOOSH_A, …) from
        // leaking across characters after a switch.
        self.audio = crate::audio::Audio::new(
            assets_root().join("characters").join(name).join("sounds"),
        );
        self.preloaded = String::new();
        self.bubble = None;
        self.menu_open = false;
        if let Some(w) = &self.window {
            let (lw, lh) = self.win_logical();
            anchored_resize(w, lw, lh);
            let _ = w.set_title(&format!("{} the Deskmate", self.display));
            w.request_redraw();
        }
        save_state(self.window.as_deref(), self.scale, name);
        // Rebuilding the tray (drop + re-register) makes the icon vanish on
        // Linux status-notifier buses; keep it and just update the tooltip.
        match &self.tray {
            Some(t) => {
                let _ = t.set_tooltip(Some(format!("{} the Deskmate", self.display)));
            }
            None => self.tray = make_tray(&self.display),
        }
    }

    /// Show text in the speech bubble for a duration scaled by length.
    fn speak(&mut self, text: &str) {
        let secs = (2.5 + text.chars().count() as f64 * 0.06).min(14.0);
        self.bubble = Some((text.to_string(), Instant::now() + Duration::from_secs_f64(secs)));
    }
    /// Walk the window along the bottom of the monitor's work area per
    /// pet.walk_dir(), and slide it between a screen edge and its rest
    /// position for `fence_*` transitions (walk in / walk out).
    ///
    /// Speed follows the original: `[dx,dy]` is a signed int16 delta
    /// applied per 100 ms frame tick — `[200,0]` = 200 px/frame =
    /// 2000 px/s (PLAYER_RE §2).
    fn apply_walk(&mut self, now: Instant) {
        let Some(w) = &self.window else { return };
        let dt = now.duration_since(self.last_move).as_secs_f64().min(0.25);
        self.last_move = now;
        let speed = 2000.0 * self.scale;
        if let Some(f) = self.pet.fence() {
            let Some(m) = w.current_monitor().or_else(|| w.primary_monitor()) else { return };
            let wa = work_area(&m);
            let (ww, wh) = {
                let s = w.outer_size();
                (s.width as i32, s.height as i32)
            };
            let p = match w.outer_position() { Ok(p) => p, Err(_) => return };
            // Rest position: the home anchor, clamped into the work area
            // with the <2/3-visible recenter (§2 "Rest position").
            let rest = match self.fence_rest {
                Some(r) => r,
                None => {
                    let r = clamp_rest(p.x, p.y, ww, wh, wa);
                    self.fence_rest = Some(r);
                    r
                }
            };
            // Edge anchor: window fully off-screen at the fence edge,
            // measured against the work area (`fence_home` = inset 1).
            let ex = match f.dx {
                1 => wa.0 - ww,
                -1 => wa.0 + wa.2,
                _ => rest.0,
            };
            let ey = match f.dy {
                1 => wa.1 - wh,
                -1 => wa.1 + wa.3,
                _ => rest.1,
            };
            let (tx, ty) = if f.entering { (rest.0, rest.1) } else { (ex, ey) };
            let step = speed * dt;
            let mut nx = p.x as f64;
            let mut ny = p.y as f64;
            let mut done = true;
            for (cur, tgt, ax) in [(p.x as f64, tx as f64, &mut nx), (p.y as f64, ty as f64, &mut ny)] {
                let d = tgt - cur;
                if d.abs() <= step.max(1.0) {
                    *ax = tgt;
                } else {
                    *ax = cur + d.signum() * step;
                    done = false;
                }
            }
            let _ = w.set_outer_position(Position::Physical(PhysicalPosition::new(nx as i32, ny as i32)));
            if let Ok(p) = w.outer_position() {
                self.walk_x = p.x as f64;
            }
            if done {
                // Keep fence_rest across exits: it is the home the next
                // `fence_*_in` slides back to. Clearing it here made the
                // pet re-enter at the off-screen edge and stay invisible.
                self.pet.fence_done();
            }
            return;
        }
        let dir = self.pet.walk_dir();
        if dir == 0 {
            if let Ok(p) = w.outer_position() {
                self.walk_x = p.x as f64;
            }
            return;
        }
        let Some(m) = w.current_monitor().or_else(|| w.primary_monitor()) else { return };
        let wa = work_area(&m);
        let ww = w.outer_size().width as i32;
        let mut x = self.walk_x + dir as f64 * speed * dt;
        // Clamp at the work-area edges: a walked-off character stops just
        // off screen (the original sprite simply left the desktop); the
        // old wrap-around made the empty window march forever.
        let lo = (wa.0 - ww + 24) as f64;
        let hi = (wa.0 + wa.2 - 24) as f64;
        if x < lo || x > hi {
            x = x.clamp(lo, hi);
            if (x - self.walk_x).abs() < 0.5 {
                self.pet.abort_walk();
            }
        }
        self.walk_x = x;
        if let Ok(p) = w.outer_position() {
            w.set_outer_position(Position::Physical(PhysicalPosition::new(x as i32, p.y)));
        }
    }
    /// A scene that plays frames while the window is parked off-screen
    /// (previous scene walked out, this one has no `fence_*_in`) would
    /// leave the pet invisible forever. After a grace period long enough
    /// for the next scene's own walk-in to begin, slide it back home.
    /// Only applies when a home position is known (a fence transition
    /// happened since the last drag); a user-dragged off-screen window
    /// stays where it was put.
    fn ensure_visible(&mut self, now: Instant) {
        let out = (|| -> Option<bool> {
            let w = self.window.as_ref()?;
            if self.pet.fence().is_some() || self.pet.walk_dir() != 0 || !self.pet.playing() {
                return Some(false);
            }
            let m = w.current_monitor().or_else(|| w.primary_monitor())?;
            // Off-screen is measured against the work area (§2): the
            // taskbar strip counts as "not on the desk".
            let (mx, my, mw, mh) = work_area(&m);
            let s = w.outer_size();
            let p = w.outer_position().ok()?;
            let (ww, wh) = (s.width as i32, s.height as i32);
            Some(p.x + ww <= mx || p.x >= mx + mw || p.y + wh <= my || p.y >= my + mh)
        })()
            .unwrap_or(false);
        if !out || self.fence_rest.is_none() {
            self.off_since = None;
            return;
        }
        match self.off_since {
            None => self.off_since = Some(now),
            Some(t) if now.duration_since(t) < Duration::from_secs_f64(2.5) => {}
            Some(_) => {
                if let (Some(w), Some(rest)) = (&self.window, self.fence_rest) {
                    let _ = w
                        .set_outer_position(Position::Physical(PhysicalPosition::new(rest.0, rest.1)));
                    self.walk_x = rest.0 as f64;
                }
                self.off_since = None;
            }
        }
    }

    fn render(&mut self) {
        let Some(px) = &mut self.pixels else { return };
        let (w, h) = self.size;
        let sf = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
        let mut canvas = vec![0u8; (w as usize) * (h as usize) * 4];
        if self.matte {
            for p in canvas.chunks_exact_mut(4) {
                p.copy_from_slice(&[40, 40, 46, 255]);
            }
        }
        // sprite. Frame 0000 is a real frame (the standing base pose in
        // banks that have it), not a blank: skipping it was part of the
        // inter-scene blink. A missing frame simply fails to decode.
        // A sprite walked off the window's edge draws nothing; the empty
        // window must auto-hide like a blank gap instead of showing a
        // black box (checked via canvas alpha below).
        if let Some(sp) = self.pet.sprite().cloned() {
            if let Some(img) = self.scenes.frame(&sp.scene, sp.id) {
                let s = (self.scale * sf) as f64;
                let ox = (sp.x as f64 * self.scale * sf) as i32;
                let oy = (sp.y as f64 * self.scale * sf) as i32;
                blit(&mut canvas, w, h, &img, ox, oy, s);
            }
        }
        // `drew_content`: does the canvas actually show pixels? A held
        // frame can be a transparent placeholder, and a walked-off sprite
        // lands outside the window; both must count as blank so the empty
        // window auto-hides instead of showing a black box.
        self.drew_content = self.matte
            || canvas.chunks_exact(4).any(|p| p[0] | p[1] | p[2] != 0);
        // speech bubble (top of window, over the sprite)
        if let Some((text, until)) = self.bubble.clone() {
            if Instant::now() >= until {
                self.bubble = None;
            } else {
                let s = (sf * self.scale.max(1.0)) as f32;
                let scale = PxScale::from(10.0 * s);
                let lh = line_height(&self.font, scale);
                let bw = (w as i32 - 6).max(1);
                let lines: Vec<String> = wrap(&self.font, &text, (bw - 12) as f32, scale)
                    .into_iter().take(6).collect();
                let bh = (lh * (lines.len() as f32 + 0.8)) as i32;
                fill_rect(&mut canvas, w, h, 3, 3, bw, bh, [255, 255, 240, 242]);
                stroke_rect(&mut canvas, w, h, 3, 3, bw, bh, [176, 48, 96, 255]);
                let mut y = 3.0 + lh;
                for line in &lines {
                    draw_text(&mut canvas, w, h, &self.font, line, 9.0, y, scale, [20, 20, 25, 255]);
                    y += lh;
                }
            }
        }
        // menu overlay
        self.menu_rects.clear();
        if self.menu_open {
            let s = (sf * self.scale.max(1.0)) as f32;
            let bw = (130.0 * s) as i32;
            let bh = (24.0 * s) as i32;
            let bx = (w as i32 - bw).max(0);
            let mut by = 0i32;
            for (i, item) in MENU.iter().enumerate() {
                fill_rect(&mut canvas, w, h, bx, by, bw, bh, [245, 245, 250, 235]);
                stroke_rect(&mut canvas, w, h, bx, by, bw, bh, [120, 120, 130, 255]);
                let label = item.label.replace("{CHAR}", &self.display);
                draw_text(&mut canvas, w, h, &self.font, &label, bx as f32 + 8.0 * s, by as f32 + bh as f32 * 0.68, PxScale::from(13.0 * s), [20, 20, 25, 255]);
                self.menu_rects.push((i, (bx, by, bw, bh)));
                by += bh;
            }
        }
        // about overlay
        if let Some(until) = self.about_until {
            if Instant::now() >= until {
                self.about_until = None;
            } else {
                let s = (sf * self.scale.max(1.0)) as f32;
                let bw = (w as i32 - 8) as i32;
                let bh = (h as i32 - 8).max(1);
                fill_rect(&mut canvas, w, h, 4, 4, bw, bh, [250, 250, 252, 240]);
                stroke_rect(&mut canvas, w, h, 4, 4, bw, bh, [120, 120, 130, 255]);
                let scale = PxScale::from(10.5 * s);
                let lh = line_height(&self.font, scale);
                let mut y = 4.0 + lh;
                let about = ABOUT_FMT.replace("{CHAR}", &self.display);
                for line in about.lines() {
                    for sub in wrap(&self.font, line, (bw - 12) as f32, scale) {
                        draw_text(&mut canvas, w, h, &self.font, &sub, 10.0, y, scale, [20, 20, 25, 255]);
                        y += lh;
                    }
                }
            }
        }
        // RGBA canvas -> premultiplied RGBA frame buffer (pixels' format).
        {
            let buf = px.frame_mut();
            for (dst, src) in buf.chunks_exact_mut(4).zip(canvas.chunks_exact(4)) {
                let a = src[3] as u16;
                dst[0] = (src[0] as u16 * a / 255) as u8;
                dst[1] = (src[1] as u16 * a / 255) as u8;
                dst[2] = (src[2] as u16 * a / 255) as u8;
                dst[3] = a as u8;
            }
        }
        if let Err(e) = px.render() {
            let s = e.to_string();
            if !s.contains("Lost") && !s.contains("timeout") {
                eprintln!("present err: {e}");
            }
        }
        let _ = px.queue();
        // Shape the window to the sprite: real transparency on bare X11.
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            if !self.matte {
                self.shaper.apply(w, h, &canvas);
            }
        }
    }

    fn menu_click(&mut self, x: f64, y: f64) -> bool {
        for (i, (mx, my, mw, mh)) in self.menu_rects.iter().rev() {
            if x >= *mx as f64 && x < (*mx + *mw) as f64 && y >= *my as f64 && y < (*my + *mh) as f64 {
                self.menu_open = false;
                match MENU[*i].label {
                    "Touch" => self.pet.touch(),
                    "Ask…" => {
                        if self.agent.enabled() {
                            fork_child("--ask-ui");
                        } else {
                            self.speak("I can chat, but no AI endpoint is set. Right-click, Settings… to add one!");
                        }
                    }
                    "Next scene" => self.pet.next_scene(),
                    "Next character" => {
                        let next = CHARACTERS
                            .iter()
                            .position(|c| *c == self.character)
                            .map(|i| CHARACTERS[(i + 1) % CHARACTERS.len()])
                            .unwrap_or("tahni");
                        self.switch_character(next);
                    }
                    "Settings…" => fork_child("--settings-ui"),
                    "Hide" => {
                        if let Some(w) = &self.window {
                            w.set_visible(false);
                            self.hidden_by_blank = false;
                        }
                    }
                    "About" => self.about_until = Some(Instant::now() + Duration::from_secs(8)),
                    "Quit" => std::process::exit(0),
                    _ => {}
                }
                return true;
            }
        }
        false
    }
}

/// Resize the window keeping the sprite's feet (bottom-center) pinned
/// at their current screen position: x recenters by half the width
/// delta, y shifts by the full height delta.
fn anchored_resize(w: &Window, lw: u32, lh: u32) {
    let sf = w.scale_factor();
    let s = w.outer_size();
    if let Ok(p) = w.outer_position() {
        let tw = (lw as f64 * sf) as i32;
        let th = (lh as f64 * sf) as i32;
        let nx = p.x + (s.width as i32 - tw) / 2;
        let ny = p.y + s.height as i32 - th;
        w.set_outer_position(Position::Physical(PhysicalPosition::new(nx, ny)));
    }
    let _ = w.request_inner_size(LogicalSize::new(lw as f64, lh as f64));
}

fn blit(canvas: &mut [u8], cw: u32, ch: u32, img: &RgbaImage, x: i32, y: i32, scale: f64) {
    let (iw, ih) = img.dimensions();
    let dw = (iw as f64 * scale) as i32;
    let dh = (ih as f64 * scale) as i32;
    for dy in 0..dh {
        let py = y + dy;
        if py < 0 || py >= ch as i32 {
            continue;
        }
        // Pixel-center bilinear sampling: exact nearest at scale == 1,
        // smooth upscale above it.
        let fy = (dy as f64 + 0.5) / scale - 0.5;
        let y0 = fy.floor() as i32;
        let wy = fy - y0 as f64;
        for dx in 0..dw {
            let px = x + dx;
            if px < 0 || px >= cw as i32 {
                continue;
            }
            let fx = (dx as f64 + 0.5) / scale - 0.5;
            let x0 = fx.floor() as i32;
            let wx = fx - x0 as f64;
            // Alpha-weighted (premultiplied) taps: fully transparent
            // pixels must not bleed their (often black) color into
            // opaque neighbours - only alpha interpolates linearly.
            let mut wa = 0.0f64;
            let mut wr = 0.0f64;
            let mut wg = 0.0f64;
            let mut wb = 0.0f64;
            for (oy, wyw) in [(y0, 1.0 - wy), (y0 + 1, wy)] {
                let oy = oy.clamp(0, ih as i32 - 1) as u32;
                for (ox, wxw) in [(x0, 1.0 - wx), (x0 + 1, wx)] {
                    let ox = ox.clamp(0, iw as i32 - 1) as u32;
                    let [r, g, b, a] = img.get_pixel(ox, oy).0;
                    let w = a as f64 * wyw * wxw;
                    wa += w;
                    wr += w * r as f64;
                    wg += w * g as f64;
                    wb += w * b as f64;
                }
            }
            if wa <= 0.0 {
                continue;
            }
            let af = wa.round().clamp(1.0, 255.0) as u16;
            let r = (wr / wa).clamp(0.0, 255.0) as u8;
            let g = (wg / wa).clamp(0.0, 255.0) as u8;
            let b = (wb / wa).clamp(0.0, 255.0) as u8;
            let o = (py as usize * cw as usize + px as usize) * 4;
            let d = &mut canvas[o..o + 4];
            d[0] = ((r as u16 * af + d[0] as u16 * (255 - af)) / 255) as u8;
            d[1] = ((g as u16 * af + d[1] as u16 * (255 - af)) / 255) as u8;
            d[2] = ((b as u16 * af + d[2] as u16 * (255 - af)) / 255) as u8;
            d[3] = af.max(d[3] as u16) as u8;
        }
    }
}

fn fill_rect(c: &mut [u8], cw: u32, ch: u32, x: i32, y: i32, w: i32, h: i32, rgba: [u8; 4]) {
    for py in y.max(0)..(y + h).min(ch as i32) {
        for px in x.max(0)..(x + w).min(cw as i32) {
            let o = (py as usize * cw as usize + px as usize) * 4;
            let a = rgba[3] as u16;
            let d = &mut c[o..o + 4];
            d[0] = ((rgba[0] as u16 * a + d[0] as u16 * (255 - a)) / 255) as u8;
            d[1] = ((rgba[1] as u16 * a + d[1] as u16 * (255 - a)) / 255) as u8;
            d[2] = ((rgba[2] as u16 * a + d[2] as u16 * (255 - a)) / 255) as u8;
            d[3] = (a.max(d[3] as u16)) as u8;
        }
    }
}

fn stroke_rect(c: &mut [u8], cw: u32, ch: u32, x: i32, y: i32, w: i32, h: i32, rgba: [u8; 4]) {
    fill_rect(c, cw, ch, x, y, w, 1, rgba);
    fill_rect(c, cw, ch, x, y + h - 1, w, 1, rgba);
    fill_rect(c, cw, ch, x, y, 1, h, rgba);
    fill_rect(c, cw, ch, x + w - 1, y, 1, h, rgba);
}

fn line_height(font: &FontArc, scale: PxScale) -> f32 {
    let sf = font.as_scaled(scale);
    (sf.ascent() - sf.descent() + sf.line_gap()).max(scale.y * 1.15)
}

fn text_width(font: &FontArc, text: &str, scale: PxScale) -> f32 {
    let sf = font.as_scaled(scale);
    text.chars().map(|c| sf.h_advance(sf.glyph_id(c))).sum()
}

fn wrap(font: &FontArc, text: &str, max_w: f32, scale: PxScale) -> Vec<String> {
    let sf = font.as_scaled(scale);
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut width = 0.0f32;
    for word in text.split_whitespace() {
        let ww: f32 = word.chars().map(|c| sf.h_advance(sf.glyph_id(c))).sum();
        let sp = if cur.is_empty() { 0.0 } else { sf.h_advance(sf.glyph_id(' ')) };
        if width + sp + ww > max_w && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            width = 0.0;
        }
        if !cur.is_empty() {
            cur.push(' ');
            width += sp;
        }
        cur.push_str(word);
        width += ww;
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

fn draw_text(c: &mut [u8], cw: u32, ch: u32, font: &FontArc, text: &str, x: f32, y: f32, scale: PxScale, color: [u8; 4]) {
    let sf = font.as_scaled(scale);
    let mut pen_x = x;
    for g in text.chars() {
        let gid = sf.glyph_id(g);
        let glyph = gid.with_scale_and_position(scale, point(pen_x, y));
        if let Some(v) = font.outline_glyph(glyph) {
            let bounds = v.px_bounds();
            v.draw(|gx, gy, cov| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px < 0 || py < 0 || px >= cw as i32 || py >= ch as i32 || cov <= 0.0 {
                    return;
                }
                let a = (cov * color[3] as f32) as u16;
                let o = (py as usize * cw as usize + px as usize) * 4;
                let d = &mut c[o..o + 4];
                d[0] = ((color[0] as u16 * a + d[0] as u16 * (255 - a)) / 255) as u8;
                d[1] = ((color[1] as u16 * a + d[1] as u16 * (255 - a)) / 255) as u8;
                d[2] = ((color[2] as u16 * a + d[2] as u16 * (255 - a)) / 255) as u8;
                d[3] = (a.max(d[3] as u16)) as u8;
            });
        }
        pen_x += sf.h_advance(gid);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Child modes: forked WM-managed windows (real keyboard focus).
    let argv: Vec<String> = std::env::args().collect();
    if argv.iter().any(|a| a == "--ask-ui") {
        return ask_ui::run(true).map(|_| ());
    }
    if argv.iter().any(|a| a == "--settings-ui") {
        return ask_ui::run(false).map(|_| ());
    }
    let args: Vec<String> = argv;
    let matte = args.iter().any(|a| a == "--matte");
    let cfg = load_config();
    let character = args
        .iter()
        .position(|a| a == "--character")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or(cfg.character.clone());
    let display = display_name(&character);
    // `--all`: one instance per character, fanned out along the bottom edge.
    if args.iter().any(|a| a == "--all") {
        if let Ok(exe) = std::env::current_exe() {
            let mut i = 1i32;
            for c in CHARACTERS {
                if *c == character {
                    continue;
                }
                let _ = std::process::Command::new(&exe)
                    .arg("--character")
                    .arg(c)
                    .arg("--offset")
                    .arg(i.to_string())
                    .spawn();
                i += 1;
            }
        }
    }
    let offset: i32 = args
        .iter()
        .position(|a| a == "--offset")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let root = assets_root();
    let sc = scenes::Scenes::load(&root.join("characters").join(&character))?;
    if args.iter().any(|a| a == "--scenes") {
        let mut names: Vec<&String> = sc.scenes.keys().collect();
        names.sort();
        for n in names {
            println!("{n}");
        }
        return Ok(());
    }
    let scene_loop = args
        .iter()
        .position(|a| a == "--scene")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let (fw0, fh0) = sc.frame_size();
    let sc = Arc::new(sc);
    let font = FontArc::try_from_vec(std::fs::read(root.join("fonts/LiberationSans-Regular.ttf"))?)?;
    let mut pet = Pet::new(sc.clone(), cfg.adult);
    pet.interstitial = cfg.interstitial.clone();
    if let Some(s) = &scene_loop {
        if !pet.play_named(s) {
            eprintln!("no scene matching {s:?}; see --scenes");
        } else {
            pet.scene_loop = Some(s.clone());
        }
    }
    let mut agent_cfg = agent::AgentCfg {
        endpoint: cfg.agent_endpoint.clone(),
        key: cfg.agent_key.clone(),
        model: cfg.agent_model.clone(),
    };
    // Env overrides win: handy for one-off `DESKMATES_AGENT_ENDPOINT=...` runs.
    if let Ok(ep) = std::env::var("DESKMATES_AGENT_ENDPOINT") {
        if !ep.trim().is_empty() {
            agent_cfg.endpoint = ep;
        }
    }
    if let Ok(k) = std::env::var("DESKMATES_AGENT_KEY") {
        if !k.trim().is_empty() {
            agent_cfg.key = k;
        }
    }
    let (agent_tx, agent_rx) = std::sync::mpsc::channel();

    let mut app = App {
        pet,
        scenes: sc,
        font,
        scale: cfg.scale,
        fw: fw0,
        fh: fh0,
        cur_dims: (fw0, fh0),
        matte,
        audio: crate::audio::Audio::new(
            root.join("characters").join(&character).join("sounds"),
        ),
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        shaper: xshape::Shaper::new(),
        character,
        display,
        pixels: None,
        agent: agent_cfg,
        agent_tx,
        agent_rx,
        bubble: None,
        ask_mtime: ask_mtime(),
        cfg_mtime: cfg_mtime(),
        walk_enabled: cfg.walk,
        user_idle: true,
        last_idle_query: Instant::now(),
        window: None,
        size: (1, 1),
        mouse: (0.0, 0.0),
        press: None,
        menu_open: false,
        menu_rects: Vec::new(),
        about_until: None,
        tray: None,
        last_move: Instant::now(),
        walk_x: 0.0,
        preloaded: String::new(),
        hidden_by_blank: false,
        fence_rest: None,
        off_since: None,
        drew_content: true,
        blank_hide_after: None,
        move_mode: None,
    };

    // winit 0.30 picks the native Wayland backend when WAYLAND_DISPLAY is
    // set, and xdg-shell forbids client self-move — walking would do
    // nothing on a Wayland desktop. Force the X11 backend so we run under
    // XWayland, where programmatic window moves work (the window stays
    // WM-managed there; the override-redirect gate in the Resumed handler
    // only fires on pure-X11 sessions).
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    let el = {
        use winit::platform::x11::EventLoopBuilderExtX11;
        let mut builder = EventLoop::builder();
        builder.with_x11();
        builder.build()?
    };
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    let el = EventLoop::new()?;
    el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(16)));
    #[allow(deprecated)]
    let run_result = el.run(move |event, el| {
        // pump tray events (menu clicks)
        #[cfg(target_os = "linux")]
        if app.tray.is_some() {
            while gtk::events_pending() {
                gtk::main_iteration();
            }
        }
        while let Ok(ev) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            match ev.id.0 {
                ref id if id == "quit" => el.exit(),
                ref id if id == "toggle" => {
                    if let Some(w) = &app.window {
                        if w.is_visible().unwrap_or(true) {
                            w.set_visible(false);
                        } else {
                            w.set_visible(true);
                            w.set_window_level(WindowLevel::AlwaysOnTop);
                        }
                        // Manual choice wins over the blank-gap auto-hide.
                        app.hidden_by_blank = false;
                        app.blank_hide_after = Some(Instant::now() + Duration::from_secs(4));
                    }
                }
                ref id if id == "move" => {
                    // Tray "Move": the window follows the global pointer
                    // until the next click lands it (see AboutToWait).
                    app.move_mode = Some(Instant::now());
                    if let Some(w) = &app.window {
                        w.set_visible(true);
                        w.set_window_level(WindowLevel::AlwaysOnTop);
                        app.hidden_by_blank = false;
                        app.blank_hide_after = Some(Instant::now() + Duration::from_secs(4));
                    }
                }
                ref id if id == "touch" => app.pet.touch(),
                ref id if id == "ask" => {
                    if app.agent.enabled() {
                        fork_child("--ask-ui");
                    } else {
                        app.speak("I can chat, but no AI endpoint is set. Open Settings from the tray to add one!");
                    }
                }
                ref id if id == "next" => app.pet.next_scene(),
                ref id if id == "settings" => fork_child("--settings-ui"),
                ref id if id == "about" => {
                    app.about_until = Some(Instant::now() + Duration::from_secs(8));
                    if let Some(w) = &app.window {
                        w.set_visible(true);
                        w.set_window_level(WindowLevel::AlwaysOnTop);
                        app.hidden_by_blank = false;
                    }
                }
                ref id if id.starts_with("char:") => app.switch_character(&id[5..]),
                _ => {}
            }
        }
        while tray_icon::TrayIconEvent::receiver().try_recv().is_ok() {}

        match event {
            Event::Resumed if app.window.is_none() => {
                #[cfg_attr(not(windows), allow(unused_mut))]
                let mut attrs = WindowAttributes::default()
                    .with_title(format!("{} the Deskmate", app.display))
                    .with_decorations(false)
                    .with_transparent(!matte)
                    .with_resizable(false)
                    .with_inner_size(LogicalSize::new(app.win_logical().0 as f64, app.win_logical().1 as f64));
                #[cfg(windows)]
                {
                    use winit::platform::windows::WindowAttributesExtWindows;
                    attrs = attrs.with_skip_taskbar(true);
                }
                // Pure X11 (no Wayland): override-redirect = no WM, no
                // taskbar, always ours. Under XWayland the window stays
                // WM-managed so programmatic moves go through the WM.
                #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                {
                    use winit::platform::x11::WindowAttributesExtX11;
                    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
                        attrs = attrs.with_override_redirect(true);
                    }
                }
                let Ok(window) = el.create_window(attrs) else {
                    eprintln!("could not create window");
                    el.exit();
                    return;
                };
                window.set_window_level(WindowLevel::AlwaysOnTop);
                let (ax, ay, aw, ah) = window
                    .current_monitor()
                    .or_else(|| window.primary_monitor())
                    .map(|m| work_area(&m))
                    .unwrap_or((0, 0, 1920, 1080));
                let sf0 = window.scale_factor();
                let (ww, wh) = { let (a, b) = app.win_logical(); ((a as f64 * sf0) as i32, (b as f64 * sf0) as i32) };
                // Deskmates live at the bottom edge of the work area
                // (`fence_home` = work area inset 1, PLAYER_RE §2).
                let pos = match (cfg.x, cfg.y) {
                    (Some(x), Some(y)) if x >= ax - ww && x <= ax + aw && y >= ay && y <= ay + ah => (x, y),
                    _ => ((ax + aw / 2 - ww / 2 + offset * (ww + 24)).min(ax + aw - ww), ay + ah - wh),
                };
                let pos = clamp_rest(pos.0, pos.1, ww, wh, (ax, ay, aw, ah));
                window.set_outer_position(Position::Physical(PhysicalPosition::new(pos.0, pos.1)));
                app.walk_x = pos.0 as f64;
                let window = Arc::new(window);
                if let Ok(img) = image::open(assets_root().join("tray.png")) {
                    let img = img.to_rgba8();
                    let (iw, ih) = img.dimensions();
                    let _ = window.set_window_icon(Icon::from_rgba(img.into_raw(), iw, ih).ok());
                }
                if app.tray.is_none() {
                    app.tray = make_tray(&app.display);
                    if app.tray.is_none() {
                        eprintln!("note: system tray unavailable (Linux: install libappindicator)");
                    }
                }
                let size = window.inner_size();
                let (sw, sh) = (size.width.max(1), size.height.max(1));
                let build = |alpha: Option<pixels::wgpu::CompositeAlphaMode>| {
                    let st = pixels::SurfaceTexture::new(sw, sh, window.clone());
                    let mut b = pixels::PixelsBuilder::new(sw, sh, st)
                        .clear_color(pixels::wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 });
                    if let Some(a) = alpha {
                        b = b.alpha_mode(a);
                    }
                    b.build()
                };
                let px = match build(Some(pixels::wgpu::CompositeAlphaMode::PreMultiplied)) {
                    Ok(px) => px,
                    Err(e1) => match build(None) {
                        Ok(px) => px,
                        Err(e2) => {
                            eprintln!("renderer: {e1} / {e2}");
                            el.exit();
                            return;
                        }
                    },
                };
                app.size = (sw, sh);
                app.pixels = Some(px);
                app.window = Some(window);
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::Resized(size) => {
                    let (nw, nh) = (size.width.max(1), size.height.max(1));
                    if let Some(px) = &mut app.pixels {
                        let _ = px.resize_surface(nw, nh);
                        let _ = px.resize_buffer(nw, nh);
                    }
                    app.size = (nw, nh);
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    let dy = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y as f64,
                        MouseScrollDelta::PixelDelta(p) => p.y / 60.0,
                    };
                    if dy != 0.0 {
                        let next = (app.scale * (1.0 + 0.1 * dy.signum())).clamp(0.5, 4.0);
                        if (next - app.scale).abs() > f64::EPSILON {
                            app.scale = next;
                            save_state(app.window.as_deref(), next, &app.character);
                            if let Some(w) = &app.window {
                                let (lw, lh) = app.win_logical();
                                anchored_resize(w, lw, lh);
                            }
                        }
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    app.mouse = (position.x, position.y);
                    if let Some(mut pr) = app.press.take() {
                        if !pr.dragging
                            && (position.x - pr.start.0).abs() + (position.y - pr.start.1).abs() > 6.0
                        {
                            pr.dragging = true;
                            #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
                            if let Some(w) = &app.window {
                                let _ = w.set_cursor_grab(CursorGrabMode::Confined);
                            }
                        }
                        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
                        if pr.dragging {
                            if let Some(w) = &app.window {
                                let dx = position.x - pr.last.0;
                                let dy = position.y - pr.last.1;
                                if dx != 0.0 || dy != 0.0 {
                                    let _ = w.set_outer_position(Position::Physical(
                                        PhysicalPosition::new(pr.win.0 + dx as i32, pr.win.1 + dy as i32),
                                    ));
                                    if let Ok(p) = w.outer_position() {
                                        // Cursor-relative coords shift by however far
                                        // the window actually moved; track that so the
                                        // next delta is pure pointer motion.
                                        pr.last = (
                                            position.x - (p.x - pr.win.0) as f64,
                                            position.y - (p.y - pr.win.1) as f64,
                                        );
                                        pr.win = (p.x, p.y);
                                        app.walk_x = p.x as f64;
                                    }
                                }
                            }
                        }
                        app.press = Some(pr);
                    }
                    if let Some(w) = &app.window {
                        w.request_redraw();
                    }
                }
                WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                    if app.move_mode.take().is_some() {
                        // Click lands the pet after a tray "Move".
                        app.fence_rest = None;
                        save_state(app.window.as_deref(), app.scale, &app.character);
                    } else {
                        let wp = app.window.as_ref().and_then(|w| w.outer_position().ok()).unwrap_or(PhysicalPosition::new(0, 0));
                        let sf = app.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
                        let gstart = app.shaper.query_pointer().map(|(x, y, _)| (x, y)).unwrap_or((0, 0));
                        app.press = Some(Press {
                            start: app.mouse,
                            last: app.mouse,
                            win: (wp.x, wp.y),
                            off: ((app.mouse.0 * sf) as i32, (app.mouse.1 * sf) as i32),
                            gstart,
                            dragging: false,
                        });
                    }
                }
                WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                    if let Some(pr) = app.press.take() {
                        if pr.dragging {
                            #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
                            if let Some(w) = &app.window {
                                let _ = w.set_cursor_grab(CursorGrabMode::None);
                            }
                            save_state(app.window.as_deref(), app.scale, &app.character);
                            // A drag re-homes the pet: the new spot is
                            // where fence transitions should return to.
                            app.fence_rest = None;
                        } else {
                            let (mx, my) = app.mouse;
                            if !app.menu_click(mx, my) {
                                if app.menu_open {
                                    app.menu_open = false;
                                } else {
                                    // Native sprite coords for the hotspot map.
                                    let sf = app.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
                                    let d = (app.scale * sf).max(0.01);
                                    app.pet.touch_at(mx / d, my / d);
                                }
                                if let Some(w) = &app.window {
                                    w.request_redraw();
                                }
                            }
                        }
                    }
                }
                WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                    app.menu_open = !app.menu_open;
                    if let Some(w) = &app.window {
                        w.request_redraw();
                    }
                }
                WindowEvent::CloseRequested => el.exit(),
                WindowEvent::RedrawRequested => app.render(),
                _ => {}
            },
            Event::UserEvent(_) => {}
            Event::AboutToWait => {
                let now = Instant::now();
                #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                app.shaper.ensure_skip_taskbar();
                // While the menu is open the pet must not walk or change
                // scenes: the menu is painted inside the pet window, and a
                // moving/resizing window makes menu clicks miss.
                let mut changed = if app.menu_open { false } else { app.pet.tick(now) };
                app.sync_scene_window();
                if let Some(snd) = app.pet.take_sound() {
                    app.audio.play(&snd);
                }
                if let Some(t) = app.pet.take_talk() {
                    app.speak(&t);
                }
                // X11 drag: follow the global pointer while button 1 is
                // held; a release anywhere (even far outside the tiny
                // window) ends the drag.
                #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                if let Some((off, gstart, dragging)) =
                    app.press.as_ref().map(|p| (p.off, p.gstart, p.dragging))
                {
                    match app.shaper.query_pointer() {
                        Some((gx, gy, true)) => {
                            if dragging {
                                if let Some(w) = &app.window {
                                    let _ = w.set_outer_position(Position::Physical(
                                        PhysicalPosition::new(gx - off.0, gy - off.1),
                                    ));
                                    if let Ok(p) = w.outer_position() {
                                        app.walk_x = p.x as f64;
                                    }
                                }
                            } else if (gx - gstart.0).abs() + (gy - gstart.1).abs() > 6 {
                                // The pointer warped straight out of the
                                // window: winit stops sending CursorMoved,
                                // so the drag must start from the polled
                                // global position, not window-relative
                                // motion.
                                if let Some(p) = &mut app.press {
                                    p.dragging = true;
                                }
                            }
                        }
                        Some((_, _, false)) | None => {
                            if dragging {
                                app.press = None;
                                save_state(app.window.as_deref(), app.scale, &app.character);
                                app.fence_rest = None;
                            }
                        }
                    }
                }
                // Tray "Move": the window follows the global pointer until
                // the next click lands it. Reuses the drag infrastructure:
                // the same Shaper::query_pointer polling as left-drag.
                #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                if let Some(armed) = app.move_mode {
                    match app.shaper.query_pointer() {
                        Some((_, _, true)) if now.duration_since(armed) > Duration::from_millis(250) => {
                            // The next click lands the pet: stop following
                            // and re-home the fence anchor to the new spot.
                            app.move_mode = None;
                            app.fence_rest = None;
                            save_state(app.window.as_deref(), app.scale, &app.character);
                        }
                        Some((gx, gy, false)) => {
                            if let Some(w) = &app.window {
                                let (ww, wh) = {
                                    let s = w.outer_size();
                                    (s.width as i32, s.height as i32)
                                };
                                let (nx, ny) = (gx - ww / 2, gy - wh / 2);
                                let _ = w.set_outer_position(Position::Physical(
                                    PhysicalPosition::new(nx, ny),
                                ));
                                app.walk_x = nx as f64;
                            }
                            changed = true;
                        }
                        _ => {}
                    }
                }
                #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
                if let Some(armed) = app.move_mode {
                    // Fallback: follow via the window-relative cursor
                    // position (only known while over the pet); a click on
                    // the pet lands it, the pointer leaving ends the move.
                    let follow = app.window.as_ref().and_then(|w| {
                        let c = w.cursor_position().ok()?;
                        let p = w.outer_position().ok()?;
                        let s = w.outer_size();
                        Some((
                            p.x + c.x as i32 - s.width as i32 / 2,
                            p.y + c.y as i32 - s.height as i32 / 2,
                        ))
                    });
                    match follow {
                        Some((nx, ny)) => {
                            if let Some(w) = &app.window {
                                let _ = w.set_outer_position(Position::Physical(
                                    PhysicalPosition::new(nx, ny),
                                ));
                                if let Ok(p) = w.outer_position() {
                                    app.walk_x = p.x as f64;
                                }
                            }
                            changed = true;
                        }
                        None if now.duration_since(armed) > Duration::from_millis(500) => {
                            app.move_mode = None;
                            app.fence_rest = None;
                            save_state(app.window.as_deref(), app.scale, &app.character);
                        }
                        None => {}
                    }
                }
                let dragging = app.press.as_ref().map_or(false, |p| p.dragging);
                // User-idle gate: no roaming while the user types/moves
                // (Sprite.exe measured time since last input the same way).
                if app.last_idle_query.elapsed() >= Duration::from_secs(1) {
                    app.last_idle_query = now;
                    app.user_idle = user_idle_secs().map(|s| s >= 5).unwrap_or(true);
                }
                if !app.menu_open
                    && !dragging
                    && app.move_mode.is_none()
                    && app.walk_enabled
                    && app.user_idle
                    && (app.pet.walk_dir() != 0 || app.pet.fence().is_some())
                {
                    app.apply_walk(now);
                    changed = true;
                }
                if app.move_mode.is_none() {
                    app.ensure_visible(now);
                }
                // Blank inter-scene gap: hide the window instead of showing
                // an empty (black without compositor) box — the original
                // deskmate simply left the screen between scenes.
                let blank = !app.drew_content
                    && app.bubble.is_none()
                    && !app.menu_open
                    && app.about_until.is_none();
                if let Some(w) = &app.window {
                    if blank && !app.hidden_by_blank {
                        let free = app.blank_hide_after.map_or(true, |t| now >= t);
                        if free {
                            w.set_visible(false);
                            app.hidden_by_blank = true;
                        }
                    } else if !blank && app.hidden_by_blank {
                        w.set_visible(true);
                        w.set_window_level(WindowLevel::AlwaysOnTop);
                        app.hidden_by_blank = false;
                    }
                }
                // Pre-decode the whole current scene off-thread, then drop
                // other scenes' frames: playback never decodes mid-frame
                // and memory stays bounded to the active scene.
                if let Some(s) = app.pet.cur_scene() {
                    if s != app.preloaded {
                        app.preloaded = s.clone();
                        let sc = app.pet.scenes_arc();
                        std::thread::spawn(move || {
                            sc.preload(&s);
                            sc.evict_except(&s);
                        });
                    }
                }
                // Agent replies arriving from the worker thread.
                while let Ok(agent::Msg::Reply(r)) = app.agent_rx.try_recv() {
                    match r {
                        Ok(text) => {
                            let (speech, dir) = agent::parse_stage(&text);
                            match dir.as_deref() {
                                Some("next") => app.pet.next_scene(),
                                Some(name) => {
                                    app.pet.play_named(name);
                                }
                                None => {}
                            }
                            app.speak(&speech);
                        }
                        Err(e) => app.speak(&format!("Ugh, {e}")),
                    }
                    changed = true;
                }
                // Question handed over by the forked Ask window?
                if let Some(mt) = ask_mtime() {
                    if app.ask_mtime != Some(mt) {
                        app.ask_mtime = Some(mt);
                        let q = std::fs::read_to_string(ask_path()).unwrap_or_default();
                        let _ = std::fs::write(ask_path(), "");
                        let q = q.trim().to_string();
                        if !q.is_empty() && app.agent.enabled() {
                            app.speak("Hmm, let me think…");
                            agent::send(app.agent.clone(), q, app.agent_tx.clone());
                        }
                        changed = true;
                    }
                }
                // Settings child saved new agent config? Reload.
                if let Some(mt) = cfg_mtime() {
                    if app.cfg_mtime != Some(mt) {
                        app.cfg_mtime = Some(mt);
                        let c = load_config();
                        app.agent.endpoint = c.agent_endpoint;
                        app.agent.key = c.agent_key;
                        app.agent.model = c.agent_model;
                        app.pet.set_adult(c.adult);
                        app.walk_enabled = c.walk;
                    }
                }
                // Bubble expiry needs one final redraw to clear.
                if app.bubble.is_some() {
                    changed = true;
                }
                if changed || app.about_until.is_some() {
                    if let Some(w) = &app.window {
                        w.request_redraw();
                    }
                }
                el.set_control_flow(ControlFlow::WaitUntil(now + Duration::from_millis(33)));
            }
            _ => {}
        }
    });
    run_result?;
    Ok(())
}

#[cfg(test)]
mod walk_tests {
    use super::clamp_rest;

    /// Work area like a 1920x1080 screen with a 40 px taskbar.
    const WA: (i32, i32, i32, i32) = (0, 0, 1920, 1040);

    #[test]
    fn rest_inside_work_area_untouched() {
        assert_eq!(clamp_rest(500, 300, 200, 400, WA), (500, 300));
    }

    #[test]
    fn rest_clamped_into_work_area() {
        // Hanging off the right/bottom edges (still >= 2/3 visible at the
        // clamped point): pulled fully inside the work area.
        assert_eq!(clamp_rest(1850, 900, 200, 400, WA), (1720, 640));
    }

    #[test]
    fn rest_recentered_when_mostly_invisible() {
        // Sprite wider/taller than the work area: even clamped, less than
        // 2/3 of it is visible -> recenter in the work area (§2).
        assert_eq!(clamp_rest(0, 0, 3000, 3000, WA), (-540, -980));
    }
}

/// Seconds since the user last touched mouse/keyboard, via the X11
/// MIT-SCREEN-SAVER extension (plain X11 and XWayland sessions). `None`
/// where no query exists (native Wayland, Windows, macOS): roaming then
/// depends only on the settings toggle.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn user_idle_secs() -> Option<u64> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::screensaver::ConnectionExt as _;
    thread_local! {
        static SS_CONN: Option<(x11rb::rust_connection::RustConnection, usize)> =
            x11rb::connect(None).ok();
    }
    SS_CONN.with(|c| {
        let (conn, screen) = c.as_ref()?;
        let root = conn.setup().roots[*screen].root;
        let info = conn.screensaver_query_info(root).ok()?.reply().ok()?;
        Some(info.ms_since_user_input as u64 / 1000)
    })
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn user_idle_secs() -> Option<u64> {
    None
}

#[cfg(test)]
mod blit_tests {
    use super::*;

    #[test]
    fn upscale_smooths_alpha_edge() {
        // Opaque left half, transparent right half; at 2x the destination
        // column straddling the source boundary must get partial alpha
        // (bilinear), where nearest-neighbor would give a hard cliff.
        let mut img = RgbaImage::new(4, 1);
        for px in 0..2 {
            img.put_pixel(px, 0, image::Rgba([255, 255, 255, 255]));
        }
        let mut canvas = vec![0u8; 8 * 4];
        blit(&mut canvas, 8, 1, &img, 0, 0, 2.0);
        let alphas: Vec<u8> = (0..8).map(|px| canvas[px * 4 + 3]).collect();
        // dest col 3 straddles src px1/px2 (w .75/.25) -> alpha 191;
        // col 4 straddles with .25/.75 -> 64; nearest-neighbor would be
        // [255,255,255,255,0,0,0,0] instead.
        assert_eq!(alphas[2], 255, "interior stays fully opaque: {alphas:?}");
        assert!(alphas[3] > 0 && alphas[3] < 255, "expected soft edge: {alphas:?}");
        assert!(alphas[4] > 0 && alphas[4] < 255, "expected soft edge: {alphas:?}");
        assert_eq!(alphas[5], 0, "outside the fade untouched: {alphas:?}");
    }

    #[test]
    fn scale_one_is_pixel_exact() {
        let img = RgbaImage::from_pixel(2, 1, image::Rgba([10, 20, 30, 255]));
        let mut canvas = vec![0u8; 4 * 4];
        blit(&mut canvas, 4, 1, &img, 0, 0, 1.0);
        assert_eq!(&canvas[0..4], &[10, 20, 30, 255]);
        assert_eq!(&canvas[4..8], &[10, 20, 30, 255]);
        assert_eq!(&canvas[8..12], &[0, 0, 0, 0]);
    }

    #[test]
    fn transparent_pixels_do_not_bleed_color() {
        // Opaque red next to black-transparent: the straddling destination
        // pixel must be red (premultiplied interp), not a darkened red.
        let mut img = RgbaImage::new(2, 1);
        img.put_pixel(0, 0, image::Rgba([200, 0, 0, 255]));
        img.put_pixel(1, 0, image::Rgba([0, 0, 0, 0]));
        let mut canvas = vec![0u8; 4 * 4];
        blit(&mut canvas, 4, 1, &img, 0, 0, 2.0);
        // dx=1 straddles: taps are red (w .75) and transparent (w .25) ->
        // a=191, premultiplied red stored as 200*191/255 = 149. Naive
        // non-premultiplied interp would store 150*191/255 = 112 (bleed).
        let o = 1 * 4;
        assert!(canvas[o] >= 140, "red must not darken by bleed: {:?}", &canvas[o..o + 4]);
    }
}
