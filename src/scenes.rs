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
}

fn one() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
pub struct Scene {
    pub name: String,
    pub entries: Vec<String>,
    pub defs: HashMap<String, Vec<Def>>,
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
    cache: Mutex<HashMap<(String, u32), Option<RgbaImage>>>,
}

impl Scenes {
    /// Window canvas = the most common frame size across scenes (logo/banner
    /// scenes may differ; they are rare). Falls back to the classic 200x200.
    pub fn frame_size(&self) -> (u32, u32) {
        let mut counts: std::collections::HashMap<(u32, u32), usize> = Default::default();
        for s in self.scenes.values() {
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
            for (k, body) in defs {
                let toks = faz::parse_reflist(&body);
                let v = toks
                    .into_iter()
                    .map(|t| match t {
                        faz::Tok::Frame { id, x, y, rep } => {
                            Def { id, x, y, rep, name: String::new() }
                        }
                        faz::Tok::Name { name, rep } => Def { id: 0, x: 0, y: 0, rep, name },
                    })
                    .collect();
                defmap.insert(k, v);
            }
            let name = f.name.clone();
            scenes.insert(name.clone(), Scene { name: name.clone(), entries, defs: defmap, w, h });
            fazs.insert(name, f);
        }
        if scenes.is_empty() {
            return Err(format!("{}: no scenes parsed", sdir.display()));
        }
        Ok(Scenes { dir: dir.to_path_buf(), scenes, faz: fazs, cache: Mutex::new(HashMap::new()) })
    }

    pub fn frame(&self, scene: &str, id: u32) -> Option<RgbaImage> {
        let key = (scene.to_string(), id);
        if let Some(hit) = self.cache.lock().get(&key) {
            return hit.clone();
        }
        let img = self.faz.get(scene).and_then(|f| faz::decode_frame(f, id as u16));
        self.cache.lock().insert(key, img.clone());
        img
    }

    /// All entry-point scene names across all scenes (idle pool).
    pub fn all_entries(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for s in self.scenes.values() {
            for e in &s.entries {
                out.push((s.name.clone(), e.clone()));
            }
        }
        out.sort();
        out
    }
}
