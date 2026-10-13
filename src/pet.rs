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

/// A screen-edge walk transition requested by a `fence_*` def.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fence {
    /// Horizontal travel: -1 left, 0 vertical-only, +1 right.
    pub dx: i8,
    /// Vertical travel: -1 up, 0 horizontal-only, +1 down.
    pub dy: i8,
    /// true: window slides from the edge to its rest position.
    pub entering: bool,
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
    /// Active screen-edge transition (`fence_*`): direction the window
    /// must travel and whether it enters (edge -> rest) or exits
    /// (rest -> edge). The host slides the window from this.
    fence: Option<Fence>,
    /// Idle gap: when to start the next random scene.
    next_at: Option<Instant>,
    speed: f64,
    /// Shuffle-bag over `entries`: every scene plays once per bag before
    /// any repeats (fixes "always the same three scenes").
    bag: Vec<usize>,
    bag_at: usize,
    /// A ` !name` token fired since the last `take_sound`.
    pending_sound: Option<String>,
    /// Scene names played during the inter-scene gap instead of blanking
    /// (user-classified "interstitial" scenes from the config).
    pub interstitial: Vec<String>,
    /// 0: next scene after the gap is a real scene (or first filler);
    /// 1: a filler just played, so the next gap plays a real scene.
    gap_phase: u8,
    /// Debug (`--scene NAME`): loop one scene forever, no random pool.
    pub scene_loop: Option<String>,
    /// Bubble text for a fired ambient line (host consumes via take_talk).
    pending_talk: Option<String>,
}

/// Scenes that read as touch reactions (the stripped touch.FAS mapped
/// clicks to these; curated from scene semantics).
const REACTIONS: &[&str] = &[
    "bulldog_chased", "unwanted_kiss", "shock", "karate", "gstring", "busty",
    "tahni_tease", "kiss_screen", "lightning", "lippy", "shake_a", "cupid",
    "balloons", "rubber_duck",
];

/// The original ticked at `1000/fps` ms with fps = 10: **100 ms per
/// frame** (PLAYER_RE §1). `speed` is the `all_slow`/`all_fast` modifier.
fn frame_dur_for(speed: f64) -> Duration {
    Duration::from_millis((100.0 / speed) as u64)
}

/// `dialog_i_can_kick_butt` -> `I can kick butt`: bubble text for an
/// ambient voice line (the FAS stores only the sound name).
fn humanize_dialog(def: &str) -> String {
    let words = def.trim_start_matches("dialog_").replace('_', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
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
            fence: None,
            next_at: None,
            speed: 1.0,
            bag: Vec::new(),
            bag_at: 0,
            pending_sound: None,
            interstitial: Vec::new(),
            gap_phase: 0,
            scene_loop: None,
            pending_talk: None,
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

    /// Take the bubble text for a fired ambient dialog line.
    pub fn take_talk(&mut self) -> Option<String> {
        self.pending_talk.take()
    }

    /// Random-pool of entry scenes minus adults-only ones unless enabled,
    /// plus the ambient `dialog_*` defs: the original had **no speech
    /// timer** — every completed script cycle drew one random def from
    /// the whole table (§1), so the voice lines ride the same shuffle-bag
    /// as the animations and fire once per cycle by pool share.
    ///
    /// The gate tests BOTH members of each (scene, entry) pair: an entry
    /// (sub-scene) name in the blocklist removes just that pair while the
    /// sibling entries of the same scene keep playing.
    fn pool(scenes: &Scenes, adult: bool) -> Vec<(String, String)> {
        Self::pool_with(scenes, adult, crate::adult::is_adult)
    }

    /// `pool` with an injectable gate predicate (entry-level gating tests).
    fn pool_with(
        scenes: &Scenes,
        adult: bool,
        gated: impl Fn(&str) -> bool,
    ) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = scenes
            .all_entries()
            .into_iter()
            .filter(|(s, e)| adult || !(gated(s) || gated(e)))
            .collect();
        v.extend(Self::dialog_defs(scenes));
        v
    }

    /// Orphaned `dialog_*` defs (ambient voice lines) as (scene, def)
    /// pool entries, minus the Gold-upgrade nag voice (adware, never
    /// ambient chatter — the `no_all_list.txt` exclusion analogue).
    fn dialog_defs(scenes: &Scenes) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = ["touch", "common"]
            .into_iter()
            .filter_map(|sn| scenes.scenes.get(sn).map(|s| (sn, s)))
            .flat_map(|(sn, s)| {
                s.raw
                    .keys()
                    .filter(|k| k.starts_with("dialog_"))
                    .map(move |k| (sn.to_string(), k.clone()))
            })
            .filter(|(_, k)| k != "dialog_buy_me_now")
            .collect();
        v.sort();
        v.dedup();
        v
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
    /// True while a scene's step stack is non-empty (mid-scene).
    pub fn playing(&self) -> bool {
        !self.stack.is_empty()
    }

    /// Active fence transition, if any.
    pub fn fence(&self) -> Option<Fence> {
        self.fence
    }

    /// Host calls when a fence slide finished: drop the transition and the
    /// walk hint it owned so the next def can set a new one.
    pub fn fence_done(&mut self) {
        self.fence = None;
        self.walk_dir = 0;
    }

    /// Host calls when a walk-out reached the screen edge: pop the walk
    /// loop levels so the character stops marching forever.
    pub fn abort_walk(&mut self) {
        while let Some(l) = self.stack.last() {
            if l.dir != 0 {
                self.stack.pop();
            } else {
                break;
            }
        }
        self.walk_dir = self.stack.iter().rev().find_map(|l| (l.dir != 0).then_some(l.dir)).unwrap_or(0);
        self.fence = None;
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
        // `%06x` of the hotspot color). Prefer whichever variant actually
        // carries a voice line (` !name` token): Tahni's `up` defs talk,
        // but Kahli's `up` is a bare blink while `down` holds her voice +
        // SFX sequence.
        let base = format!("T0x{color:06x}");
        let mut best: Option<(bool, std::cmp::Reverse<usize>, String)> = None;
        for (order, suf) in ["up", "down", "start"].iter().enumerate() {
            let def = format!("{base}{suf}");
            if let Some(body) = self.scenes.scenes.get("touch").and_then(|s| s.raw.get(&def)) {
                let voiced = body.windows(2).any(|w| w == b" !");
                let cand = (voiced, std::cmp::Reverse(order), def);
                if best.as_ref().map_or(true, |b| cand > *b) {
                    best = Some(cand);
                }
            }
        }
        if let Some((_, _, def)) = best {
            self.play("touch", &def);
            return;
        }
        self.touch();
    }

    /// Click on the sprite: interrupt with a touch reaction.
    pub fn touch(&mut self) {
        if let Some((scene, entry)) = self.reactions.choose(&mut rand::rng()).cloned() {
            self.play(&scene, &entry);
            return;
        }
        // Characters without curated reaction scenes (everyone but Tahni):
        // fire a voiced hotspot definition from their own touch.FAS.
        if let Some(s) = self.scenes.scenes.get("touch") {
            let voiced: Vec<String> = s
                .raw
                .keys()
                .filter(|k| k.starts_with("T0x") && s.raw.get(*k).is_some_and(|b| b.windows(2).any(|w| w == b" !")))
                .cloned()
                .collect();
            if let Some(def) = voiced.choose(&mut rand::rng()) {
                let def = def.clone();
                self.play("touch", &def);
            }
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
                    // One random def per completed cycle (§1): the bag
                    // over `entries` already contains the ambient
                    // `dialog_*` defs, so the next draw may be a voice
                    // line — no separate speech timer.
                    if let Some(s) = self.scene_loop.clone() {
                        if !self.play_named(&s) {
                            self.start_random();
                        }
                    } else if self.gap_phase == 0 && !self.interstitial.is_empty() {
                        // User-classified interstitial scenes fill the gap
                        // (classify candidates with --scenes/--scene NAME);
                        // the filler ending re-arms the gap for a real
                        // scene via gap_phase.
                        self.gap_phase = 1;
                        let s = self.interstitial
                            [rand::rng().random_range(0..self.interstitial.len())]
                            .clone();
                        if !self.play_named(&s) {
                            self.gap_phase = 0;
                            self.start_random();
                        }
                    } else {
                        self.gap_phase = 0;
                        self.start_random();
                    }
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
        // A `dialog_*` def drawn from the pool: bubble text (the FAS
        // stores only the sound name).
        if entry.starts_with("dialog_") && entry != "dialog_buy_me_now" {
            self.pending_talk = Some(humanize_dialog(entry));
        }
    }

    fn steps_for(&self, scene: &str, name: &str) -> Option<Vec<Step>> {
        let sc: &Scene = self.scenes.scenes.get(scene)?;
        // Re-roll weighted `{a|b%2}` choice groups fresh on every play.
        // Shared behavior defs (blink…) live in common.FAS but reference
        // the playing scene's frame bank, so fall back to common for the
        // def while frames stay resolved against `scene`.
        let body = sc
            .raw
            .get(name)
            .or_else(|| self.scenes.scenes.get("common").and_then(|c| c.raw.get(name)))?;
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
                    // The original never blanks between scenes: the sprite
                    // keeps its last frame through the idle gap until the
                    // next sequence's first frame replaces it. Clearing
                    // here was the inter-scene blink.
                    //
                    // §1/§5 inter-scene gap: the `pause` def holds 500 ms
                    // (`0000*5` at 10 fps) before the next weighted-random
                    // pick — keep the hold at ~500 ms, not longer.
                    self.next_at = Some(now + Duration::from_millis(500 + rand::rng().random_range(0..250)));
                    return false;
                }
                continue;
            }
            let step = lv.steps[lv.idx].clone();
            lv.idx += 1;
            match step {
                Step::Frame { scene, id, x, y } => {
                    // A frame token that doesn't decode — e.g. the
                    // common:0000 "return to base" marker, while common.FAS
                    // has no frame 0 — means "hold the current image": the
                    // original sprite never blanks mid-sequence.
                    if self.cur.is_none() || self.scenes.frame(&scene, id).is_some() {
                        self.cur = Some(Sprite { scene, id, x, y });
                    }
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
                    self.walk_dir = 0;
                    // §1/§5: ~500 ms `pause` hold, then the next draw.
                    self.next_at = Some(now + Duration::from_millis(500 + rand::rng().random_range(0..250)));
                    return false;
                }
                Step::Sub { scene, name, rep } => {
                    self.frame_dur = frame_dur_for(self.speed);
                    // A weighted `{dialog_hi|…}` list resolved to a dialog
                    // def: voice line + bubble text.
                    if name.starts_with("dialog_") && name != "dialog_buy_me_now" {
                        self.pending_talk = Some(humanize_dialog(&name));
                    }
                    match name.as_str() {
                        "all" => { self.speed = 1.0; continue; }
                        "all_slow" => { self.speed = 0.65; self.frame_dur = frame_dur_for(self.speed); continue; }
                        "all_fast" => { self.speed = 1.5; self.frame_dur = frame_dur_for(self.speed); continue; }
                        _ => {}
                    }
                    if name.starts_with("fence_") {
                        // Screen-edge transition: the window slides
                        // between a screen edge and its rest position.
                        // Suffix _in / _out gives the travel direction.
                        // Edge letters appear in either order (fence_lb
                        // and fence_tl both exist) and `fs` (front stage,
                        // bottom-center) matches neither position slot, so
                        // scan the name instead of reading fixed bytes.
                        let edge = name
                            .trim_start_matches("fence_")
                            .trim_end_matches("_in")
                            .trim_end_matches("_out");
                        let dx = if edge.contains('l') {
                            1
                        } else if edge.contains('r') {
                            -1
                        } else {
                            0
                        };
                        let dy = if edge.contains('t') {
                            1
                        } else if edge.contains('b') || edge.contains('f') {
                            -1
                        } else {
                            0
                        };
                        let entering = name.ends_with("_in");
                        self.fence = Some(Fence { dx, dy, entering });
                        self.walk_dir = dx;
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

    /// Ambient `dialog_*` defs ride the random scene pool: the original
    /// fired them with the one-random-def-per-completed-script draw (§1),
    /// not a timer. The Gold-upgrade nag stays excluded from the pool.
    #[test]
    fn dialog_defs_join_the_pool() {
        let sc = crate::scenes::Scenes::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/characters/tahni"),
        )
        .unwrap();
        let pool = Pet::pool(&sc, false);
        let n = pool.iter().filter(|(_, e)| e.starts_with("dialog_")).count();
        assert!(n >= 10, "expected ambient dialog defs in the pool, got {n}");
        assert!(!pool.iter().any(|(_, e)| e == "dialog_buy_me_now"));
    }

    /// Entry-level gating: an entry (sub-scene) name in the blocklist
    /// removes exactly that (scene, entry) pair from the default pool —
    /// the sibling entries of the same scene keep playing.
    #[test]
    fn entry_level_gate_removes_only_the_named_pair() {
        let sc = crate::scenes::Scenes::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/characters/tahni"),
        )
        .unwrap();
        // A real multi-entry scene to gate one entry of.
        let scene = sc
            .scenes
            .values()
            .find(|s| !sc.behaviors.contains(&s.name) && s.entries.len() >= 2)
            .expect("need a scene with >= 2 entries");
        let gated = scene.entries[1].clone();
        let pool = Pet::pool_with(&sc, false, |n| n == gated);
        assert!(
            !pool.iter().any(|(s, e)| *s == scene.name && *e == gated),
            "gated entry {gated} leaked into the default pool"
        );
        for e in &scene.entries {
            if *e == gated {
                continue;
            }
            assert!(
                pool.iter().any(|(s, x)| *s == scene.name && *x == *e),
                "sibling entry {e} of {} was wrongly removed",
                scene.name
            );
        }
        // Adult mode still ships the gated pair.
        let on = Pet::pool_with(&sc, true, |n| n == gated);
        assert!(on.iter().any(|(s, e)| *s == scene.name && *e == gated));
    }
}
