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
mod classic;
mod faz;
mod pet;
mod retro;
mod review_ui;
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

/// The original Manager.exe launched a "Visit <Character>" item per
/// character with `ShellExecuteA`, straight at that character's own period
/// product site — the URLs below are the ones decoded out of the shipped
/// .mnu files (Visit Tahni → tahni.com/deskmate.php, Visit FatB →
/// fatb.oska.com/deskmate.php, ...). Those sites died with Oska, so each
/// entry is the earliest good Wayback capture of the original URL. Handing
/// the URL to the OS browser is the original's behavior; this binary still
/// contains no network code of its own.
pub const VISIT_SITES: &[(&str, &str)] = &[
    ("tahni", "https://web.archive.org/web/20011204210101/http://www.tahni.com/deskmate.php"),
    ("fatb", "https://web.archive.org/web/20011117191049/http://fatb.oska.com/deskmate.php"),
    ("maeka", "https://web.archive.org/web/20030207101939/http://www.maeka.com/deskmate.php"),
    ("kahli", "https://web.archive.org/web/20011214204657/http://www.kahli.com/deskmate.php"),
    ("teecee", "https://web.archive.org/web/20011118094820/http://www.teecee.net/deskmate.php"),
    ("oska", "https://web.archive.org/web/20011117185101/http://www.oska.com/deskmate.php"),
];

/// The archived product page for one character, if we ship one for it.
fn visit_url(character: &str) -> Option<&'static str> {
    VISIT_SITES.iter().find(|(c, _)| *c == character).map(|(_, u)| *u)
}

/// Hand a URL to the OS default browser — the original's `ShellExecuteA`
/// path. Fire-and-forget: no network code here, and a missing handler must
/// never take the pet down.
fn open_in_browser(url: &str) {
    let spawned = {
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("xdg-open").arg(url).spawn()
        }
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open").arg(url).spawn()
        }
        #[cfg(windows)]
        {
            std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "no browser launcher on this platform",
            ))
        }
    };
    if let Err(e) = spawned {
        eprintln!("could not open {url} in the browser: {e}");
    }
}

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
    /// With `adult` on, each random scene draw takes an adults-only scene
    /// with 60% probability when one is available (settings checkbox).
    #[serde(default)]
    pub prefer_adult: bool,
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
        Config { scale: 1.0, x: None, y: None, character: "tahni".into(), agent_endpoint: String::new(), agent_key: String::new(), agent_model: String::new(), adult: false, prefer_adult: false, interstitial: Vec::new(), walk: true }
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

/// Play handoff file written by the forked scene-browser window:
/// `scene\nentry` (empty entry = whole scene) or `switch:<character>`,
/// mtime-polled by the pet loop exactly like the ask file.
pub fn play_path() -> std::path::PathBuf {
    config_path()
        .map(|p| p.with_extension("play"))
        .unwrap_or_else(|| std::path::PathBuf::from("deskmates-rs.play"))
}

fn play_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(play_path()).ok().and_then(|m| m.modified().ok())
}

/// Mark-export file written by the scene browser's Export button: the
/// names the user wants gated, one per line (also printed to stdout).
pub fn marks_path() -> std::path::PathBuf {
    config_path()
        .map(|p| p.with_extension("marks"))
        .unwrap_or_else(|| std::path::PathBuf::from("deskmates-rs.marks"))
}

fn cfg_mtime() -> Option<std::time::SystemTime> {
    config_path().and_then(|p| std::fs::metadata(p).ok().and_then(|m| m.modified().ok()))
}

/// Fork a WM-managed child window (Ask / Settings / scene browser): the
/// pet window is override-redirect and never gets keyboard focus; the
/// child does. Returns false when the child could not be spawned.
fn fork_child(args: &[&str]) -> bool {
    if let Ok(exe) = std::env::current_exe() {
        return std::process::Command::new(exe).args(args).spawn().is_ok();
    }
    false
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
pub(crate) const CHARACTERS: &[&str] = &["tahni", "fatb", "maeka", "kahli", "teecee", "oska"];

pub(crate) fn display_name(character: &str) -> String {
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

fn make_tray(
    display: &str,
    character: &str,
    adult: bool,
) -> Option<(tray_icon::TrayIcon, Vec<(String, tray_icon::menu::MenuItem)>)> {
    use tray_icon::menu::{Menu, MenuItem, Submenu};
    use tray_icon::TrayIconBuilder;
    #[cfg(target_os = "linux")]
    {
        if let Err(e) = gtk::init() {
            eprintln!("tray: gtk init failed: {e}");
            return None;
        }
    }
    let img = match image::open(assets_root().join("tray.png")) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            eprintln!("tray: tray.png open failed at {}: {e}", assets_root().join("tray.png").display());
            return None;
        }
    };
    let (tw, th) = (img.width(), img.height());
    let icon = match tray_icon::Icon::from_rgba(img.into_raw(), tw, th) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("tray: icon from_rgba failed: {e}");
            return None;
        }
    };
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
    let visit = MenuItem::with_id("visit", format!("Visit {display}"), true, None);
    let chars = Submenu::new("Character", true);
    for c in CHARACTERS {
        let item = MenuItem::with_id(format!("char:{c}"), display_name(c), true, None);
        chars.append_items(&[&item]).ok();
    }
    // Classic submenu: this character's early-set scenes (no classic set
    // = no submenu). Adults-gated entries start disabled; the handles are
    // kept so the gate can flip live when the config reloads.
    let classic = Submenu::new("Classic", true);
    let mut classic_items: Vec<(String, tray_icon::menu::MenuItem)> = Vec::new();
    for cs in crate::classic::classic_set(character) {
        let item = MenuItem::with_id(
            format!("classic:{}", cs.scene),
            cs.label,
            adult || !crate::adult::is_adult(cs.scene),
            None,
        );
        classic_items.push((cs.scene.to_string(), item.clone()));
        classic.append(&item).ok();
    }
    let quit = MenuItem::with_id("quit", "Quit", true, None);
    menu.append_items(&[&toggle, &mv, &touch, &ask, &next, &chars, &settings, &about, &visit, &quit]).ok()?;
    if !classic_items.is_empty() {
        menu.append(&classic).ok()?;
    }
    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(format!("{display} the Deskmate"))
        .with_icon(icon)
        .build()
        .map_err(|e| eprintln!("tray: build failed: {e} (Plasma: check tray Settings > Entries - new items can be hidden by default)")).ok()
        .map(|t| (t, classic_items))
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
    // `{CHAR}` is substituted with the live character's display name when the
    // overlay is drawn (see `render`), so one row serves all six characters.
    MenuItem { label: "Visit {CHAR}" },
    MenuItem { label: "Quit" },
];

/// Which row of the in-window menu overlay a click landed on; the
/// classic submenu replaces the main list while open.
#[derive(Clone, Copy, PartialEq)]
enum MenuRow {
    /// Index into MENU.
    Item(usize),
    /// "Classic" opener row (only present when the character has one).
    OpenClassic,
    /// Index into the current classic list.
    ClassicItem(usize),
    /// "Back" row of the classic submenu.
    ClassicBack,
}

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
    /// The classic submenu is open in place of the main menu list.
    classic_open: bool,
    menu_rects: Vec<(MenuRow, (i32, i32, i32, i32))>,
    about_until: Option<Instant>,
    agent: agent::AgentCfg,
    agent_tx: std::sync::mpsc::Sender<agent::Msg>,
    agent_rx: std::sync::mpsc::Receiver<agent::Msg>,
    /// Speech bubble: text shown until the given instant.
    bubble: Option<(String, Instant)>,
    ask_mtime: Option<std::time::SystemTime>,
    /// Play-request handoff mtime from the forked scene browser.
    play_mtime: Option<std::time::SystemTime>,
    cfg_mtime: Option<std::time::SystemTime>,
    /// Config toggle: pet may roam (walk/fence scenes move the window).
    walk_enabled: bool,
    /// Current adults-only gate (mirrors config; drives the classic
    /// submenu visibility and the tray item enabled state).
    adult: bool,
    /// Scene review mode (--review): (sorted scene names, current index).
    review: Option<(Vec<String>, usize)>,
    /// User-idle state (MIT-SCREEN-SAVER): the original gated its actions
    /// on user inactivity; we gate roaming on it too.
    user_idle: bool,
    last_idle_query: Instant,
    tray: Option<tray_icon::TrayIcon>,
    /// Handles to the tray Classic submenu items (scene name, item) so
    /// the adult gate can enable/disable them live.
    tray_classic: Vec<(String, tray_icon::menu::MenuItem)>,
    last_move: Instant,
    /// Fractional window x for smooth walking.
    walk_x: f64,
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
    /// Reused RGBA canvas buffer (render() must not reallocate per frame).
    canvas: Vec<u8>,
    /// Visible-state fingerprint of the last presented frame (see
    /// `render`): identical redraw requests skip canvas + present.
    last_present: Option<PresentKey>,
    /// Next render must present even if the fingerprint matches (window
    /// contents may be lost: unhide, resize, occlusion, first map).
    force_present: bool,
    /// When the last frame was actually presented; a picture older than
    /// `PAINT_REFRESH` is repainted even if unchanged (bounds a lost
    /// Expose, see `SELF_REDRAW`).
    painted_at: Option<Instant>,
}

/// Fingerprint of everything `render` paints; equal keys => identical
/// pixels, so the redraw can be skipped entirely.
#[derive(PartialEq)]
struct PresentKey {
    sprite: Option<(String, u32, i32, i32)>,
    w: u32,
    h: u32,
    scale: f64,
    sf: f64,
    bubble: Option<String>,
    menu_open: bool,
    classic_open: bool,
    about: bool,
    matte: bool,
    character: String,
}

/// Set by every `request_redraw` we issue ourselves. A RedrawRequested
/// that arrives without it was generated by the X server (Expose after
/// uncover/first map): the window contents may be gone, so the dirty
/// check in `render` must not skip the present.
static SELF_REDRAW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn request_paint(w: &winit::window::Window) {
    SELF_REDRAW.store(true, std::sync::atomic::Ordering::Relaxed);
    w.request_redraw();
}

/// How long an unchanged picture may stay on screen before it is repainted
/// anyway. Bounds the worst case of a lost Expose (see `SELF_REDRAW`):
/// 10 s is far below "user notices a blank pet" and costs one present
/// per 10 s (~0.05% of a core).
const PAINT_REFRESH: Duration = Duration::from_secs(10);
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

/// --review stepping: play scene idx+dir (wrapping), announce the name in
/// stdout, the window title, and the speech bubble.
fn review_step(app: &mut App, dir: i32) {
    let Some((names, idx)) = &mut app.review else { return };
    let n = names.len();
    *idx = (*idx as i32 + dir).rem_euclid(n as i32) as usize;
    let name = names[*idx].clone();
    app.pet.play_named(&name);
    app.pet.scene_loop = Some(name.clone());
    println!("[review {}/{}] {name}", *idx + 1, n);
    if let Some(w) = &app.window {
        let _ = w.set_title(&format!("{}/{} {name}", *idx + 1, n));
    }
    app.bubble = Some((name, Instant::now() + Duration::from_secs(3600)));
}

impl App {
    fn win_logical(&self) -> (u32, u32) {
        // Zoom tracks the CURRENT scene canvas (cur_dims, kept in sync by
        // sync_scene_window), not the modal frame size — scenes with
        // off-modal canvases otherwise clip or skew when zoomed.
        ((self.cur_dims.0 as f64 * self.scale) as u32, (self.cur_dims.1 as f64 * self.scale) as u32)
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
        request_paint(&w);
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
        let cfg = load_config();
        let adult = cfg.adult;
        self.adult = adult;
        self.pet = Pet::new(self.scenes.clone(), adult);
        self.pet.set_prefer_adult(cfg.prefer_adult);
        // Rebind sound playback to the new character's sounds dir. The
        // WAV cache lives inside Audio keyed by bare name, so rebuilding
        // it here also stops same-named waves (LAND, SWOOSH_A, …) from
        // leaking across characters after a switch.
        self.audio = crate::audio::Audio::new(
            assets_root().join("characters").join(name).join("sounds"),
        );
        self.bubble = None;
        self.menu_open = false;
        self.classic_open = false;
        if let Some(w) = &self.window {
            let (lw, lh) = self.win_logical();
            anchored_resize(w, lw, lh);
            let _ = w.set_title(&format!("{} the Deskmate", self.display));
            request_paint(w);
        }
        save_state(self.window.as_deref(), self.scale, name);
        // Rebuilding the tray (drop + re-register) makes the icon vanish on
        // Linux status-notifier buses; keep it and just update the tooltip.
        match &self.tray {
            Some(t) => {
                let _ = t.set_tooltip(Some(format!("{} the Deskmate", self.display)));
            }
            None => {
                if let Some((t, items)) = make_tray(&self.display, &self.character, self.adult) {
                    self.tray = Some(t);
                    self.tray_classic = items;
                }
            }
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

    /// `expose` = the X server asked for this repaint (window contents may
    /// be gone); never skip the present then.
    fn render(&mut self, expose: bool) {
        // Computed before the pixels borrow: classic_list borrows self.
        let classic = self.classic_list();
        let Some(px) = &mut self.pixels else { return };
        let (w, h) = self.size;
        let sf = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
        // Identical visible state and nothing that could have destroyed the
        // window's contents => the previous present is still on screen;
        // skip canvas rebuild, premultiply, put_image and the swapchain.
        // (Walking moves the *window*, not the sprite: the canvas pattern
        // is constant while walking, so every walk tick used to re-present
        // byte-identical pixels through llvmpipe.)
        let key = PresentKey {
            sprite: self.pet.sprite().map(|s| (s.scene.clone(), s.id, s.x, s.y)),
            w,
            h,
            scale: self.scale,
            sf,
            bubble: self.bubble.as_ref().map(|(t, _)| t.clone()),
            menu_open: self.menu_open,
            classic_open: self.classic_open,
            about: self.about_until.is_some(),
            matte: self.matte,
            character: self.character.clone(),
        };
        let stale = self.painted_at.is_none_or(|t| t.elapsed() >= PAINT_REFRESH);
        if !expose && !self.force_present && !stale && self.last_present.as_ref() == Some(&key) {
            return;
        }
        self.force_present = false;
        self.last_present = Some(key);
        self.painted_at = Some(Instant::now());
        let mut canvas = std::mem::take(&mut self.canvas);
        canvas.clear();
        canvas.resize((w as usize) * (h as usize) * 4, 0);
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
        if let Some((text, _until)) = self.bubble.clone() {
            {
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
        // menu overlay: main list, or the classic submenu in its place.
        self.menu_rects.clear();
        if self.menu_open {
            let s = (sf * self.scale.max(1.0)) as f32;
            let rows: Vec<(MenuRow, String)> = if self.classic_open {
                let mut r: Vec<(MenuRow, String)> = classic
                    .iter()
                    .enumerate()
                    .map(|(i, c)| (MenuRow::ClassicItem(i), c.label.to_string()))
                    .collect();
                r.push((MenuRow::ClassicBack, "Back".to_string()));
                r
            } else {
                let mut r: Vec<(MenuRow, String)> = MENU
                    .iter()
                    .enumerate()
                    .map(|(i, item)| (MenuRow::Item(i), item.label.replace("{CHAR}", &self.display)))
                    .collect();
                if !classic.is_empty() {
                    r.push((MenuRow::OpenClassic, "Classic".to_string()));
                }
                r
            };
            // The window IS the scene canvas: shrink rows so a long menu
            // (the classic submenu can run to a dozen entries) still fits.
            let bw = (130.0 * s) as i32;
            let fit = (h as f32 / rows.len().max(1) as f32) as i32;
            let bh = ((24.0 * s) as i32).min(fit.max((14.0 * s) as i32));
            let bx = (w as i32 - bw).max(0);
            let mut by = 0i32;
            for (row, label) in &rows {
                fill_rect(&mut canvas, w, h, bx, by, bw, bh, [245, 245, 250, 235]);
                stroke_rect(&mut canvas, w, h, bx, by, bw, bh, [120, 120, 130, 255]);
                let ts = (13.0 * s).min(bh as f32 * 0.62);
                draw_text(&mut canvas, w, h, &self.font, label, bx as f32 + 8.0 * s, by as f32 + bh as f32 * 0.68, PxScale::from(ts), [20, 20, 25, 255]);
                self.menu_rects.push((*row, (bx, by, bw, bh)));
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
        // A blank-hidden window is unmapped: presenting to it would still
        // rasterize and put_image the frame for nobody. The canvas alpha
        // scan above already updated drew_content, which is all the hidden
        // period needs (it drives the un-hide).
        if self.hidden_by_blank {
            self.canvas = canvas;
            return;
        }
        // RGBA canvas -> premultiplied RGBA frame buffer (pixels' format).
        {
            let buf = px.frame_mut();
            for (dst, src) in buf.chunks_exact_mut(4).zip(canvas.chunks_exact(4)) {
                let a = src[3] as usize;
                dst[0] = MUL255[a][src[0] as usize];
                dst[1] = MUL255[a][src[1] as usize];
                dst[2] = MUL255[a][src[2] as usize];
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
        self.canvas = canvas;
    }

    /// The classic submenu for the current character: classic-set scenes
    /// that are actually loaded, minus adults-only ones while the gate is
    /// off (the same rule that hides them from the random pools).
    fn classic_list(&self) -> Vec<&'static classic::ClassicScene> {
        classic::classic_set(&self.character)
            .iter()
            .filter(|cs| self.scenes.scenes.contains_key(cs.scene))
            .filter(|cs| self.adult || !crate::adult::is_adult(cs.scene))
            .collect()
    }

    fn menu_click(&mut self, x: f64, y: f64) -> bool {
        for (row, (mx, my, mw, mh)) in self.menu_rects.iter().rev() {
            if x >= *mx as f64 && x < (*mx + *mw) as f64 && y >= *my as f64 && y < (*my + *mh) as f64 {
                match *row {
                    MenuRow::OpenClassic => {
                        self.classic_open = true;
                        return true;
                    }
                    MenuRow::ClassicBack => {
                        self.classic_open = false;
                        return true;
                    }
                    MenuRow::ClassicItem(i) => {
                        self.menu_open = false;
                        self.classic_open = false;
                        if let Some(cs) = self.classic_list().get(i).copied() {
                            let scene = cs.scene.to_string();
                            if !self.pet.play_named(&scene) {
                                self.speak("Not right now — that scene is off.");
                            }
                        }
                        return true;
                    }
                    MenuRow::Item(i) => {
                        self.menu_open = false;
                        self.classic_open = false;
                        match MENU[i].label {
                            // Review mode: clicks must never hijack the scene
                            // under review with a touch reaction.
                            "Touch" if self.review.is_none() => self.pet.touch(),
                            "Ask…" => {
                                if self.agent.enabled() {
                                    fork_child(&["--ask-ui"]);
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
                            "Settings…" => { fork_child(&["--settings-ui"]); }
                            "Hide" => {
                                if let Some(w) = &self.window {
                                    w.set_visible(false);
                                    self.hidden_by_blank = false;
                                }
                            }
                            "About" => self.about_until = Some(Instant::now() + Duration::from_secs(8)),
                            "Visit {CHAR}" => {
                                if let Some(u) = visit_url(&self.character) {
                                    open_in_browser(u);
                                } else {
                                    self.speak("No old web page for me — I'm a new character!");
                                }
                            }
                            "Quit" => std::process::exit(0),
                            _ => {}
                        }
                        return true;
                    }
                }
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

/// `v * a / 255` rounded, as a 256x256 table (a-major). Source-over and
/// premultiplication run per-pixel every frame; the table keeps both hot
/// loops division-free.
static MUL255: [[u8; 256]; 256] = {
    let mut t = [[0u8; 256]; 256];
    let mut a = 0;
    while a < 256 {
        let mut v = 0;
        while v < 256 {
            t[a][v] = ((v * a + 127) / 255) as u8;
            v += 1;
        }
        a += 1;
    }
    t
};

/// Alpha-weighted bilinear sprite blit: pixel-center sampling is exact
/// nearest at scale == 1 and smooth above it; premultiplied taps keep
/// transparent pixels from bleeding their color, and edge taps clamp.
/// Integer fixed-point (10-bit weights) with per-column taps hoisted out
/// of the row loop; the inner loop is division-free.
fn blit(canvas: &mut [u8], cw: u32, ch: u32, img: &RgbaImage, x: i32, y: i32, scale: f64) {
    let (iw, ih) = (img.width() as usize, img.height() as usize);
    if iw == 0 || ih == 0 || cw == 0 || ch == 0 {
        return;
    }
    let src = img.as_raw();
    let dw = (iw as f64 * scale).max(0.0) as i32;
    let dh = (ih as f64 * scale).max(0.0) as i32;
    if dw == 0 || dh == 0 {
        return;
    }
    // Per-column source taps: (byte offset of x0, byte offset of x1, wx).
    let mut cols: Vec<(usize, usize, u32)> = Vec::with_capacity(dw as usize);
    for dx in 0..dw {
        let fx = (dx as f64 + 0.5) / scale - 0.5;
        let x0 = fx.floor() as i32;
        let wx = ((fx - x0 as f64) * 1024.0) as u32;
        let i0 = x0.clamp(0, iw as i32 - 1) as usize * 4;
        let i1 = (x0 + 1).clamp(0, iw as i32 - 1) as usize * 4;
        cols.push((i0, i1, wx));
    }
    for dy in 0..dh {
        let py = y + dy;
        if py < 0 || py >= ch as i32 {
            continue;
        }
        let fy = (dy as f64 + 0.5) / scale - 0.5;
        let y0 = fy.floor() as i32;
        let wy = ((fy - y0 as f64) * 1024.0) as u32;
        let wy1 = 1024 - wy;
        let r0 = y0.clamp(0, ih as i32 - 1) as usize * iw * 4;
        let r1 = (y0 + 1).clamp(0, ih as i32 - 1) as usize * iw * 4;
        let row = py as usize * cw as usize * 4;
        for (dx, &(c0, c1, wx)) in cols.iter().enumerate() {
            let px = x + dx as i32;
            if px < 0 || px >= cw as i32 {
                continue;
            }
            let wx1 = 1024 - wx;
            let i00 = r0 + c0;
            let i01 = r0 + c1;
            let i10 = r1 + c0;
            let i11 = r1 + c1;
            // Tap weights: alpha x 10-bit y-weight x 10-bit x-weight.
            let w00 = src[i00 + 3] as u32 * wy1 * wx1;
            let w01 = src[i01 + 3] as u32 * wy1 * wx;
            let w10 = src[i10 + 3] as u32 * wy * wx1;
            let w11 = src[i11 + 3] as u32 * wy * wx;
            let wa = w00 + w01 + w10 + w11;
            if wa == 0 {
                continue;
            }
            let mut wr = 0u64;
            let mut wg = 0u64;
            let mut wb = 0u64;
            for (w, i) in [(w00, i00), (w01, i01), (w10, i10), (w11, i11)] {
                if w == 0 {
                    continue;
                }
                wr += w as u64 * src[i] as u64;
                wg += w as u64 * src[i + 1] as u64;
                wb += w as u64 * src[i + 2] as u64;
            }
            // Weighted mean of taps: wr/wa lands in 0..=255.
            let inv = 1.0 / wa as f64;
            let r = (wr as f64 * inv) as u8;
            let g = (wg as f64 * inv) as u8;
            let b = (wb as f64 * inv) as u8;
            // Tap weights sum to 1024*1024, so wa/2^20 is the
            // interpolated alpha (0..255), rounded, min 1.
            let af = ((wa + (1 << 19)) >> 20).max(1) as usize;
            let o = row + px as usize * 4;
            let d = &mut canvas[o..o + 4];
            let ia = 255 - af;
            d[0] = MUL255[af][r as usize] + MUL255[ia][d[0] as usize];
            d[1] = MUL255[af][g as usize] + MUL255[ia][d[1] as usize];
            d[2] = MUL255[af][b as usize] + MUL255[ia][d[2] as usize];
            d[3] = d[3].max(af as u8);
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
    // Software GL (llvmpipe/lavapipe — what any GPU-less machine gets) wakes
    // a raster thread pool per present: 24 threads for a small pet window,
    // ~2x the total CPU of a single raster thread for the same frame. Mesa
    // reads this lazily when the driver initialises, so setting it before
    // the first window wins; hardware drivers ignore the variable. Forked
    // children inherit it.
    if std::env::var_os("LP_NUM_THREADS").is_none() {
        std::env::set_var("LP_NUM_THREADS", "1");
    }
    // Child modes: forked WM-managed windows (real keyboard focus).
    let argv: Vec<String> = std::env::args().collect();
    if argv.iter().any(|a| a == "--ask-ui") {
        return ask_ui::run(true).map(|_| ());
    }
    if argv.iter().any(|a| a == "--settings-ui") {
        return ask_ui::run(false).map(|_| ());
    }
    if argv.iter().any(|a| a == "--review-ui") {
        let character = argv
            .iter()
            .position(|a| a == "--character")
            .and_then(|i| argv.get(i + 1))
            .cloned()
            .unwrap_or_else(|| load_config().character);
        return review_ui::run(&character);
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
    let character = if CHARACTERS.contains(&character.as_str()) {
        character
    } else {
        eprintln!("character '{character}' is not shipped (Johlee moved to deskmates3d-rs); using default");
        "tahni".to_string()
    };
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
    pet.set_prefer_adult(cfg.prefer_adult);
    if let Some(s) = &scene_loop {
        if !pet.play_named(s) {
            eprintln!("no scene matching {s:?}; see --scenes");
        } else {
            pet.scene_loop = Some(s.clone());
        }
    }
    // --review: curate the scene set in the forked scene-browser window
    // (scrollable tree of scenes + their entries, click to play, per-row
    // gate marks, Export). The old keyboard stepping (Right/Space next,
    // Left back, Esc quit; name in stdout + title + bubble) stays wired
    // unchanged as the fallback whenever the browser cannot start. Shows
    // gated scenes too — that is the point of the review.
    let mut review: Option<(Vec<String>, usize)> = None;
    if args.iter().any(|a| a == "--review") {
        let mut names: Vec<String> = sc.scenes.keys().cloned().collect();
        names.sort();
        if names.is_empty() {
            eprintln!("--review: no scenes loaded");
        } else {
            pet.set_adult(true);
            pet.play_named(&names[0]);
            pet.scene_loop = Some(names[0].clone());
            println!("[review 1/{}] {}", names.len(), names[0]);
            review = Some((names, 0));
            if !fork_child(&["--review-ui", "--character", &character]) {
                eprintln!(
                    "--review: scene browser failed to start; keyboard stepping only (→/Space next, ← back)"
                );
            }
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
        play_mtime: play_mtime(),
        cfg_mtime: cfg_mtime(),
        walk_enabled: cfg.walk,
        adult: cfg.adult,
        user_idle: true,
        last_idle_query: Instant::now(),
        window: None,
        size: (1, 1),
        mouse: (0.0, 0.0),
        press: None,
        menu_open: false,
        classic_open: false,
        menu_rects: Vec::new(),
        about_until: None,
        tray: None,
        tray_classic: Vec::new(),
        last_move: Instant::now(),
        walk_x: 0.0,
        hidden_by_blank: false,
        fence_rest: None,
        off_since: None,
        drew_content: true,
        blank_hide_after: None,
        move_mode: None,
        canvas: Vec::new(),
        last_present: None,
        force_present: true,
        painted_at: None,
        review,
    };
    if let Some((names, _)) = &app.review {
        app.bubble = Some((names[0].clone(), Instant::now() + Duration::from_secs(3600)));
    }

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
                            // Re-map destroys window contents: must present.
                            app.force_present = true;
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
                        app.force_present = true;
                        app.hidden_by_blank = false;
                        app.blank_hide_after = Some(Instant::now() + Duration::from_secs(4));
                    }
                }
                ref id if id == "touch" && app.review.is_none() => app.pet.touch(),
                ref id if id == "ask" => {
                    if app.agent.enabled() {
                        fork_child(&["--ask-ui"]);
                    } else {
                        app.speak("I can chat, but no AI endpoint is set. Open Settings from the tray to add one!");
                    }
                }
                ref id if id == "next" => app.pet.next_scene(),
                ref id if id == "settings" => { fork_child(&["--settings-ui"]); }
                ref id if id == "about" => {
                    app.about_until = Some(Instant::now() + Duration::from_secs(8));
                    if let Some(w) = &app.window {
                        w.set_visible(true);
                        w.set_window_level(WindowLevel::AlwaysOnTop);
                        app.force_present = true;
                        app.hidden_by_blank = false;
                    }
                }
                ref id if id == "visit" => {
                    if let Some(u) = visit_url(&app.character) {
                        open_in_browser(u);
                    }
                }
                ref id if id.starts_with("char:") => app.switch_character(&id[5..]),
                ref id if id.starts_with("classic:") => {
                    let scene = id[8..].to_string();
                    if !app.pet.play_named(&scene) {
                        app.speak("Not right now — that scene is off.");
                    }
                }
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
                    if let Some((t, items)) = make_tray(&app.display, &app.character, app.adult) {
                        app.tray = Some(t);
                        app.tray_classic = items;
                    }
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
                WindowEvent::KeyboardInput { event: k, .. }
                    if k.state == ElementState::Pressed =>
                {
                    use winit::keyboard::{Key, NamedKey};
                    match k.logical_key {
                        Key::Named(NamedKey::Escape) => el.exit(),
                        Key::Named(NamedKey::ArrowRight) => review_step(&mut app, 1),
                        Key::Named(NamedKey::ArrowLeft) => review_step(&mut app, -1),
                        Key::Named(NamedKey::Space) => review_step(&mut app, 1),
                        _ => {}
                    }
                }
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
                        request_paint(w);
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
                                    if app.review.is_none() { app.pet.touch_at(mx / d, my / d); }
                                }
                                if let Some(w) = &app.window {
                                    request_paint(w);
                                }
                            }
                        }
                    }
                }
                WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                    app.menu_open = !app.menu_open;
                    app.classic_open = false;
                    if let Some(w) = &app.window {
                        request_paint(w);
                    }
                }
                WindowEvent::CloseRequested => el.exit(),
                WindowEvent::Occluded(false) => app.force_present = true,
                WindowEvent::RedrawRequested => {
                    // A redraw we did not ask for is the X server's Expose
                    // (uncovered / first map): the pixels on screen may be
                    // gone, so present even when the fingerprint is unchanged.
                    let expose = !SELF_REDRAW.swap(false, std::sync::atomic::Ordering::Relaxed);
                    app.render(expose);
                }
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
                        app.force_present = true;
                        app.hidden_by_blank = false;
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
                                None => {
                                    // No stage direction: repurpose the
                                    // original email-check scene if this
                                    // character ships one (kahli, maeka and
                                    // oska do; others no-op and keep the
                                    // current animation).
                                    app.pet.play_named("email");
                                }
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
                // Play request handed over by the forked scene browser?
                if let Some(mt) = play_mtime() {
                    if app.play_mtime != Some(mt) {
                        app.play_mtime = Some(mt);
                        let req = std::fs::read_to_string(play_path()).unwrap_or_default();
                        let _ = std::fs::write(play_path(), "");
                        let req = req.trim().to_string();
                        if let Some(name) = req.strip_prefix("switch:") {
                            app.switch_character(name.trim());
                        } else if let Some(scene) = req.lines().next().map(|l| l.trim().to_string()) {
                            if !scene.is_empty() {
                                let entry = req.lines().nth(1).unwrap_or("").trim().to_string();
                                let ok = if entry.is_empty() {
                                    app.pet.play_named(&scene)
                                } else {
                                    app.pet.play_pair(&scene, &entry)
                                };
                                if ok {
                                    // Pin the loop: nothing hijacks the scene under review.
                                    app.pet.scene_loop = Some(scene.clone());
                                    app.pet.scene_loop_entry =
                                        (!entry.is_empty()).then_some(entry.clone());
                                    let shown = if entry.is_empty() {
                                        scene
                                    } else {
                                        format!("{scene}/{entry}")
                                    };
                                    println!("[review] {shown}");
                                    app.bubble =
                                        Some((shown.clone(), Instant::now() + Duration::from_secs(3600)));
                                    if let Some(w) = &app.window {
                                        let _ = w.set_title(&shown);
                                    }
                                } else {
                                    eprintln!("[review] no such scene: {scene}");
                                }
                            }
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
                        app.pet.set_prefer_adult(c.prefer_adult);
                        app.adult = c.adult;
                        for (scene, item) in &app.tray_classic {
                            item.set_enabled(c.adult || !adult::is_adult(scene));
                        }
                        app.walk_enabled = c.walk;
                    }
                }
                // Bubble/about expiry: one final redraw clears the overlay
                // (render itself never expires them, so a shown bubble no
                // longer forces a redraw every tick).
                if app.bubble.take_if(|(_, until)| now >= *until).is_some() {
                    changed = true;
                }
                if app.about_until.take_if(|until| now >= *until).is_some() {
                    changed = true;
                }
                if changed {
                    if let Some(w) = &app.window {
                        request_paint(w);
                    }
                }
                // Sleep until the next thing that actually changes pixels:
                // the pet's next frame/gap, an overlay expiry, the tray
                // pump, or the 1 Hz idle poll. Dragging and tray "Move"
                // need pointer-rate updates; otherwise 60 Hz is waste.
                let mut wake = app
                    .pet
                    .next_wake()
                    .into_iter()
                    .chain(app.bubble.iter().map(|(_, until)| *until))
                    .chain(app.about_until)
                    .min()
                    .unwrap_or(now + Duration::from_secs(1));
                if app.tray.is_some() {
                    wake = wake.min(now + Duration::from_millis(100));
                }
                if app.blank_hide_after.is_some() {
                    wake = wake.min(now + Duration::from_millis(250));
                }
                if let Some(t) = app.blank_hide_after {
                    wake = wake.min(t);
                }
                if app.press.is_some() || app.move_mode.is_some() {
                    wake = wake.min(now + Duration::from_millis(16));
                }
                wake = wake.max(now);
                if app.last_idle_query.elapsed() < Duration::from_secs(1) {
                    wake = wake.min(app.last_idle_query + Duration::from_secs(1));
                }
                el.set_control_flow(ControlFlow::WaitUntil(wake));
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

#[cfg(test)]
mod visit_tests {
    use super::{visit_url, CHARACTERS, VISIT_SITES};

    const P: &str = "https://web.archive.org/web/";

    #[test]
    fn every_shipped_character_has_an_archived_product_page() {
        for c in CHARACTERS {
            assert!(visit_url(c).is_some(), "no archived product page for {c}");
        }
    }

    #[test]
    fn visit_urls_are_https_wayback_captures() {
        for (character, url) in VISIT_SITES {
            assert!(url.starts_with(P), "{character}: not a wayback capture: {url}");
            // .../<14-digit capture timestamp>/<original http(s) url>
            let rest = &url[P.len()..];
            let (ts, target) = rest.split_once('/').unwrap_or((rest, ""));
            assert_eq!(ts.len(), 14, "{character}: bad capture timestamp: {ts}");
            assert!(ts.chars().all(|c| c.is_ascii_digit()), "{character}: bad timestamp: {ts}");
            assert!(
                target.starts_with("http://") || target.starts_with("https://"),
                "{character}: bad capture target: {target}"
            );
        }
    }
}
