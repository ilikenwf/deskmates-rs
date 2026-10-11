//! Scene/asset loading: `scenes.json` (from analysis/faz_convert.py) plus
//! lazily-decoded frame PNGs (`frames/<scene>/<id>.png`).
//!
//! The JSON keeps the original FAZ script structure: each scene has entry
//! points and named defs; a def body is a token list of frame refs
//! (`{id,x,y,rep}`) and sub-def refs (`{name,rep}`).

use image::RgbaImage;
use serde::Deserialize;
use std::collections::HashMap;
use parking_lot::Mutex;
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
}

#[derive(Debug, Deserialize)]
struct ScenesFile(HashMap<String, Scene>);

pub struct Scenes {
    pub dir: PathBuf,
    pub scenes: HashMap<String, Scene>,
    /// scene -> (frame id -> decoded RGBA). Frames are 200x200 indexed PNGs;
    /// decoded once on first use and kept for the process lifetime.
    cache: Mutex<HashMap<(String, u32), Option<RgbaImage>>>,
}

/// Frame canvas size baked into the FAZ format.
pub const FRAME: u32 = 200;

impl Scenes {
    pub fn load(dir: &Path) -> Result<Scenes, String> {
        let raw = std::fs::read(dir.join("scenes.json")).map_err(|e| format!("scenes.json: {e}"))?;
        let ScenesFile(scenes) = serde_json::from_slice(&raw).map_err(|e| format!("scenes.json: {e}"))?;
        Ok(Scenes { dir: dir.to_path_buf(), scenes, cache: Mutex::new(HashMap::new()) })
    }

    pub fn frame(&self, scene: &str, id: u32) -> Option<RgbaImage> {
        let key = (scene.to_string(), id);
        if let Some(hit) = self.cache.lock().get(&key) {
            return hit.clone();
        }
        let path = self.dir.join("frames").join(scene).join(format!("{id}.png"));
        let img = match image::open(&path) {
            Ok(im) => Some(im.to_rgba8()),
            Err(_) => None,
        };
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
