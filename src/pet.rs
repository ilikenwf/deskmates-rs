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
use std::collections::VecDeque;

const TIME_REP: u32 = 1000;

/// Probability a random draw takes the adult subpool when the gate is
/// open and `prefer_adult` is on (config `prefer_adult`).
const ADULT_PREF_P: f64 = 0.6;

/// D4 "dig deep": hand out bag slot `at`, but when that pair is in the
/// recent-play ring, look up to 3 slots ahead for a fresher pair and
/// swap it into `at` — the skipped pair keeps its turn, so the
/// shuffle-bag spread is untouched. When everything in reach is recent
/// (a pool too small to dodge), the bag's own pick stands.
fn pick_fresh(
    entries: &[(String, String)],
    bag: &mut [usize],
    at: usize,
    recent: &VecDeque<(String, String)>,
) -> usize {
    let first = bag[at];
    if recent.iter().any(|p| *p == entries[first]) {
        for k in 1..=3 {
            if at + k >= bag.len() {
                break;
            }
            let cand = bag[at + k];
            if !recent.iter().any(|p| *p == entries[cand]) {
                bag.swap(at, at + k);
                return cand;
            }
        }
    }
    first
}

/// Push a played pair onto the anti-replay ring: newest first, deduped,
/// capped at `cap` (production uses `min(20, pool/2)`). In-memory only.
fn push_recent(
    recent: &mut VecDeque<(String, String)>,
    pair: (String, String),
    cap: usize,
) {
    recent.retain(|p| *p != pair);
    recent.push_front(pair);
    while recent.len() > cap {
        recent.pop_back();
    }
}
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
    /// `fence_fs` / `fence_home`: the original grows the window rect to
    /// the full work area and moves the sprite inside it. Our window
    /// tracks the sprite, so the host settles it at the rest anchor
    /// instead of sliding it anywhere (PLAYER_RE §2).
    pub fullscreen: bool,
}

/// Decode a `fence_*` def name into a window transition (PLAYER_RE §2).
/// Edge letters appear in either order (`fence_lb` and `fence_tl` both
/// exist); `fs` (front stage) / `home` are full-work-area rects.
pub fn fence_parse(name: &str) -> Fence {
    let edge = name
        .trim_start_matches("fence_")
        .trim_end_matches("_in")
        .trim_end_matches("_out");
    let fullscreen = matches!(edge, "fs" | "home" | "full_screen");
    let dx = if fullscreen {
        0
    } else if edge.contains('l') {
        1
    } else if edge.contains('r') {
        -1
    } else {
        0
    };
    let dy = if fullscreen {
        0
    } else if edge.contains('t') {
        1
    } else if edge.contains('b') || edge.contains('f') {
        -1
    } else {
        0
    };
    Fence { dx, dy, entering: name.ends_with("_in"), fullscreen }
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
    /// With `scene_loop`: loop that exact entry (sub-scene) instead of
    /// the scene's first entry point (set by the scene browser).
    pub scene_loop_entry: Option<String>,
    /// Current adults-only gate state (mirrors the config; gates the
    /// prefer-adult bias).
    adult: bool,
    /// With `adult`: each random draw takes an adults-only pair with
    /// [`ADULT_PREF_P`] probability when the subpool has candidates.
    prefer_adult: bool,
    /// D4 anti-replay ring: the last `min(20, pool/2)` played
    /// (scene, entry) pairs, newest first. In-memory only.
    recent: VecDeque<(String, String)>,
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
            scene_loop_entry: None,
            adult,
            prefer_adult: false,
            recent: VecDeque::new(),
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
        self.adult = adult;
        self.entries = Self::pool(&self.scenes, adult);
        self.bag.clear();
        self.bag_at = 0;
    }

    /// Toggle the prefer-adult rotation bias (config `prefer_adult`).
    pub fn set_prefer_adult(&mut self, on: bool) {
        self.prefer_adult = on;
    }

    /// Pool indices whose scene OR entry name is gated (the adult
    /// subpool the prefer-adult bias draws from).
    fn adult_pool(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, (s, e))| crate::adult::is_adult(s) || crate::adult::is_adult(e))
            .map(|(i, _)| i)
            .collect()
    }

    /// Does this draw go to the adult subpool? `roll` is the injected
    /// random value (0..1) so the bias is deterministically testable.
    fn adult_pref_roll(&self, roll: f64) -> bool {
        self.adult && self.prefer_adult && roll < ADULT_PREF_P && !self.adult_pool().is_empty()
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
    /// Earliest instant the host must tick again: next frame advance, or
    /// the idle-gap pick. `None` = no frame clock and no pending gap.
    pub fn next_wake(&self) -> Option<Instant> {
        if self.stack.is_empty() {
            return self.next_at;
        }
        Some(self.frame_at + self.frame_dur)
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
            // Adults-only hotspot reactions stay behind the gate. The
            // state aliases (`T0x……..up/down/start`) embed the animation
            // frames directly — no textual link to the named def — so
            // the gate keys on the hotspot *color*: a color is gated
            // when its named def (`T0x…….._Name`) is. A gated zone
            // click falls back to a safe random reaction.
            if self.adult || !Self::hotspot_gated(&base) {
                self.play("touch", &def);
                return;
            }
        }
        self.touch();
    }

    /// True when the hotspot color `base` (`T0x` + 6 hex) belongs to a
    /// gated named reaction.
    fn hotspot_gated(base: &str) -> bool {
        crate::adult::ADULT_TOUCH
            .iter()
            .any(|g| g.strip_prefix(base).is_some_and(|rest| rest.starts_with('_')))
    }
    /// Play a touch-reaction definition by exact name (review-browser
    /// `touch:<def>` handoff). Returns false when the character has no
    /// such touch def.
    pub fn play_touch_def(&mut self, def: &str) -> bool {
        if self
            .scenes
            .scenes
            .get("touch")
            .is_some_and(|s| s.raw.contains_key(def))
        {
            self.play("touch", def);
            true
        } else {
            false
        }
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
                // Adults-only reactions stay behind the gate, keyed on
                // the hotspot color: aliases (`T0x……..down`) embed the
                // frames directly, so match the `T0x` + 6 hex prefix
                // against the gated named defs.
                .filter(|k| {
                    self.adult
                        || (!crate::adult::is_adult_touch(k)
                            && !Self::hotspot_gated(&k.chars().take(9).collect::<String>()))
                })
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

    /// Play an exact (scene, entry) pair — the scene browser's row click.
    /// Bypasses the shuffle bag and the gated pool (review shows gated
    /// scenes); an empty `entry` plays the scene's first entry point.
    pub fn play_pair(&mut self, scene: &str, entry: &str) -> bool {
        let Some(sc) = self.scenes.scenes.get(scene) else {
            return false;
        };
        let found = if entry.is_empty() {
            sc.entries.first()
        } else {
            sc.entries.iter().find(|e| *e == entry)
        };
        match found {
            Some(e) => {
                let e = e.clone();
                self.play(scene, &e);
                true
            }
            None => false,
        }
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
                        let ok = match self.scene_loop_entry.clone() {
                            Some(e) => self.play_pair(&s, &e),
                            None => self.play_named(&s),
                        };
                        if !ok {
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
        // Prefer-adult bias: with the gate open and the bias on, 60% of
        // draws come straight from the adult subpool when it has
        // candidates; everything else rides the normal shuffle bag.
        if self.adult_pref_roll(rand::rng().random::<f64>()) {
            let sub = self.adult_pool();
            let idx = sub[rand::rng().random_range(0..sub.len())];
            let (scene, entry) = self.entries[idx].clone();
            self.play(&scene, &entry);
            return;
        }
        if self.bag_at >= self.bag.len() {
            self.bag = (0..self.entries.len()).collect();
            self.bag.shuffle(&mut rand::rng());
            self.bag_at = 0;
        }
        // "Dig deep": a bag pick that was just played swaps with a
        // fresher pair up to 3 slots ahead, so back-to-back repeats
        // thin out without breaking the shuffle-bag spread.
        let idx = pick_fresh(&self.entries, &mut self.bag, self.bag_at, &self.recent);
        self.bag_at += 1;
        if let Some((scene, entry)) = self.entries.get(idx).cloned() {
            self.play(&scene, &entry);
        }
    }

    fn play(&mut self, scene: &str, entry: &str) {
        self.stack.clear();
        self.walk_dir = 0;
        self.speed = 1.0;
        self.next_at = None;
        let cap = 20usize.min(self.entries.len() / 2);
        push_recent(&mut self.recent, (scene.to_string(), entry.to_string()), cap);
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
                        // Fence defs are window-rect anchors, not
                        // off-screen parks (PLAYER_RE §2): `fence_lb_out`
                        // = ©[-200,200]0l0b,[0,0]0l0b© slides the window
                        // by per-frame deltas until its corner rests AT
                        // the bottom-left of the work area — fully
                        // visible. `_in` slides from that edge anchor
                        // inward (toward rest). `fs`/`home` are full
                        // work-area rects: the original grows the window
                        // and moves the sprite inside it; our window
                        // tracks the sprite, so settle at rest instead.
                        let f = fence_parse(&name);
                        self.fence = Some(f);
                        self.walk_dir = if f.fullscreen { 0 } else { f.dx };
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

    /// D3: the prefer-adult bias fires on 60% of draws (deterministic
    /// roll bounds + a seeded 1000-draw ratio check), only while the
    /// gate itself is open.
    #[test]
    fn prefer_adult_roll_bounds_and_ratio() {
        let sc = crate::scenes::Scenes::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/characters/tahni"),
        )
        .unwrap();
        let mut pet = Pet::new(std::sync::Arc::new(sc), true);
        assert!(!pet.adult_pool().is_empty(), "tahni ships gated scenes");
        // Prefer off, or gate off: never biased.
        pet.set_prefer_adult(false);
        assert!(!pet.adult_pref_roll(0.0));
        pet.set_adult(false);
        pet.set_prefer_adult(true);
        assert!(!pet.adult_pref_roll(0.0));
        // Gate on + prefer on: rolls below 0.6 hit, at/above miss.
        pet.set_adult(true);
        assert!(pet.adult_pref_roll(0.599));
        assert!(!pet.adult_pref_roll(0.6));
        assert!(!pet.adult_pref_roll(0.999));
        // 1000 seeded draws (xorshift LCG) land near the 60% target.
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        let hits = (0..1000).filter(|_| pet.adult_pref_roll(next())).count();
        assert!((500..=700).contains(&hits), "expected ~600/1000 adult-preferred draws, got {hits}");
    }

    /// Touch-reaction gating: with the gate closed, a random touch
    /// reaction must never resolve to a gated hotspot color (named def
    /// OR its state aliases, which embed the frames directly); with the
    /// gate open, gated reactions become reachable again.
    #[test]
    fn touch_reactions_respect_the_gate() {
        let sc = crate::scenes::Scenes::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/characters/kahli"),
        )
        .unwrap();
        // Color-prefix predicate: gated named def colors yes, plain
        // hotspot colors no.
        assert!(Pet::hotspot_gated("T0x454545")); // Secret_A_Lap_Dance
        assert!(!Pet::hotspot_gated("T0x00ff00")); // Bum_Bump stays safe

        let mut pet = Pet::new(std::sync::Arc::new(sc), false);
        for _ in 0..200 {
            pet.touch();
            let last = pet.recent.front().expect("touch played nothing");
            if last.0 == "touch" {
                assert!(
                    !Pet::hotspot_gated(&last.1.chars().take(9).collect::<String>()),
                    "gated touch reaction {} leaked with the gate closed",
                    last.1
                );
            }
        }
        pet.set_adult(true);
        let mut saw_gated = false;
        for _ in 0..200 {
            pet.touch();
            if let Some(l) = pet.recent.front() {
                if l.0 == "touch" && Pet::hotspot_gated(&l.1.chars().take(9).collect::<String>()) {
                    saw_gated = true;
                }
            }
        }
        assert!(saw_gated, "gate open but no gated touch reaction ever played");
    }
}

#[cfg(test)]
mod dig_tests {
    use super::*;

    fn pool3() -> Vec<(String, String)> {
        (0..3).map(|i| (format!("s{i}"), format!("e{i}"))).collect()
    }

    /// Pool of 3 pairs, ring N=2: no pair repeats within 2 consecutive
    /// plays — the dig-deep resample always finds the third pair.
    #[test]
    fn small_pool_no_repeat_within_two_plays() {
        let entries = pool3();
        // Deterministic stand-ins for the bag shuffle, cycled forever.
        let cycles = [[0usize, 1, 2], [2, 0, 1], [1, 2, 0], [0, 2, 1]];
        let mut recent: VecDeque<(String, String)> = VecDeque::new();
        let mut played: Vec<usize> = Vec::new();
        let mut bag = cycles[0].to_vec();
        let mut at = 0usize;
        for step in 0..24 {
            if at >= bag.len() {
                bag = cycles[(step % (cycles.len() - 1)) + 1].to_vec();
                at = 0;
            }
            let idx = pick_fresh(&entries, &mut bag, at, &recent);
            at += 1;
            played.push(idx);
            push_recent(&mut recent, entries[idx].clone(), 2);
        }
        // Ring N=2, pool 3: no pair repeats within 2 consecutive plays.
        for i in 1..played.len() {
            assert_ne!(played[i], played[i - 1], "immediate repeat: {played:?}");
        }
        for i in 2..played.len() {
            assert_ne!(played[i], played[i - 2], "repeat within 2 plays: {played:?}");
        }
    }

    /// Large pool: consecutive shuffled bag cycles still spread — no
    /// immediate repeats and nothing starves over 500 seeded draws.
    #[test]
    fn large_pool_spreads_across_cycles() {
        let n = 50usize;
        let entries: Vec<(String, String)> =
            (0..n).map(|i| (format!("s{i}"), format!("e{i}"))).collect();
        let mut state = 0x853C49E6748FBAD4u64;
        let mut rnd = |m: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as usize % m
        };
        let mut recent: VecDeque<(String, String)> = VecDeque::new();
        let mut counts = vec![0usize; n];
        let mut prev: Option<usize> = None;
        let mut draws = 0;
        while draws < 500 {
            let mut bag: Vec<usize> = (0..n).collect();
            for i in (1..n).rev() {
                let j = rnd(i + 1);
                bag.swap(i, j);
            }
            let mut at = 0;
            while at < bag.len() && draws < 500 {
                let idx = pick_fresh(&entries, &mut bag, at, &recent);
                at += 1;
                draws += 1;
                if let Some(p) = prev {
                    assert_ne!(p, idx, "immediate repeat in a {n}-pair pool");
                }
                prev = Some(idx);
                counts[idx] += 1;
                push_recent(&mut recent, entries[idx].clone(), 20.min(n / 2));
            }
        }
        assert!(counts.iter().all(|c| *c > 0), "a pair starved over 500 draws: {counts:?}");
        let min = counts.iter().min().unwrap();
        let max = counts.iter().max().unwrap();
        assert!(*max <= *min * 4, "rotation lopsided: min {min} max {max}");
    }
}

#[cfg(test)]
mod fence_tests {
    use super::*;

    /// PLAYER_RE §2 grammar: corner fences travel toward/away from the
    /// named edge (dx/dy point INWARD for `_in`), `_in` slides to rest,
    /// `fs`/`home` are full-rect settles with no travel.
    #[test]
    fn fence_grammar() {
        assert_eq!(fence_parse("fence_lb_in"), Fence { dx: 1, dy: -1, entering: true, fullscreen: false });
        assert_eq!(fence_parse("fence_lb_out"), Fence { dx: 1, dy: -1, entering: false, fullscreen: false });
        assert_eq!(fence_parse("fence_rb_in"), Fence { dx: -1, dy: -1, entering: true, fullscreen: false });
        assert_eq!(fence_parse("fence_tl_out"), Fence { dx: 1, dy: 1, entering: false, fullscreen: false });
        // `c` horizontal center: no horizontal travel; `m` middle: none vertical.
        assert_eq!(fence_parse("fence_cm"), Fence { dx: 0, dy: 0, entering: false, fullscreen: false });
        assert_eq!(fence_parse("fence_cb_in"), Fence { dx: 0, dy: -1, entering: true, fullscreen: false });
        assert_eq!(fence_parse("fence_fs"), Fence { dx: 0, dy: 0, entering: false, fullscreen: true });
        assert_eq!(fence_parse("fence_fs_out"), Fence { dx: 0, dy: 0, entering: false, fullscreen: true });
        assert_eq!(fence_parse("fence_home"), Fence { dx: 0, dy: 0, entering: false, fullscreen: true });
        assert_eq!(fence_parse("fence_full_screen"), Fence { dx: 0, dy: 0, entering: false, fullscreen: true });
    }
}
