//! Adults-only scene gating.
//!
//! The original Deskmates shipped a handful of nude scenes (and the whole
//! adult line). They stay in the asset set but are hidden from every random
//! pool (idle, Next scene, agent stage directions) unless the user enables
//! "Adults-only content" in the tray Settings window.
//!
//! Curated by visual inspection of 4-frame strips sampled across every
//! scene timeline (`cargo test audit_tahni -- --ignored`, 2026-10-11):
//! exposed breasts/genitals gate; butt-only views and bikinis do not.

/// Scene names considered adults-only. Scene names are character-prefixed
/// and globally unique within a character's asset set.
pub const ADULT_SCENES: &[&str] = &[
    // Tahni (2D)
    "shower",              // rear nude shower
    "tahni_tease",         // full-frontal tease with towel
    "tahni_underwater",    // topless swim
    "swimming_pool",       // nude male swimmer
    "surfing",             // nude male surfer
    "waking_up",           // nude from behind
    "balloons",            // balloons pop, full frontal reveal
    "bath",                // topless in the bath
    "busty",               // breasts spill out of top
    "cake",                // topless faceplant
    "deckchair",           // nude carrying chair
    "marilyn",             // grate scene ends nude
    "mermaid",             // topless in the water
    "tahni_fashion_shoot", // topless star-cover frame
    // Maeka
    "maeka_creeper", // topless frames mid-scene
    "maeka_dancer",  // topless
    "maeka_emerge",  // full frontal from the hole
    // Kahli
    "kahli_s_dive_a", // topless swim
    "kahli_s_dive_b", // topless swim
    // FatB
    "fb_s_singe", // original ships crotch-mosaic: nude
];

pub fn is_adult(scene: &str) -> bool {
    ADULT_SCENES.contains(&scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gated_scenes_exist_in_shipped_assets() {
        // Every gated name must actually exist in its character's packed
        // scene set, otherwise the blocklist silently rots.
        let mut checked = 0;
        for c in ["tahni", "maeka", "kahli", "fatb"] {
            let dir = format!(
                "{}/assets/characters/{c}/scenes",
                env!("CARGO_MANIFEST_DIR")
            );
            for s in ADULT_SCENES {
                if std::path::Path::new(&format!("{dir}/{s}.bin.gz")).exists() {
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, ADULT_SCENES.len(), "some gated scenes missing from assets");
    }
}
