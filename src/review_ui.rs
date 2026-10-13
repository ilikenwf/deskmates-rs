//! Forked scene-browser window: the `--review` upgrade. A normal,
//! WM-managed winit window (same fork pattern as [`crate::ask_ui`]) that
//! lists the running pet's whole scene set as a two-column scrollable
//! tree — scene rows expand to their entries (sub-scenes) — so the adult
//! blocklist can be curated down to the entry level.
//!
//! Run as: `deskmates-rs --review-ui [--character NAME]` (the pet forks
//! it when `--review` starts). Row clicks hand play requests to the pet
//! through the play file (`scene\nentry`, or `switch:<name>` for the
//! character dropdown), mtime-polled exactly like the ask file. Each row
//! carries a "gate?" checkbox; **Export** writes every marked name
//! (one per line) to the marks file and prints them to stdout — that is
//! how the user reports which sub-scenes need gating. The window title
//! shows the current mark count.

use crate::assets_root;
use crate::retro;
use crate::display_name;
use crate::line_height;
use crate::CHARACTERS;
use ab_glyph::FontArc;
use ab_glyph::PxScale;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::dpi::LogicalSize;
use winit::event::{ElementState, Event, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes};

const W: u32 = 520;
const H: u32 = 640;

/// One visible tree row: a scene (entry None) or one of its entries.
#[derive(Clone)]
struct Row {
    scene: String,
    entry: Option<String>,
    /// Gated name tested for this row (scene name or entry name).
    name: String,
    gated: bool,
    expanded: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Zone {
    Play,
    Mark,
    Toggle,
}

/// Hit rect recorded by the last render: (row index, zone, rect).
struct Hit {
    row: usize,
    zone: Zone,
    rect: (i32, i32, i32, i32),
}

fn in_rect(x: f64, y: f64, r: &(i32, i32, i32, i32)) -> bool {
    x >= r.0 as f64 && x < (r.0 + r.2) as f64 && y >= r.1 as f64 && y < (r.1 + r.3) as f64
}

/// Startup parent pid. The forked browser is a direct child of the pet
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
/// Windows the forked browser can outlive the pet.
#[cfg(unix)]
fn parent_died(start: u32) -> bool {
    (unsafe { libc::getppid() as u32 }) != start
}

#[cfg(not(unix))]
fn parent_died(_start: u32) -> bool {
    false
}

/// The character's playable scenes, sorted: (scene name, entry names).
/// Behaviors (common.FAS etc.) are def sources, never review rows.
fn load_tree(character: &str) -> Vec<(String, Vec<String>)> {
    let dir = assets_root().join("characters").join(character);
    let Ok(sc) = crate::scenes::Scenes::load(&dir) else {
        return Vec::new();
    };
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (s, e) in sc.all_entries() {
        map.entry(s).or_default().push(e);
    }
    map.into_iter().collect()
}

struct Ui {
    font: FontArc,
    character: String,
    scenes: Vec<(String, Vec<String>)>,
    expanded: HashSet<String>,
    marks: HashSet<String>,
    rows: Vec<Row>,
    hits: Vec<Hit>,
    scroll: usize,
    dropdown_open: bool,
    dropdown_rect: (i32, i32, i32, i32),
    dropdown_items: Vec<(usize, (i32, i32, i32, i32))>,
    export_rect: (i32, i32, i32, i32),
    tree_rect: (i32, i32, i32, i32),
    status: String,
    window: Option<Arc<Window>>,
    pixels: Option<pixels::Pixels<'static>>,
    size: (u32, u32),
    mouse_pos: (f64, f64),
    /// Last row this browser asked the pet to play: (scene, entry).
    /// Rendered with a ▶ marker so the playing row is obvious.
    playing: Option<(String, Option<String>)>,
}

impl Ui {
    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for (scene, entries) in &self.scenes {
            let expanded = self.expanded.contains(scene);
            self.rows.push(Row {
                scene: scene.clone(),
                entry: None,
                name: scene.clone(),
                gated: crate::adult::is_adult(scene),
                expanded,
            });
            if expanded {
                for e in entries {
                    self.rows.push(Row {
                        scene: scene.clone(),
                        entry: Some(e.clone()),
                        name: e.clone(),
                        gated: crate::adult::is_adult(e) || crate::adult::is_adult(scene),
                        expanded: false,
                    });
                }
            }
        }
    }

    fn set_title(&self) {
        if let Some(w) = &self.window {
            let _ = w.set_title(&format!(
                "Scene review — {} marked",
                self.marks.len()
            ));
        }
    }

    /// Hand a request to the running pet via the play file (mtime-polled
    /// exactly like the ask file).
    fn send(&self, req: &str) {
        let path = crate::play_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, req);
    }

    fn play_row(&mut self, row: &Row) {
        match &row.entry {
            None => self.send(&format!("{}\n", row.scene)),
            Some(e) => self.send(&format!("{}\n{e}\n", row.scene)),
        }
        self.playing = Some((row.scene.clone(), row.entry.clone()));
    }

    fn select_character(&mut self, idx: usize) {
        let Some(&name) = CHARACTERS.get(idx) else {
            return;
        };
        self.dropdown_open = false;
        if name == self.character {
            return;
        }
        self.send(&format!("switch:{name}"));
        self.character = name.to_string();
        self.scenes = load_tree(name);
        self.expanded.clear();
        self.scroll = 0;
        self.rebuild_rows();
        self.status = format!("Switched the pet to {}.", display_name(name));
    }

    fn export(&mut self) {
        let mut names: Vec<&String> = self.marks.iter().collect();
        names.sort();
        let body = names
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let body = if body.is_empty() { body } else { format!("{body}\n") };
        let path = crate::marks_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, &body);
        for n in &names {
            println!("{n}");
        }
        self.status = format!(
            "Exported {} marks to {} (and to stdout).",
            names.len(),
            path.display()
        );
    }

    fn click(&mut self, x: f64, y: f64) {
        if self.dropdown_open {
            for (idx, rect) in self.dropdown_items.clone() {
                if in_rect(x, y, &rect) {
                    self.select_character(idx);
                    return;
                }
            }
            self.dropdown_open = false;
            return;
        }
        if in_rect(x, y, &self.dropdown_rect) {
            self.dropdown_open = true;
            return;
        }
        if self.export_rect.2 > 0 && in_rect(x, y, &self.export_rect) {
            self.export();
            return;
        }
        if in_rect(x, y, &self.tree_rect) {
            // Resolve the hit first, then act: the row borrow must end
            // before the mutable play/mark/toggle handlers run.
            let Some((rowi, zone)) = self
                .hits
                .iter()
                .find(|h| in_rect(x, y, &h.rect))
                .map(|h| (h.row, h.zone))
            else {
                return;
            };
            let Some(row) = self.rows.get(rowi).cloned() else { return };
            match zone {
                Zone::Play => {
                    self.play_row(&row);
                    self.status = format!("Playing {} on the pet.", row.name);
                }
                Zone::Mark => {
                    if !self.marks.remove(&row.name) {
                        self.marks.insert(row.name.clone());
                    }
                    self.set_title();
                    self.status = format!(
                        "{} marked — {} total. Export writes them.",
                        row.name,
                        self.marks.len()
                    );
                }
                Zone::Toggle => {
                    if !self.expanded.remove(&row.scene) {
                        self.expanded.insert(row.scene.clone());
                    }
                    self.rebuild_rows();
                }
            }
        }
    }

    fn scroll_by(&mut self, lines: i32) {
        let vis = self.visible_rows().max(1) as i32;
        let max = (self.rows.len() as i32 - vis).max(0);
        self.scroll = ((self.scroll as i32 + lines).clamp(0, max)) as usize;
    }

    fn build_pixels(w: u32, h: u32, window: &Arc<Window>) -> Option<pixels::Pixels<'static>> {
        let st = pixels::SurfaceTexture::new(w, h, window.clone());
        pixels::PixelsBuilder::new(w, h, st)
            .clear_color(pixels::wgpu::Color { r: 0.75, g: 0.75, b: 0.75, a: 1.0 })
            .build()
            .ok()
    }

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

    /// Row pitch in physical px: theme font line height + padding. The
    /// scroll clamp and the row painter MUST use the same number, or the
    /// list overflows the tree frame into the Export footer.
    fn row_h(&self) -> f32 {
        let s = self.window.as_ref().map_or(1.0, |w| w.scale_factor()) as f32;
        let px = (12.0 * s).max(10.0);
        line_height(&self.font, PxScale { x: px, y: px }) + 6.0 * s
    }

    /// Column caption strip height inside the tree frame.
    fn header_h(&self) -> i32 {
        self.row_h().round() as i32
    }

    /// Visible row count for the current tree area.
    fn visible_rows(&self) -> usize {
        ((self.tree_rect.3 - self.header_h() - 6) as f32 / self.row_h()).max(0.0) as usize
    }

    fn render(&mut self) {
        let Some(window) = &self.window else { return };
        let (w, h) = self.size;
        let mut canvas = vec![0u8; (w as usize) * (h as usize) * 4];
        let s = window.scale_factor() as f32;
        let t = retro::apply_retro_theme(&mut canvas, w, h, s);
        let lh = t.lh(&self.font);
        self.hits.clear();
        self.dropdown_items.clear();

        let m = 8.0 * s;
        // Header: character dropdown.
        let dd_w = (180.0 * s) as i32;
        let dd_h = (26.0 * s) as i32;
        retro::label(&mut canvas, w, h, &self.font, &t, "Character:", m, m + 2.0 * s, false);
        let dd_x = (m + retro::text_w(&self.font, &t, "Character:") + 8.0 * s) as i32;
        retro::field(&mut canvas, w, h, dd_x, m as i32, dd_w, dd_h, false);
        retro::text(
            &mut canvas,
            w,
            h,
            &self.font,
            &t,
            &format!("{}  v", display_name(&self.character)),
            dd_x as f32 + 6.0 * s,
            m + (dd_h as f32 - lh) / 2.0 + lh,
            retro::TEXT,
        );
        self.dropdown_rect = (dd_x, m as i32, dd_w, dd_h);

        // Bottom: Export button + sunken status bar.
        let sb_h = (22.0 * s) as i32;
        let sb_y = h as i32 - sb_h;
        let btn_w = (90.0 * s) as i32;
        let btn_h = (26.0 * s) as i32;
        let btn_y = sb_y - btn_h - (6.0 * s) as i32;
        self.export_rect = retro::button(
            &mut canvas, w, h, &self.font, &t,
            m as i32, btn_y, btn_w, btn_h, "Export", true, false, false,
        );
        let marked = format!(
            "{} marked — click a name to play it on the pet; tick gate? to mark, Export to save.",
            self.marks.len()
        );
        retro::statusbar(
            &mut canvas, w, h, &self.font, &t,
            0, sb_y, w as i32, sb_h,
            if self.status.is_empty() { marked.as_str() } else { &self.status },
        );

        // Tree: sunken white client area between header and button row.
        let ty = m as i32 + dd_h + (8.0 * s) as i32;
        let tx = m as i32;
        let tw = w as i32 - 2 * m as i32;
        let th = btn_y - ty - (8.0 * s) as i32;
        self.tree_rect = (tx, ty, tw, th);
        retro::sunken(&mut canvas, w, h, tx, ty, tw, th, retro::WHITE);
        let row_h = self.row_h().round() as i32;
        let gate_x = tx + tw - (150.0 * s) as i32;
        let mark_x = tx + tw - (80.0 * s) as i32;
        let sep_x = gate_x - (14.0 * s) as i32;
        // Column caption strip: makes the click-to-play name cell and the
        // gate-mark checkbox column visually distinct zones.
        let hh = self.header_h();
        crate::fill_rect(&mut canvas, w, h, tx + 2, ty + 2, tw - 4, hh, retro::FACE);
        let hbase = ty as f32 + 2.0 + (hh as f32 - lh) / 2.0 + lh;
        retro::text(&mut canvas, w, h, &self.font, &t, "name — click to play", tx as f32 + 8.0 * s, hbase, retro::SHADOW);
        retro::text(&mut canvas, w, h, &self.font, &t, "adult", gate_x as f32, hbase, retro::SHADOW);
        retro::text(&mut canvas, w, h, &self.font, &t, "gate?", mark_x as f32 - 6.0 * s, hbase, retro::SHADOW);
        crate::fill_rect(&mut canvas, w, h, sep_x, ty + 2, 1, th - 4, retro::SHADOW);
        let vis = self.visible_rows();
        for (vi, ri) in (self.scroll..self.rows.len()).enumerate().take(vis) {
            let row = &self.rows[ri];
            let y = ty + 2 + hh + (vi as i32) * row_h;
            if y + row_h > ty + th {
                break; // never paint over the footer
            }
            let baseline = y as f32 + (row_h as f32 - lh) / 2.0 + lh;
            let (tri_w, name_x);
            if row.entry.is_none() {
                tri_w = (16.0 * s) as i32;
                name_x = tx as f32 + 6.0 * s + tri_w as f32;
                retro::text(
                    &mut canvas, w, h, &self.font, &t,
                    if row.expanded { "v" } else { ">" },
                    tx as f32 + 6.0 * s, baseline, retro::TEXT,
                );
                self.hits.push(Hit {
                    row: ri,
                    zone: Zone::Toggle,
                    rect: (tx, y, tri_w, row_h),
                });
            } else {
                tri_w = 0;
                name_x = tx as f32 + 28.0 * s;
            }
            let play_rect = (tx + tri_w, y, (sep_x - tx - tri_w).max(1), row_h);
            let hover = in_rect(self.mouse_pos.0, self.mouse_pos.1, &play_rect);
            if hover {
                // Classic navy highlight: the name cell is a play button.
                crate::fill_rect(&mut canvas, w, h, tx + tri_w + 1, y, (sep_x - tx - tri_w - 2).max(1), row_h, retro::SELECTION);
            }
            retro::text(
                &mut canvas, w, h, &self.font, &t,
                &row.name, name_x, baseline,
                if hover {
                    retro::WHITE
                } else if row.entry.is_none() {
                    retro::TEXT
                } else {
                    retro::SHADOW
                },
            );
            if self.playing.as_ref().is_some_and(|(s, e)| *s == row.scene && *e == row.entry) {
                // U+25BA (right-pointing pointer, reads as ▶): the bundled
                // Liberation Sans lacks U+25B6, which would draw tofu.
                retro::text(
                    &mut canvas, w, h, &self.font, &t,
                    "\u{25ba}", (sep_x - (18.0 * s) as i32) as f32, baseline,
                    if hover { retro::WHITE } else { retro::SELECTION },
                );
            }
            retro::text(
                &mut canvas, w, h, &self.font, &t,
                if row.gated { "[x] adult" } else { "[ ]" },
                gate_x as f32, baseline,
                if row.gated { retro::DARK } else { retro::SHADOW },
            );
            retro::checkbox(
                &mut canvas, w, h, &self.font, &t,
                mark_x, y + (row_h - t.box_px()) / 2, "", self.marks.contains(&row.name), false,
            );
            self.hits.push(Hit {
                row: ri,
                zone: Zone::Play,
                rect: play_rect,
            });
            self.hits.push(Hit {
                row: ri,
                zone: Zone::Mark,
                rect: (mark_x - 4, y, t.box_px() + 8, row_h),
            });
        }

        // Dropdown list paints over the tree when open.
        if self.dropdown_open {
            let item_h = dd_h;
            let n = CHARACTERS.len();
            retro::sunken(
                &mut canvas, w, h,
                dd_x, m as i32 + dd_h, dd_w, item_h * n as i32 + 2, retro::WHITE,
            );
            for (i, c) in CHARACTERS.iter().enumerate() {
                let iy = m as i32 + dd_h + 1 + i as i32 * item_h;
                let sel = *c == self.character;
                if sel {
                    crate::fill_rect(&mut canvas, w, h, dd_x + 1, iy, dd_w - 2, item_h, retro::SELECTION);
                }
                retro::text(
                    &mut canvas, w, h, &self.font, &t,
                    &display_name(c),
                    dd_x as f32 + 6.0 * s,
                    iy as f32 + (item_h as f32 - lh) / 2.0 + lh,
                    if sel { retro::WHITE } else { retro::TEXT },
                );
                self.dropdown_items.push((i, (dd_x, iy, dd_w, item_h)));
            }
        }
        self.present(canvas);
    }
}

pub fn run(character: &str) -> Result<(), Box<dyn std::error::Error>> {
    let el = EventLoop::new()?;
    let font = FontArc::try_from_vec(std::fs::read(assets_root().join("fonts/LiberationSans-Regular.ttf"))?)?;
    // Resume any previous marking session: the marks file seeds the boxes.
    let marks: HashSet<String> = std::fs::read_to_string(crate::marks_path())
        .unwrap_or_default()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let scenes = load_tree(character);
    let mut ui = Ui {
        font,
        character: character.to_string(),
        scenes,
        expanded: HashSet::new(),
        marks,
        rows: Vec::new(),
        hits: Vec::new(),
        scroll: 0,
        dropdown_open: false,
        dropdown_rect: (0, 0, 0, 0),
        dropdown_items: Vec::new(),
        export_rect: (0, 0, 0, 0),
        tree_rect: (0, 0, 0, 0),
        status: String::new(),
        window: None,
        pixels: None,
        size: (W, H),
        mouse_pos: (0.0, 0.0),
        playing: None,
    };
    ui.rebuild_rows();

    let start_parent = startup_parent();

    #[allow(deprecated)]
    el.run(move |event, el| match event {
        Event::NewEvents(winit::event::StartCause::Init) => {
            let Ok(window) = el.create_window(
                WindowAttributes::default()
                    .with_title("Scene review")
                    .with_inner_size(LogicalSize::new(W, H))
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
            let s = window.inner_size();
            let (pw, ph) = (s.width.max(1), s.height.max(1));
            ui.size = (pw, ph);
            match Ui::build_pixels(pw, ph, &window) {
                Some(p) => {
                    ui.pixels = Some(p);
                    ui.window = Some(window);
                    ui.set_title();
                }
                None => {
                    eprintln!("review: renderer failed");
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
                // Rects are painted in physical px; winit delivers logical.
                let sf = ui.window.as_ref().map_or(1.0, |w| w.scale_factor());
                ui.mouse_pos = (position.x * sf, position.y * sf);
                ui.window.as_ref().map(|w| w.request_redraw());
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let (x, y) = ui.mouse_pos;
                ui.click(x, y);
                ui.window.as_ref().map(|w| w.request_redraw());
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * 3.0) as i32,
                    MouseScrollDelta::PixelDelta(p) => (p.y / 40.0) as i32,
                };
                if lines != 0 {
                    ui.scroll_by(-lines);
                    ui.window.as_ref().map(|w| w.request_redraw());
                }
            }
            WindowEvent::KeyboardInput { event: k, .. } => {
                if k.state == ElementState::Pressed {
                    match k.logical_key {
                        Key::Named(NamedKey::Escape) => el.exit(),
                        Key::Named(NamedKey::ArrowDown) => ui.scroll_by(1),
                        Key::Named(NamedKey::ArrowUp) => ui.scroll_by(-1),
                        Key::Named(NamedKey::PageDown) => ui.scroll_by(ui.visible_rows() as i32),
                        Key::Named(NamedKey::PageUp) => ui.scroll_by(-(ui.visible_rows() as i32)),
                        _ => {}
                    }
                    ui.window.as_ref().map(|w| w.request_redraw());
                }
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
