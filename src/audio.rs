//! Sound playback for the ` !name` script tokens (rodio/cpal: WASAPI on
//! Windows, CoreAudio on macOS, ALSA on Linux — no native toolchain needed).
//!
//! The original played waves from one in-memory archive (`DeskMate.WAS`);
//! `analysis/extract_behaviors.py` split that archive into per-sound gzipped
//! WAVs under `assets/characters/<c>/sounds/`. Names in scripts are matched
//! case-insensitively, exactly like the original's `sounds[name.casefold()]`.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::sync::Mutex;

pub struct Audio {
    // Kept alive for the process lifetime; dropping it closes the device.
    sink: Option<rodio::MixerDeviceSink>,
    dir: PathBuf,
    cache: Mutex<HashMap<String, std::sync::Arc<Vec<u8>>>>,
}

impl Audio {
    /// Never fails: if no audio device exists, `play` becomes a no-op.
    pub fn new(dir: PathBuf) -> Audio {
        Audio {
            sink: rodio::DeviceSinkBuilder::open_default_sink().ok(),
            dir,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Play `NAME.wav.gz` from the character's sounds dir (fire-and-forget;
    /// overlapping sounds mix on the device).
    pub fn play(&self, name: &str) {
        let Some(sink) = &self.sink else { return };
        let key = name.trim().to_uppercase();
        if key.is_empty() {
            return;
        }
        let bytes = {
            let mut cache = self.cache.lock().unwrap();
            if let Some(b) = cache.get(&key) {
                b.clone()
            } else {
                let raw = std::fs::read(self.dir.join(format!("{key}.wav.gz"))).ok();
                let wav = raw.map(|r| {
                    let mut w = Vec::new();
                    let _ = flate2::read::GzDecoder::new(Cursor::new(r)).read_to_end(&mut w);
                    w
                });
                match wav {
                    Some(w) => {
                        let w = std::sync::Arc::new(w);
                        cache.insert(key, w.clone());
                        w
                    }
                    None => return,
                }
            }
        };
        use rodio::source::Source;
        if let Ok(src) = rodio::Decoder::new(Cursor::new(bytes.as_ref().clone())) {
            sink.mixer().add(src.amplify(0.7));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::Source;
    /// Every shipped wave archive must decode: spot-check a few WAVs per
    /// character (the scripts reference these by name at play time).
    #[test]
    fn waves_decode() {
        let chars = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/characters");
        let mut checked = 0;
        for c in std::fs::read_dir(&chars).unwrap() {
            let sdir = c.unwrap().path().join("sounds");
            let Ok(files) = std::fs::read_dir(&sdir) else { continue };
            let mut k = 0;
            for f in files {
                if k == 3 {
                    break;
                }
                let p = f.unwrap().path();
                let raw = std::fs::read(&p).unwrap();
                let mut wav = Vec::new();
                flate2::read::GzDecoder::new(Cursor::new(raw))
                    .read_to_end(&mut wav)
                    .unwrap();
                let src = rodio::Decoder::new(Cursor::new(wav)).expect(&p.display().to_string());
                drop(src.total_duration());
                checked += 1;
                k += 1;
            }
        }
        assert!(checked > 0);
    }
}

#[cfg(test)]
mod script_sounds {
    use super::*;
    use rodio::Source;
    /// The waves the shipped tahni scripts actually fire (` !hi`,
    /// ` !hi_baby`, ` !i_love_it_when_you_turn_me_on`, touch SFX) must be
    /// present and decodable.
    #[test]
    fn referenced_sounds_present() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/characters/tahni/sounds");
        for name in ["HI", "HI_BABY", "I_LOVE_IT_WHEN_YOU_TURN_ME_ON", "CAMERA_B", "EXPLOSION"] {
            let raw = std::fs::read(dir.join(format!("{name}.wav.gz"))).expect(name);
            let mut wav = Vec::new();
            flate2::read::GzDecoder::new(Cursor::new(raw))
                .read_to_end(&mut wav)
                .unwrap();
            let src = rodio::Decoder::new(Cursor::new(wav)).expect(name);
            drop(src.total_duration());
        }
    }
}
