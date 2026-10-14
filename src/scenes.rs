//! Scene/asset loading: the original FAZ scene containers, stored as
//! gzip-compressed FAZ bytes under `scenes/*.bin.gz` (see
//! `analysis/pack_assets.py`) and parsed natively by `crate::faz`.
//!
//! Each scene keeps the original FAZ script structure: entry points and
//! named defs; a def body is a token list of frame refs (`{id,x,y,rep}`)
//! and sub-def refs (`{name,rep}`).
 
use crate::faz;
use flate2::read::GzDecoder;
use image::RgbaImage;
use serde::Deserialize;
use std::collections::HashMap;
use parking_lot::Mutex;
use std::io::Read;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct Def {
    #[serde(default)]
    pub id: u32,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default = "one")]
    pub rep: u32,
    #[serde(default)]
    pub name: String,
    /// ` !name` sound token; non-empty on sound defs.
    #[serde(default)]
    pub sound: String,
}

fn one() -> u32 {
    1
}

/// Behavior script names loaded from `behaviors/` (see `load`).
pub const BEHAVIOR_NAMES: &[&str] = &[
    "common",
    "common_demo",
    "common_enter",
    "common_enter_demo",
    "touch",
    "touch_demo",
];

#[derive(Debug, Deserialize)]
pub struct Scene {
    pub name: String,
    pub entries: Vec<String>,
    pub defs: HashMap<String, Vec<Def>>,
    /// Raw def bodies (before tokenizing) so weighted `{a|b%2}` choice
    /// groups can be re-rolled every time the def plays.
    #[serde(default)]
    pub raw: HashMap<String, Vec<u8>>,
    /// Native frame size; 200x200 for classic FAZ, larger for 8bpp scenes.
    #[serde(default = "two_hundred")]
    pub w: u32,
    #[serde(default = "two_hundred")]
    pub h: u32,
}

fn two_hundred() -> u32 { 200 }

fn read_gunzip(path: &Path) -> Result<Vec<u8>, String> {
    let f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut gz = GzDecoder::new(f);
    let mut v = Vec::new();
    gz.read_to_end(&mut v).map_err(|e| e.to_string())?;
    Ok(v)
}

pub struct Scenes {
    // kept as part of the loader's public surface (asset dir it was loaded from)
    #[allow(dead_code)]
    pub dir: PathBuf,
    pub scenes: HashMap<String, Scene>,
    /// Parsed FAZ containers keyed by scene name; frames decode on demand.
    faz: HashMap<String, faz::Faz>,
    /// Decoded-frame LRU (bounded by `Lru::CAP` frames): playback decodes
    /// each frame at most once per cache residency, and scene changes never
    /// evict the frames the next scene shares (behaviors, base poses).
    cache: Mutex<Lru>,
    /// Names loaded from `behaviors/` (common.FAS etc.): def sources, never
    /// part of the random idle pool or the window-size vote.
    pub behaviors: std::collections::HashSet<String>,
    /// Click-hotspot map from touch.FAS (colors + bottom-up bitmap).
    pub touch: Option<faz::Touch>,
}

impl Scenes {
    /// Window canvas = the most common frame size across scenes (logo/banner
    /// scenes may differ; they are rare). Falls back to the classic 200x200.
    pub fn frame_size(&self) -> (u32, u32) {
        let mut counts: std::collections::HashMap<(u32, u32), usize> = Default::default();
        for s in self.scenes.values() {
            if self.behaviors.contains(&s.name) {
                continue;
            }
            *counts.entry((s.w, s.h)).or_default() += 1;
        }
        counts
            .into_iter()
            .max_by_key(|(_, n)| *n)
            .map(|(d, _)| d)
            .unwrap_or((200, 200))
    }
}


impl Scenes {
    /// Load every `scenes/*.bin.gz` FAZ container in `dir`, applying the
    /// same scene filter as `faz_convert.py`.
    pub fn load(dir: &Path) -> Result<Scenes, String> {
        let sdir = dir.join("scenes");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&sdir)
            .map_err(|e| format!("{}: {e}", sdir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "gz").unwrap_or(false))
            .collect();
        files.sort();
        let mut scenes: HashMap<String, Scene> = HashMap::new();
        let mut fazs: HashMap<String, faz::Faz> = HashMap::new();
        for path in files {
            let raw = match read_gunzip(&path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let f = match faz::parse_faz(&raw) {
                Some(f) => f,
                None => continue,
            };
            if !faz::scene_ok(&f) {
                continue;
            }
            let (w, h) = match f.dims {
                Some(d) => d,
                None => continue,
            };
            let (mut entries, defs) = faz::script_defs(&f);
            if faz::is_placeholder(&f) {
                // demo "?" placeholder: keep the art but never play it
                entries.clear();
            }
            let mut defmap: HashMap<String, Vec<Def>> = HashMap::new();
            let mut rawmap: HashMap<String, Vec<u8>> = HashMap::new();
            for (k, body) in defs {
                let toks = faz::parse_reflist(&body);
                let v = toks
                    .into_iter()
                    .map(|t| match t {
                        faz::Tok::Frame { id, x, y, rep } => {
                            Def { id, x, y, rep, name: String::new(), sound: String::new() }
                        }
                        faz::Tok::Name { name, rep } => {
                            Def { id: 0, x: 0, y: 0, rep, name, sound: String::new() }
                        }
                        faz::Tok::Sound { name } => {
                            Def { id: 0, x: 0, y: 0, rep: 1, name: String::new(), sound: name }
                        }
                    })
                    .collect();
                rawmap.insert(k.clone(), body);
                defmap.insert(k, v);
            }
            // The original upsold the paid "Deskmate Gold" upgrade with a
            // nag scene (` !buy_me_now` voice line). Adware: dropped.
            defmap.remove("dialog_buy_me_now");
            rawmap.remove("dialog_buy_me_now");
            // Key by filename stem, not the FAZ-internal name: the
            // classic-mode variants (classic_buggy.bin.gz …) carry the
            // original internal name `buggy` and must coexist with the
            // current scene instead of overwriting it in the map. For
            // every other shipped file the stem equals the internal name.
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.trim_end_matches(".bin.gz").to_string())
                .unwrap_or_else(|| f.name.clone());
            // Original-author wiring bug (RC3), maeka: the entry defs the
            // shipped `all` scripts name (`maeka_S021_Big_Dance_sequence_aa`
            // etc.) are stubs whose bodies are the
            // `{common_sword_training_long_flipped|…}` choice — every menu
            // entry of these scenes played the SAME sword-training frames
            // via the common.FAS fallback. The real choreography lives in
            // never-called trailing-underscore defs (`…_aa_`) that
            // reference each scene's own frame bank (ids 4000+, visually
            // verified). Rewire every stub to its orphaned body; `all`
            // needs no override because it names the stubs. (The earlier
            // maeka_dancer fix overrode `all` only — but the idle pool
            // plays the expanded entry names, so the stubs still played
            // sword frames.) maeka_dummy is deliberately NOT listed: its
            // only defs ARE the sword stubs and no orphaned alternative
            // exists — sword training is its original content, see
            // analysis/MAEKA_WIRING.md.
            const MAEKA_REAL_DEFS: &[(&str, &[(&str, &str)])] = &[
                (
                    "maeka_dancer",
                    &[
                        ("maeka_S021_Big_Dance_sequence_aa", "maeka_S021_Big_Dance_sequence_aa_"),
                        ("maeka_S021_Big_Dance_sequence_ab", "maeka_S021_Big_Dance_sequence_ab_"),
                    ],
                ),
                ("maeka_emerge", &[("maeka_CE005_startup", "maeka_CE005_startup_")]),
                (
                    "maeka_float",
                    &[
                        ("maeka_S002_sequence_aa", "maeka_S002_sequence_aa_"),
                        ("maeka_S002_sequence_ab", "maeka_S002_sequence_ab_"),
                    ],
                ),
                (
                    "maeka_form",
                    &[
                        ("maeka_S015_Training_Bare_Hands_aa", "maeka_S015_Training_Bare_Hands_aa_"),
                        ("maeka_S015_Training_Bare_Hands_ab", "maeka_S015_Training_Bare_Hands_ab_"),
                        ("maeka_S015_Training_Bare_Hands_ac", "maeka_S015_Training_Bare_Hands_ac_"),
                        ("maeka_S015_Training_Bare_Hands_ad", "maeka_S015_Training_Bare_Hands_ad_"),
                        ("maeka_S015_Training_Bare_Hands_ae", "maeka_S015_Training_Bare_Hands_ae_"),
                        ("maeka_S015_Training_Bare_Hands_af", "maeka_S015_Training_Bare_Hands_af_"),
                        ("maeka_S015_Training_Bare_Hands_ag", "maeka_S015_Training_Bare_Hands_ag_"),
                        ("maeka_S015_Training_Bare_Hands_ah", "maeka_S015_Training_Bare_Hands_ah_"),
                    ],
                ),
                (
                    "maeka_splash",
                    &[("maeka_splash_aa", "maeka_splash_aa_"), ("maeka_splash_ab", "maeka_splash_ab_")],
                ),
            ];
            if let Some(pairs) =
                MAEKA_REAL_DEFS.iter().find(|(s, _)| *s == name).map(|(_, p)| *p)
            {
                for (stub, orphan) in pairs {
                    if let Some(body) = rawmap.get(*orphan).cloned() {
                        let v = faz::parse_reflist(&body)
                            .into_iter()
                            .map(|t| match t {
                                faz::Tok::Frame { id, x, y, rep } => {
                                    Def { id, x, y, rep, name: String::new(), sound: String::new() }
                                }
                                faz::Tok::Name { name, rep } => {
                                    Def { id: 0, x: 0, y: 0, rep, name, sound: String::new() }
                                }
                                faz::Tok::Sound { name } => {
                                    Def { id: 0, x: 0, y: 0, rep: 1, name: String::new(), sound: name }
                                }
                            })
                            .collect();
                        rawmap.insert(stub.to_string(), body);
                        defmap.insert(stub.to_string(), v);
                    }
                }
            }
            scenes.insert(name.clone(), Scene { name: name.clone(), entries, defs: defmap, raw: rawmap, w, h });
            fazs.insert(name, f);
        }
        // Behavior scripts (behaviors/*.fas.gz): the original common.FAS /
        // common_enter.FAS / touch.FAS containers. They supply shared defs
        // (blink, pause pools, enter sequences) and their own frames, but
        // are never played as random scenes.
        let mut behaviors = std::collections::HashSet::new();
        let mut touch_map: Option<faz::Touch> = None;
        let bdir = dir.join("behaviors");
        for name in BEHAVIOR_NAMES {
            let path = bdir.join(format!("{name}.fas.gz"));
            let Ok(raw) = read_gunzip(&path) else { continue };
            let Some(f) = faz::parse_faz(&raw) else { continue };
            let Some((w, h)) = f.dims else { continue };
            let (entries, defs) = faz::script_defs(&f);
            let mut defmap: HashMap<String, Vec<Def>> = HashMap::new();
            let mut rawmap: HashMap<String, Vec<u8>> = HashMap::new();
            for (k, body) in defs {
                let v = faz::parse_reflist(&body)
                    .into_iter()
                    .map(|t| match t {
                        faz::Tok::Frame { id, x, y, rep } => {
                            Def { id, x, y, rep, name: String::new(), sound: String::new() }
                        }
                        faz::Tok::Name { name, rep } => {
                            Def { id: 0, x: 0, y: 0, rep, name, sound: String::new() }
                        }
                        faz::Tok::Sound { name } => {
                            Def { id: 0, x: 0, y: 0, rep: 1, name: String::new(), sound: name }
                        }
                    })
                    .collect();
                rawmap.insert(k.clone(), body);
                defmap.insert(k, v);
            }
            // Upsell nag voice line (` !buy_me_now`): adware, dropped.
            defmap.remove("dialog_buy_me_now");
            rawmap.remove("dialog_buy_me_now");
            scenes.insert(name.to_string(), Scene { name: name.to_string(), entries, defs: defmap, raw: rawmap, w, h });
            if *name == "touch" {
                touch_map = f.touch.clone();
            }
            fazs.insert(name.to_string(), f);
            behaviors.insert(name.to_string());
        }
        if scenes.is_empty() {
            return Err(format!("{}: no scenes parsed", sdir.display()));
        }
        Ok(Scenes { dir: dir.to_path_buf(), scenes, faz: fazs, cache: Mutex::new(Lru::new()), behaviors, touch: touch_map })
    }

    pub fn frame(&self, scene: &str, id: u32) -> Option<std::sync::Arc<RgbaImage>> {
        let key = (scene.to_string(), id);
        if let Some(hit) = self.cache.lock().get(&key) {
            return hit.clone();
        }
        let mut img = self.faz.get(scene).and_then(|f| faz::decode_frame(f, id as u16));
        if img.is_none() {
            // The original resolves frame tokens against a global frame
            // table: current scene first, then the shared behavior files.
            // That is how base poses (0000 stand, 0001/0002 blink, 0003
            // wait — all in common_demo.FAS) resolve when a scene script
            // references them without carrying its own copy.
            for b in BEHAVIOR_NAMES {
                if *b == scene {
                    continue;
                }
                if let Some(f) = self.faz.get(*b) {
                    if let Some(i2) = faz::decode_frame(f, id as u16) {
                        img = Some(i2);
                        break;
                    }
                }
            }
        }
        let img = img.map(std::sync::Arc::new);
        self.cache.lock().put(key, img.clone());
        img
    }


    /// Frames resident in the cache (tests).
    pub fn cache_len(&self) -> usize {
        self.cache.lock().map.len()
    }
    /// All entry-point scene names across all scenes (idle pool).
    pub fn all_entries(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for s in self.scenes.values() {
            if self.behaviors.contains(&s.name) {
                continue;
            }
            for e in &s.entries {
                out.push((s.name.clone(), e.clone()));
            }
        }
        out.sort();
        out
    }
}

/// Bounded decoded-frame cache: HashMap + recency deque. `CAP` frames of
/// 123x123 RGBA is ~75 MB worst case; scenes hold 2-12k frames each, so
/// the working set (current scene + behavior poses) always fits and
/// playback never re-decodes.
pub struct Lru {
    map: HashMap<(String, u32), Option<std::sync::Arc<RgbaImage>>>,
    order: VecDeque<(String, u32)>,
}

impl Lru {
    const CAP: usize = 640;

    fn new() -> Lru {
        Lru { map: HashMap::new(), order: VecDeque::new() }
    }

    fn get(&mut self, key: &(String, u32)) -> Option<&Option<std::sync::Arc<RgbaImage>>> {
        if self.map.contains_key(key) {
            self.touch(key);
        }
        self.map.get(key)
    }


    fn touch(&mut self, key: &(String, u32)) {
        self.order.retain(|k| k != key);
        self.order.push_back(key.clone());
    }

    fn put(&mut self, key: (String, u32), img: Option<std::sync::Arc<RgbaImage>>) {
        self.map.insert(key.clone(), img);
        self.touch(&key);
        while self.map.len() > Self::CAP {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_char(c: &str) -> Scenes {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("assets/characters/{c}"));
        Scenes::load(&root).expect("asset tree loads")
    }

    /// Classic mode: every scene named by `crate::classic` for a shipped
    /// character must parse into a playable scene (non-empty entries), not
    /// be silently skipped by the loader's scene filter.
    #[test]
    fn classic_scenes_ship_and_parse() {
        for c in crate::classic::CHARACTERS_WITH_CLASSICS {
            let s = load_char(c);
            for cs in crate::classic::classic_set(c) {
                let scn = s
                    .scenes
                    .get(cs.scene)
                    .unwrap_or_else(|| panic!("{c}: classic scene {} ({}) missing", cs.scene, cs.label));
                assert!(
                    !scn.entries.is_empty(),
                    "{c}: classic scene {} has no playable entries",
                    cs.scene
                );
                assert!(
                    !s.behaviors.contains(cs.scene),
                    "{c}: classic scene {} must be a scene, not a behavior bank",
                    cs.scene
                );
                // Entries alone are not enough: the entry points must
                // actually resolve to frames (the v1.04 demo cuts reuse
                // the shipped behavior banks for their base poses).
                let steps: usize = scn
                    .entries
                    .iter()
                    .filter(|e| scn.defs.contains_key(*e))
                    .count();
                assert!(steps > 0, "{c}: classic scene {} entries resolve to no defs", cs.scene);
                let drawn = s
                    .faz
                    .get(cs.scene)
                    .map(|f| f.frames.keys().filter(|id| s.frame(cs.scene, **id as u32).is_some()).count())
                    .unwrap_or(0);
                assert!(drawn > 0, "{c}: classic scene {} decodes no frames", cs.scene);
            }
        }
    }

    /// The classic variants carry the original FAZ-internal name of a scene
    /// that is still shipped (v1.04 `buggy` vs current `buggy`). They must
    /// coexist: the loader keys by filename stem, so neither overwrites the
    /// other and both keep their own frame bank.
    #[test]
    fn classic_variants_do_not_overwrite_current_scenes() {
        let s = load_char("tahni");
        for (current, classic) in [
            ("buggy", "classic_buggy"),
            ("dustbuster", "classic_dustbuster"),
            ("scuba", "classic_scuba"),
            ("snowboard", "classic_snowboard"),
            ("space_hopper", "classic_space_hopper"),
        ] {
            assert!(s.scenes.contains_key(current), "current {current} lost");
            assert!(s.scenes.contains_key(classic), "classic {classic} missing");
            assert_eq!(s.scenes.get(classic).unwrap().name, classic);
            let cf = s.faz.get(classic).expect("classic faz");
            let nf = s.faz.get(current).expect("current faz");
            assert_eq!(cf.name, nf.name, "variant must be the same original scene");
            assert!(
                cf.frames != nf.frames || cf.pairs != nf.pairs,
                "{current} and {classic} carry identical content"
            );
        }
    }
    /// The 10 palette-swap variant scenes (dark, dummy, firedance, …) were
    /// shipped frameless by the installer (truncated FAZ: script + palettes
    /// only; their frames live in the touch/common behavior masters). They
    /// are recovered by splicing the referenced frame blocks verbatim from
    /// those masters (`analysis/m001_unpack.py`); each must now load as a
    /// real playable scene: parse, non-placeholder, own frames for every
    /// frame token its entry defs reference, and decodable pixels.
    #[test]
    fn maeka_variants_recovered_and_playable() {
        const VARIANTS: &[&str] = &[
            "maeka_dark",
            "maeka_dummy",
            "maeka_firedance",
            "maeka_ignius",
            "maeka_knife",
            "maeka_rune",
            "maeka_splash",
            "maeka_strikes",
            "maeka_veridius",
            "maeka_white",
        ];
        let s = load_char("maeka");
        for v in VARIANTS {
            let f = s
                .faz
                .get(*v)
                .unwrap_or_else(|| panic!("{v}: scene missing from asset tree"));
            assert!(!faz::is_placeholder(f), "{v}: still a placeholder");
            let scn = s.scenes.get(*v).unwrap();
            assert!(!scn.entries.is_empty(), "{v}: no entries");
            // Every frame token referenced by the defs reachable from the
            // entry points (scene defs first, then behavior defs, like the
            // runtime) must resolve to a frame.
            let mut stack: Vec<String> = scn.entries.clone();
            let mut seen = std::collections::HashSet::new();
            let mut refs = Vec::new();
            while let Some(n) = stack.pop() {
                if !seen.insert(n.clone()) {
                    continue;
                }
                let defs = scn
                    .defs
                    .get(&n)
                    .or_else(|| {
                        s.behaviors
                            .iter()
                            .filter_map(|b| s.scenes.get(b))
                            .find_map(|b| b.defs.get(&n))
                    });
                for d in defs.into_iter().flatten() {
                    if d.id != 0 || !d.name.is_empty() {
                        refs.push((n.clone(), d.clone()));
                    }
                    if !d.name.is_empty() {
                        stack.push(d.name.clone());
                    }
                }
            }
            assert!(!refs.is_empty(), "{v}: entry defs reference nothing");
            for (def, d) in &refs {
                if !d.name.is_empty() {
                    continue; // named sub-defs: checked via the stack walk
                }
                assert!(
                    s.frame(v, d.id).is_some(),
                    "{v}: def {def} references frame {:04} that resolves nowhere",
                    d.id
                );
            }
            // Decoding must produce visible pixels somewhere (frame 0003
            // is a legitimate all-transparent timing pose, so scan until a
            // frame actually paints).
            let mut painted = false;
            for id in f.frames.keys() {
                if let Some(img) = s.frame(v, *id as u32) {
                    if img.pixels().any(|p| p[3] > 0) {
                        painted = true;
                        break;
                    }
                }
            }
            assert!(painted, "{v}: no frame decodes to visible pixels");
        }
    }

    /// Regression pin for the maeka stub rewiring (see MAEKA_REAL_DEFS in
    /// `load`): every fixed scene's entry-reachable defs must reference
    /// frames of its OWN bank, and those frames must not be the
    /// sword-training frames the stubs used to fall back to. The four
    /// `common_sword_training_*` defs in common.FAS reference ids 3..263
    /// which decode from common_demo.FAS; the fixed scenes must paint
    /// content outside that byte-set.
    #[test]
    fn maeka_rewired_scenes_are_not_sword_training() {
        let s = load_char("maeka");
        // Byte signatures of every frame the sword-training defs play.
        let sword = s.scenes.get("common").expect("common behavior");
        let mut sword_bytes: std::collections::HashSet<Vec<u8>> = Default::default();
        for def in [
            "common_sword_training_long",
            "common_sword_training_long_flipped",
            "common_sword_training_short",
            "common_sword_training_short_flipped",
        ] {
            let mut n = 0;
            for d in sword.defs.get(def).expect(def) {
                if d.id != 0 {
                    n += 1;
                    if let Some(img) = s.frame("common_demo", d.id) {
                        sword_bytes.insert(img.as_raw().clone());
                    }
                }
            }
            assert!(n > 0, "sword def {def} references no frames");
        }
        assert!(!sword_bytes.is_empty(), "no sword frames decoded");

        for scene in ["maeka_dancer", "maeka_emerge", "maeka_float", "maeka_form", "maeka_splash"] {
            let scn = s.scenes.get(scene).expect(scene);
            // No entry-reachable scene-local def may name the sword stubs.
            for (name, body) in &scn.raw {
                assert!(
                    !body.windows(14).any(|w| w == b"sword_training"),
                    "{scene}: def {name} still references sword training"
                );
            }
            // Walk defs reachable from the entries, descending only into
            // scene-local defs (the behavior-side scaffolding — enter/exit/
            // pause — is shared and out of scope for this pin).
            let mut stack: Vec<String> = scn.entries.clone();
            let mut seen = std::collections::HashSet::new();
            let mut foreign = 0usize;
            let mut total = 0usize;
            while let Some(n) = stack.pop() {
                if !seen.insert(n.clone()) {
                    continue;
                }
                let Some(defs) = scn.defs.get(&n) else { continue };
                for d in defs {
                    if !d.name.is_empty() {
                        stack.push(d.name.clone());
                    } else if d.id != 0 {
                        total += 1;
                        if let Some(img) = s.frame(scene, d.id) {
                            if !sword_bytes.contains(img.as_raw()) {
                                foreign += 1;
                            }
                        }
                    }
                }
            }
            assert!(total > 0, "{scene}: entries reference no frames at all");
            // The choreographies are 100+ unique frames; before the fix the
            // count was 0 (stubs referenced nothing of their own).
            assert!(
                foreign >= 50,
                "{scene}: only {foreign}/{total} entry frames differ from sword training"
            );
        }
    }
}

