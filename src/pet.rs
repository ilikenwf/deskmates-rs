//! Scene scheduler: plays the original FAZ script structure natively.
//!
//! A scene entry (e.g. `tankgirl/tank_aa`) is a token list; tokens are frame
//! refs, sub-def refs (with repeat count or duration-ms), and builtins that
//! the original engine understood (`exit_wait`, `fence_lb_out`, …). Missing
//! defs (the stripped `common.FAS` lists like `0003`) degrade to a short
//! hold, exactly like a blank frame.

use crate::scenes::{Scene, Scenes};
use rand::Rng;
use std::sync::Arc;
use std::time::{Duration, Instant};
use rand::seq::{IndexedRandom, SliceRandom};

const TIME_REP: u32 = 1000;
const MAX_DEPTH: usize = 24;

#[derive(Clone, Debug)]
pub enum Step {
    Frame { scene: String, id: u32, x: i32, y: i32 },
    Sub { scene: String, name: String, rep: u32 },
    /// ` !name` — fire a sound effect, no frame change.
    Sound { name: String },
    Hold { ms: u64 },
    /// `enter_home`: scene finished.
    End,
    /// `pause`: scene finished, idle gap before the next one.
    EndPause,
}

struct Level {
    steps: Vec<Step>,
    idx: usize,
    rep_left: u32,
    /// For time-looped subs: loop until this deadline.
    deadline: Option<Instant>,
    /// walk hint owned by this level (from *_r / *_l def names).
    dir: i8,
}

/// Current sprite to draw: scene + frame id + pixel offset. id 0 = blank.
#[derive(Clone, Debug, PartialEq)]
pub struct Sprite {
    pub scene: String,
    pub id: u32,
    pub x: i32,
    pub y: i32,
}

pub struct Pet {
    scenes: Arc<Scenes>,
    entries: Vec<(String, String)>,
    reactions: Vec<(String, String)>,
    stack: Vec<Level>,
    cur: Option<Sprite>,
    frame_at: Instant,
    frame_dur: Duration,
    walk_dir: i8,
    /// Idle gap: when to start the next random scene.
    next_at: Option<Instant>,
    speed: f64,
    /// Shuffle-bag over `entries`: every scene plays once per bag before
    /// any repeats (fixes "always the same three scenes").
    bag: Vec<usize>,
    bag_at: usize,
    /// A ` !name` token fired since the last `take_sound`.
    pending_sound: Option<String>,
}

/// Scenes that read as touch reactions (the stripped touch.FAS mapped
/// clicks to these; curated from scene semantics).
const REACTIONS: &[&str] = &[
    "bulldog_chased", "unwanted_kiss", "shock", "karate", "gstring", "busty",
    "tahni_tease", "kiss_screen", "lightning", "lippy", "shake_a", "cupid",
    "balloons", "rubber_duck",
];

fn frame_dur_for(speed: f64) -> Duration {
    Duration::from_millis((90.0 / speed) as u64)
}

impl Pet {
    pub fn new(scenes: Arc<Scenes>, adult: bool) -> Pet {
        let entries = Self::pool(&scenes, adult);
        let reactions = REACTIONS
            .iter()
            .filter_map(|r| {
                entries.iter().find(|(s, _)| s == r).cloned().map(|(s, e)| (s, e))
            })
            .collect();
        let mut pet = Pet {
            scenes,
            entries,
            reactions,
            stack: Vec::new(),
            cur: None,
            frame_at: Instant::now(),
            frame_dur: frame_dur_for(1.0),
            walk_dir: 0,
            next_at: None,
            speed: 1.0,
            bag: Vec::new(),
            bag_at: 0,
            pending_sound: None,
        };
        pet.start_random();
        // Original startup: S_Deskmate_Enter (walk in + a weighted-random
        // greeting voice line) from common.FAS; fall back to the bare
        // entry pool in common_enter.FAS.
        const STARTUPS: [(&str, &str); 2] =
            [("common", "S_Deskmate_Enter"), ("common_enter", "enter")];
        if let Some((s, d)) = STARTUPS
            .into_iter()
            .find(|(s, d)| pet.scenes.scenes.get(*s).map_or(false, |x| x.raw.contains_key(*d)))
        {
            pet.play(s, d);
        }
        pet
    }

    /// Take a fired sound effect name for the host to play.
    pub fn take_sound(&mut self) -> Option<String> {
        self.pending_sound.take()
    }

    /// Random-pool of entry scenes minus adults-only ones unless enabled.
    fn pool(scenes: &Scenes, adult: bool) -> Vec<(String, String)> {
        scenes
            .all_entries()
            .into_iter()
            .filter(|(s, _)| adult || !crate::adult::is_adult(s))
            .collect()
    }

    /// Toggle adults-only scenes; takes effect from the next scene onward.
    pub fn set_adult(&mut self, adult: bool) {
        self.entries = Self::pool(&self.scenes, adult);
        self.bag.clear();
        self.bag_at = 0;
    }

    pub fn sprite(&self) -> Option<&Sprite> {
        self.cur.as_ref()
    }

    /// Scene the current sprite belongs to (for preload scheduling).
    pub fn cur_scene(&self) -> Option<String> {
        self.cur.as_ref().map(|s| s.scene.clone())
    }

    pub fn scenes_arc(&self) -> Arc<Scenes> {
        self.scenes.clone()
    }

    pub fn walk_dir(&self) -> i8 {
        self.walk_dir
    }

    /// Click at native sprite coords: resolve the touch.FAS hotspot under the
    /// cursor (bottom-up 8bpp bitmap → color → `T0x<hex>` sequence) and play
    /// its reaction (voice line + animation). Falls back to a random reaction
    /// scene when the hotspot has no playable definition.
    pub fn touch_at(&mut self, nx: f64, ny: f64) {
        let Some(t) = self.scenes.touch.as_ref() else {
            self.touch();
            return;
        };
        let bx = nx.floor();
        let by = t.height as f64 - 1.0 - ny.floor();
        if bx < 0.0 || by < 0.0 || bx >= t.width as f64 || by >= t.height as f64 {
            self.touch();
            return;
        }
        let ix = by as usize * t.width as usize + bx as usize;
        let color = match t.bitmap.get(ix).and_then(|&v| t.colors.get(v as usize)) {
            Some(c) => *c,
            None => {
                self.touch();
                return;
            }
        };
        // Definition names are `T0x` + 6 lowercase hex digits (python
        // `%06x` of the hotspot color) + state; a tap is press+release, so
        // prefer the full `up` reaction, then `down`, then `start`.
        let base = format!("T0x{color:06x}");
        for suf in ["up", "down", "start"] {
            let def = format!("{base}{suf}");
            if self
                .scenes
                .scenes
                .get("touch")
                .map_or(false, |s| s.raw.contains_key(&def))
            {
                self.play("touch", &def);
                return;
            }
        }
        self.touch();
    }

    /// Click on the sprite: interrupt with a touch reaction.
    pub fn touch(&mut self) {
        if let Some((scene, entry)) = self.reactions.choose(&mut rand::rng()).cloned() {
            self.play(&scene, &entry);
        }
    }

    /// Menu "next scene": interrupt with any random scene.
    pub fn next_scene(&mut self) {
        self.start_random();
    }

    /// Play a scene by (partial) name — used for agent stage directions.
    /// Matches scene names equal to or prefixed by `name`.
    pub fn play_named(&mut self, name: &str) -> bool {
        if let Some((scene, entry)) = self
            .entries
            .iter()
            .find(|(s, _)| s == name || s.starts_with(name))
            .cloned()
        {
            self.play(&scene, &entry);
            return true;
        }
        false
    }

    pub fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        // Idle gap.
        if self.stack.is_empty() {
            if let Some(at) = self.next_at {
                if now >= at {
                    self.next_at = None;
                    self.start_random();
                    changed = true;
                }
            }
            return changed;
        }
        while now >= self.frame_at {
            self.frame_at += self.frame_dur;
            // guard against long sleeps producing a huge catch-up loop
            if now.duration_since(self.frame_at) > Duration::from_secs(1) {
                self.frame_at = now;
            }
            if !self.advance(now) {
                break;
            }
            changed = true;
        }
        changed
    }

    fn start_random(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        if self.bag_at >= self.bag.len() {
            self.bag = (0..self.entries.len()).collect();
            self.bag.shuffle(&mut rand::rng());
            self.bag_at = 0;
        }
        if let Some((scene, entry)) = self.entries.get(self.bag[self.bag_at]).cloned() {
            self.bag_at += 1;
            self.play(&scene, &entry);
        }
    }

    fn play(&mut self, scene: &str, entry: &str) {
        self.stack.clear();
        self.walk_dir = 0;
        self.speed = 1.0;
        self.next_at = None;
        if let Some(steps) = self.steps_for(scene, entry) {
            self.stack.push(Level { steps, idx: 0, rep_left: 1, deadline: None, dir: 0 });
        }
        self.frame_at = Instant::now();
        self.frame_dur = frame_dur_for(self.speed);
    }

    fn steps_for(&self, scene: &str, name: &str) -> Option<Vec<Step>> {
        let sc: &Scene = self.scenes.scenes.get(scene)?;
        // Re-roll weighted `{a|b%2}` choice groups fresh on every play.
        let body = sc.raw.get(name)?;
        let body = crate::faz::resolve_choices(body, &mut rand::rng());
        let toks = crate::faz::parse_reflist(&body);
        let mut steps = Vec::with_capacity(toks.len());
        for t in toks {
            match t {
                crate::faz::Tok::Name { name, rep } => {
                    steps.push(Step::Sub { scene: scene.to_string(), name, rep })
                }
                crate::faz::Tok::Frame { id, x, y, rep } => {
                    for _ in 0..rep.max(1) {
                        steps.push(Step::Frame { scene: scene.to_string(), id, x, y });
                    }
                }
                crate::faz::Tok::Sound { name } => steps.push(Step::Sound { name }),
            }
        }
        Some(steps)
    }

    /// Advance one step. Returns false when the stack is exhausted.
    fn advance(&mut self, now: Instant) -> bool {
        loop {
            let Some(lv) = self.stack.last_mut() else { return false };
            if lv.idx >= lv.steps.len() {
                // level exhausted: repeat or expire
                if lv.rep_left > 1 {
                    lv.rep_left -= 1;
                    lv.idx = 0;
                    continue;
                }
                if let Some(deadline) = lv.deadline {
                    if now < deadline {
                        lv.idx = 0;
                        continue;
                    }
                }
                let dir = lv.dir;
                self.stack.pop();
                if dir != 0 {
                    self.walk_dir = self.stack.iter().rev().find_map(|l| (l.dir != 0).then_some(l.dir)).unwrap_or(0);
                }
                if self.stack.is_empty() {
                    self.cur = None;
                    self.next_at = Some(now + Duration::from_millis(600 + rand::rng().random_range(0..2500)));
                    return false;
                }
                continue;
            }
            let step = lv.steps[lv.idx].clone();
            lv.idx += 1;
            match step {
                Step::Frame { scene, id, x, y } => {
                    self.cur = Some(Sprite { scene, id, x, y });
                    return true;
                }
                Step::Sound { name } => {
                    self.pending_sound = Some(name);
                    continue;
                }
                Step::Hold { ms } => {
                    self.frame_dur = Duration::from_millis(ms);
                    return true;
                }
                Step::End | Step::EndPause => {
                    self.stack.clear();
                    self.cur = None;
                    self.walk_dir = 0;
                    let gap = if matches!(step, Step::EndPause) { 1500 } else { 400 };
                    self.next_at = Some(now + Duration::from_millis(gap + rand::rng().random_range(0..1500)));
                    return false;
                }
                Step::Sub { scene, name, rep } => {
                    self.frame_dur = frame_dur_for(self.speed);
                    match name.as_str() {
                        "all" => { self.speed = 1.0; continue; }
                        "all_slow" => { self.speed = 0.65; self.frame_dur = frame_dur_for(self.speed); continue; }
                        "all_fast" => { self.speed = 1.5; self.frame_dur = frame_dur_for(self.speed); continue; }
                        _ => {}
                    }
                    if name.starts_with("fence_") {
                        // screen-edge transitions: pick a walk direction
                        self.walk_dir = if name.contains("lb") || name.contains("lm") || name.contains("tl") {
                            1
                        } else if name.contains("rb") || name.contains("rm") || name.contains("tr") {
                            -1
                        } else {
                            self.walk_dir
                        };
                        self.frame_dur = Duration::from_millis(300);
                        return true;
                    }
                    // Resolve the def: the scene's own defs first, then the
                    // shared common.FAS namespace (blink, pause pools, …).
                    let mut scene = scene;
                    let own = self
                        .scenes
                        .scenes
                        .get(&scene)
                        .map_or(false, |s| s.raw.contains_key(&name));
                    if !own {
                        for fb in ["common", "common_enter"] {
                            if self
                                .scenes
                                .scenes
                                .get(fb)
                                .map_or(false, |s| s.raw.contains_key(&name))
                            {
                                scene = fb.to_string();
                                break;
                            }
                        }
                    }
                    if let Some(steps) = self.steps_for(&scene, &name) {
                        if self.stack.len() >= MAX_DEPTH {
                            continue;
                        }
                        let dir = if name.ends_with("_r") {
                            1
                        } else if name.ends_with("_l") {
                            -1
                        } else {
                            0
                        };
                        if dir != 0 {
                            self.walk_dir = dir;
                        }
                        let (rep_left, deadline) = if rep >= TIME_REP {
                            (1, Some(now + Duration::from_millis(rep as u64)))
                        } else {
                            (rep.max(1), None)
                        };
                        self.stack.push(Level { steps, idx: 0, rep_left, deadline, dir });
                        continue;
                    }
                    // No def anywhere: builtin wait or unknown-list hold.
                    match name.as_str() {
                        "exit_wait" => { self.frame_dur = Duration::from_millis(900); return true; }
                        "home_wait" => { self.frame_dur = Duration::from_millis(1600); return true; }
                        "pause" => { self.frame_dur = Duration::from_millis(1200); return true; }
                        _ => { self.frame_dur = Duration::from_millis(120); return true; }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod gating_tests {
    use super::*;
    /// Default (adult-off) pools must contain zero gated scenes, and every
    /// gated tahni scene must be a real playable entry (catches blocklist
    /// typos that would make a gate vacuous).
    #[test]
    fn default_pool_excludes_gated() {
        let sc = crate::scenes::Scenes::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/characters/tahni"),
        )
        .unwrap();
        let off = Pet::pool(&sc, false);
        for (s, _) in &off {
            assert!(!crate::adult::is_adult(s), "gated {s} leaked into default pool");
        }
        let on = Pet::pool(&sc, true);
        for g in crate::adult::ADULT_SCENES {
            let tahni_scene = !g.starts_with("maeka_") && !g.starts_with("kahli_") && !g.starts_with("fb_");
            if tahni_scene {
                assert!(
                    on.iter().any(|(s, _)| s == g),
                    "gated tahni scene {g} is not a playable entry"
                );
            }
        }
    }
}
