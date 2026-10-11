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

mod pet;
mod scenes;

use ab_glyph::{point, Font, FontArc, PxScale, ScaleFont};
use image::RgbaImage;
use pet::Pet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::dpi::{LogicalSize, PhysicalPosition, Position};
use winit::event::{ElementState, Event, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::{Icon, Window, WindowAttributes, WindowLevel};

/// FAZ frame canvas; the window is exactly one frame.
const WIN: u32 = 200;

const ABOUT: &str = "DeskMates-rs v0.1\nTahni the Deskmate, safely reimplemented.\nNo phone-home. No unlock keys. No spyware.\n100% offline. Original animation art.\nClick her to make her react!";

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    pub scale: f64,
    pub x: Option<i32>,
    pub y: Option<i32>,
}

impl Default for Config {
    fn default() -> Self {
        Config { scale: 1.0, x: None, y: None }
    }
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

fn save_state(window: Option<&Window>, scale: f64) {
    let pos = window.and_then(|w| w.outer_position().ok());
    let cfg = Config { scale: scale.clamp(0.5, 4.0), x: pos.map(|p| p.x), y: pos.map(|p| p.y) };
    if let Some(p) = config_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(b) = serde_json::to_vec_pretty(&cfg) {
            let _ = std::fs::write(p, b);
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

fn make_tray() -> Option<tray_icon::TrayIcon> {
    use tray_icon::menu::{Menu, MenuItem};
    use tray_icon::TrayIconBuilder;
    #[cfg(target_os = "linux")]
    {
        if gtk::init().is_err() {
            return None;
        }
    }
    let img = image::open(assets_root().join("tray.png")).ok()?.to_rgba8();
    let (tw, th) = (img.width(), img.height());
    let icon = tray_icon::Icon::from_rgba(img.into_raw(), tw, th).ok()?;
    let menu = Menu::new();
    let toggle = MenuItem::with_id("toggle", "Show / Hide", true, None);
    let quit = MenuItem::with_id("quit", "Quit", true, None);
    menu.append_items(&[&toggle, &quit]).ok()?;
    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Tahni the Deskmate")
        .with_icon(icon)
        .build()
        .ok()
}

struct MenuItem {
    label: &'static str,
}

const MENU: &[MenuItem] = &[
    MenuItem { label: "Touch her" },
    MenuItem { label: "Next scene" },
    MenuItem { label: "Hide" },
    MenuItem { label: "About" },
    MenuItem { label: "Quit" },
];

struct App {
    pet: Pet,
    scenes: Arc<scenes::Scenes>,
    font: FontArc,
    scale: f64,
    matte: bool,
    pixels: Option<pixels::Pixels<'static>>,
    window: Option<Arc<Window>>,
    size: (u32, u32),
    mouse: (f64, f64),
    press: Option<((f64, f64), (i32, i32))>,
    menu_open: bool,
    menu_rects: Vec<(usize, (i32, i32, i32, i32))>,
    about_until: Option<Instant>,
    tray: Option<tray_icon::TrayIcon>,
    last_move: Instant,
    /// Fractional window x for smooth walking.
    walk_x: f64,
}

impl App {
    fn win_logical(&self) -> u32 {
        (WIN as f64 * self.scale) as u32
    }

    /// Walk the window along the bottom of the monitor per pet.walk_dir().
    fn apply_walk(&mut self, now: Instant) {
        let Some(w) = &self.window else { return };
        let dir = self.pet.walk_dir();
        let dt = now.duration_since(self.last_move).as_secs_f64().min(0.25);
        self.last_move = now;
        if dir == 0 {
            if let Ok(p) = w.outer_position() {
                self.walk_x = p.x as f64;
            }
            return;
        }
        let Some(m) = w.current_monitor().or_else(|| w.primary_monitor()) else { return };
        let (mx, mw) = (m.position().x, m.size().width as i32);
        let ww = w.outer_size().width as i32;
        let speed = 70.0 * self.scale;
        let mut x = self.walk_x + dir as f64 * speed * dt;
        // wrap around the screen edges
        if x < (mx - ww) as f64 {
            x = (mx + mw) as f64;
        } else if x > (mx + mw) as f64 {
            x = (mx - ww) as f64;
        }
        self.walk_x = x;
        if let Ok(p) = w.outer_position() {
            w.set_outer_position(Position::Physical(PhysicalPosition::new(x as i32, p.y)));
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
        // sprite
        if let Some(sp) = self.pet.sprite().cloned() {
            if sp.id != 0 {
                if let Some(img) = self.scenes.frame(&sp.scene, sp.id) {
                    let s = (self.scale * sf) as f64;
                    let ox = (sp.x as f64 * self.scale * sf) as i32;
                    let oy = (sp.y as f64 * self.scale * sf) as i32;
                    blit(&mut canvas, w, h, &img, ox, oy, s);
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
                draw_text(&mut canvas, w, h, &self.font, item.label, bx as f32 + 8.0 * s, by as f32 + bh as f32 * 0.68, PxScale::from(13.0 * s), [20, 20, 25, 255]);
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
                for line in ABOUT.lines() {
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
    }

    fn menu_click(&mut self, x: f64, y: f64) -> bool {
        for (i, (mx, my, mw, mh)) in self.menu_rects.iter().rev() {
            if x >= *mx as f64 && x < (*mx + *mw) as f64 && y >= *my as f64 && y < (*my + *mh) as f64 {
                self.menu_open = false;
                match MENU[*i].label {
                    "Touch her" => self.pet.touch(),
                    "Next scene" => self.pet.next_scene(),
                    "Hide" => {
                        if let Some(w) = &self.window {
                            w.set_visible(false);
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

fn blit(canvas: &mut [u8], cw: u32, ch: u32, img: &RgbaImage, x: i32, y: i32, scale: f64) {
    let (iw, ih) = img.dimensions();
    let dw = (iw as f64 * scale) as i32;
    let dh = (ih as f64 * scale) as i32;
    for dy in 0..dh {
        let py = y + dy;
        if py < 0 || py >= ch as i32 {
            continue;
        }
        let sy = ((dy as f64 / scale) as u32).min(ih - 1);
        for dx in 0..dw {
            let px = x + dx;
            if px < 0 || px >= cw as i32 {
                continue;
            }
            let sx = ((dx as f64 / scale) as u32).min(iw - 1);
            let [r, g, b, a] = img.get_pixel(sx, sy).0;
            if a == 0 {
                continue;
            }
            let o = (py as usize * cw as usize + px as usize) * 4;
            let d = &mut canvas[o..o + 4];
            let af = a as u16;
            d[0] = ((r as u16 * af + d[0] as u16 * (255 - af)) / 255) as u8;
            d[1] = ((g as u16 * af + d[1] as u16 * (255 - af)) / 255) as u8;
            d[2] = ((b as u16 * af + d[2] as u16 * (255 - af)) / 255) as u8;
            d[3] = (af.max(d[3] as u16)) as u8;
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
    let args: Vec<String> = std::env::args().collect();
    let matte = args.iter().any(|a| a == "--matte");
    let cfg = load_config();

    let root = assets_root();
    let sc = scenes::Scenes::load(&root.join("characters").join("tahni"))?;
    let sc = Arc::new(sc);
    let font = FontArc::try_from_vec(std::fs::read(root.join("fonts/LiberationSans-Regular.ttf"))?)?;
    let pet = Pet::new(sc.clone());

    let mut app = App {
        pet,
        scenes: sc,
        font,
        scale: cfg.scale,
        matte,
        pixels: None,
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
    };

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
                    .with_title("Tahni the Deskmate")
                    .with_decorations(false)
                    .with_transparent(!matte)
                    .with_resizable(false)
                    .with_inner_size(LogicalSize::new(app.win_logical() as f64, app.win_logical() as f64));
                #[cfg(windows)]
                {
                    use winit::platform::windows::WindowAttributesExtWindows;
                    attrs = attrs.with_skip_taskbar(true);
                }
                // X11: override-redirect = no WM, no taskbar, always ours.
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
                    .map(|m| {
                        let s = m.size();
                        let o = m.position();
                        (o.x, o.y, s.width as i32, s.height as i32)
                    })
                    .unwrap_or((0, 0, 1920, 1080));
                let sf0 = window.scale_factor();
                let ww = (app.win_logical() as f64 * sf0) as i32;
                let wh = (app.win_logical() as f64 * sf0) as i32;
                // Deskmates live at the bottom edge of the screen.
                let pos = match (cfg.x, cfg.y) {
                    (Some(x), Some(y)) if x >= ax - ww && x <= ax + aw && y >= ay && y <= ay + ah => (x, y),
                    _ => (ax + aw / 2 - ww / 2, ay + ah - wh),
                };
                window.set_outer_position(Position::Physical(PhysicalPosition::new(pos.0, pos.1)));
                app.walk_x = pos.0 as f64;
                let window = Arc::new(window);
                if let Ok(img) = image::open(assets_root().join("tray.png")) {
                    let img = img.to_rgba8();
                    let (iw, ih) = img.dimensions();
                    let _ = window.set_window_icon(Icon::from_rgba(img.into_raw(), iw, ih).ok());
                }
                if app.tray.is_none() {
                    app.tray = make_tray();
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
                            save_state(app.window.as_deref(), next);
                            if let Some(w) = &app.window {
                                let _ = w.request_inner_size(LogicalSize::new(app.win_logical() as f64, app.win_logical() as f64));
                            }
                        }
                    }
                }
                WindowEvent::CursorMoved { position, .. } => {
                    app.mouse = (position.x, position.y);
                    if let Some(((pmx, pmy), (wpx, wpy))) = app.press {
                        let dx = position.x - pmx;
                        let dy = position.y - pmy;
                        if dx.abs() + dy.abs() > 6.0 {
                            if let Some(w) = &app.window {
                                w.set_outer_position(Position::Physical(PhysicalPosition::new(wpx + dx as i32, wpy + dy as i32)));
                                app.walk_x = (wpx + dx as i32) as f64;
                            }
                        }
                    }
                    if let Some(w) = &app.window {
                        w.request_redraw();
                    }
                }
                WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                    let wp = app.window.as_ref().and_then(|w| w.outer_position().ok()).unwrap_or(PhysicalPosition::new(0, 0));
                    app.press = Some((app.mouse, (wp.x, wp.y)));
                }
                WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                    if let Some(((pmx, pmy), _)) = app.press.take() {
                        let (mx, my) = app.mouse;
                        let moved = (mx - pmx).abs() + (my - pmy).abs() > 6.0;
                        if !moved {
                            if !app.menu_click(mx, my) {
                                if app.menu_open {
                                    app.menu_open = false;
                                } else {
                                    app.pet.touch();
                                }
                                if let Some(w) = &app.window {
                                    w.request_redraw();
                                }
                            }
                        } else {
                            save_state(app.window.as_deref(), app.scale);
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
                let mut changed = app.pet.tick(now);
                if app.pet.walk_dir() != 0 {
                    app.apply_walk(now);
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
