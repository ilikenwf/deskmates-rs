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
    /// scene -> (frame id -> decoded RGBA), kept for the process lifetime.
    cache: Mutex<HashMap<(String, u32), Option<std::sync::Arc<RgbaImage>>>>,
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
            // Original-author wiring bug (RC3): `all` points at
            // maeka_S021_Big_Dance_sequence_aa/_ab, which resolve via
            // the common.FAS fallback to sword-TRAINING frames. The
            // actual choreography (frames 4000-4154, visually verified)
            // lives in the orphaned trailing-underscore defs nothing
            // called. Wire the entry to the real dance.
            if name == "maeka_dancer" {
                const DANCE: &str = "{maeka_S021_Big_Dance_sequence_aa_|maeka_S021_Big_Dance_sequence_ab_},pause";
                if defmap.contains_key("maeka_S021_Big_Dance_sequence_aa_") {
                    let v = faz::parse_reflist(DANCE.as_bytes())
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
                    rawmap.insert("all".to_string(), DANCE.as_bytes().to_vec());
                    defmap.insert("all".to_string(), v);
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
        Ok(Scenes { dir: dir.to_path_buf(), scenes, faz: fazs, cache: Mutex::new(HashMap::new()), behaviors, touch: touch_map })
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
        self.cache.lock().insert(key, img.clone());
        img
    }

    /// Decode every frame of `scene` into the cache (called from a helper
    /// thread when a scene starts, so playback never decodes mid-frame).
    pub fn preload(&self, scene: &str) {
        let Some(f) = self.faz.get(scene) else { return };
        for id in f.frames.keys() {
            let key = (scene.to_string(), *id as u32);
            if self.cache.lock().contains_key(&key) {
                continue;
            }
            let img = faz::decode_frame(f, *id).map(std::sync::Arc::new);
            self.cache.lock().insert(key, img);
        }
    }

    /// Drop cached frames of every other scene (bounds memory to roughly
    /// the active scene; reaction frames re-decode on demand).
    pub fn evict_except(&self, keep: &str) {
        self.cache
            .lock()
            .retain(|(s, _), _| s == keep || self.behaviors.contains(s));
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
}

