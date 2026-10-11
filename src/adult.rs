//! Adults-only scene gating.
//!
//! The original Deskmates shipped a handful of nude scenes (and the whole
//! adult line). They stay in the asset set but are hidden from every random
//! pool (idle, Next scene, agent stage directions) unless the user enables
//! "Adults-only content" in the tray Settings window.
//!
//! Curated by visual inspection of 4-frame contact sheets for every scene
//! (`cargo test sheet_dump -- --ignored`) plus full-size 6-frame strips for
//! every borderline scene (`cargo test borderline_strips -- --ignored`,
//! 2026-10-11): exposed breasts/genitals gate; butt-only views, topless
//! males, and bikinis/high-cut leotards without exposure do not.

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
    "sailboard",           // topless on the board
    "tarzan",              // nude male (Tarzan) full-frontal
    // Maeka
    "maeka_creeper", // topless frames mid-scene
    "maeka_dancer",  // topless
    "maeka_emerge",  // full frontal from the hole
    // Kahli
    "kahli_s_dive_a", // topless swim
    "kahli_s_dive_b", // topless swim
    "kahli_s_barbarella", // topless frames mid-scene
    "kahli_s_gun",        // topless with rifle
    // FatB
    "fb_s_singe",  // original ships crotch-mosaic: nude
    "fb_s_streak", // streaker with crotch mosaic
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
