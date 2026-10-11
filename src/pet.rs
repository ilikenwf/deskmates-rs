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
use rand::seq::IndexedRandom;

const TIME_REP: u32 = 1000;
const MAX_DEPTH: usize = 24;

#[derive(Clone, Debug)]
pub enum Step {
    Frame { scene: String, id: u32, x: i32, y: i32 },
    Sub { scene: String, name: String, rep: u32 },
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
    pub fn new(scenes: Arc<Scenes>) -> Pet {
        let entries = scenes.all_entries();
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
        };
        pet.start_random();
        pet
    }

    pub fn sprite(&self) -> Option<&Sprite> {
        self.cur.as_ref()
    }

    pub fn walk_dir(&self) -> i8 {
        self.walk_dir
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
        if let Some((scene, entry)) = self.entries.choose(&mut rand::rng()).cloned() {
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
        let defs = sc.defs.get(name)?;
        let mut steps = Vec::with_capacity(defs.len());
        for d in defs {
            if !d.name.is_empty() {
                steps.push(Step::Sub { scene: scene.to_string(), name: d.name.clone(), rep: d.rep });
            } else {
                steps.push(Step::Frame { scene: scene.to_string(), id: d.id, x: d.x, y: d.y });
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
                        "exit_wait" => { self.frame_dur = Duration::from_millis(900); return true; }
                        "home_wait" => { self.frame_dur = Duration::from_millis(1600); return true; }
                        "pause" => { self.frame_dur = Duration::from_millis(1200); return true; }
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
                    // unknown builtin (0003-style missing common list): hold
                    if self.scenes.scenes.get(&scene).and_then(|s| s.defs.get(&name)).is_none() {
                        self.frame_dur = Duration::from_millis(120);
                        return true;
                    }
                    if self.stack.len() >= MAX_DEPTH {
                        continue;
                    }
                    let Some(steps) = self.steps_for(&scene, &name) else { continue };
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
            }
        }
    }
}
