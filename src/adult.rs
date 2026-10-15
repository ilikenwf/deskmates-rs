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

/// Names considered adults-only. Scene names are character-prefixed and
/// globally unique within a character's asset set. **Entry (sub-scene)
/// names are also valid here** (e.g. `kahli_s_action_b_a`, one entry of
/// the `kahli_s_action_b` scene): the random-pool filter tests both the
/// scene and the entry name of every (scene, entry) pair, so a single
/// sub-scene can be gated while its sibling entries keep playing. The
/// scene-browser (`--review`) marks and the classic/menu/tray sites are
/// scene-level only.
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
    // Added from the user --review walkthrough (2026-10-13):
    "beach",       // a voyeur leers at her — gated at user request
    "dumb_eric",   // user-gated
    "dumb_eric_b", // user-gated
    "bambi_flash", // classic-set Bambii spot — gated; the Classic submenu +
                   // tray disable it automatically when the gate is off
    // Maeka
    "maeka_creeper", // topless frames mid-scene
    "maeka_dancer",  // topless
    "maeka_emerge",  // full frontal from the hole
    // Kahli
    "kahli_s_dive_a", // topless swim
    "kahli_s_dive_b", // topless swim
    "kahli_s_barbarella", // topless frames mid-scene
    "kahli_s_gun",        // topless with rifle
    "kahli_s_action_b",   // user review 2026-10-13: topless
    "kahli_s_jabba",      // user review 2026-10-13: exposed breasts
    // FatB
    "fb_s_singe",  // original ships crotch-mosaic: nude
    "fb_s_streak", // streaker with crotch mosaic
];

/// Adults-only **touch reactions** (the `T0x…….._Name` hotspot defs in
/// each character's touch.FAS). Curated by visual inspection of 12-frame
/// strips for every named hotspot, voiced or silent
/// (`cargo test touch_strips -- --ignored`, 2026-10-14/15): same rule as
/// scenes — exposed breasts/genitals gate; pastie/band-covered
/// toplessness does NOT (Tahni's Secret_A_Tassles keeps its bra,
/// Knickers_Fall keeps the band over her pussy), butt-only views do not
/// (FatB's Pixel Moon, Tahni's Secret_C), and covered catsuit/leotard
/// teasing does not (Hot Touch, Bum Bump, Head Job, Mega_Guns).
/// Kahli's Basic Instinct DOES gate: the magnifying-glass zoom circle
/// shows exposed breasts; Tahni's Flash_Tits despite its name shows
/// fully exposed breasts.
/// These are def names, not scene names, so they live outside
/// `ADULT_SCENES` (whose existence test walks scene/entry containers).
pub const ADULT_TOUCH: &[&str] = &[
    // Kahli
    "T0x454545_Secret_A_Lap_Dance",          // full frontal on the chair
    "T0xa9a9a9_Secret_B_Pole_Dance",         // full frontal on the pole
    "T0xff40ff_Breast_R_A_Flasher_DEMO",   // coat-off full frontal (variant)
    "T0x80ffff_Breast_R_B_Flasher",         // coat-off full frontal
    "T0x8040ff_Breast_L_B_Shower",          // nude shower silhouette
    "T0xc0ffff_Breast_L_A_Shrink",           // shrinking bra, exposed
    "T0xff40ff_Breast_R_A_Ape_Shit",         // ape transformation nudity
    "T0xff0000_Crotch_Bottom_Basic_Instinct", // leg spread + breast zoom circle
    // Tahni
    "T0x408000_Secret_B_Flash_Tits",         // top comes off: exposed breasts
    // Maeka
    "T0x003b7f_Pubes",                       // full frontal
    "T0x660066_Breast_L_DEMO",               // bare breasts (pasties)
];

pub fn is_adult(scene: &str) -> bool {
    ADULT_SCENES.contains(&scene)
}

/// Gate predicate for touch-reaction defs.
pub fn is_adult_touch(def: &str) -> bool {
    ADULT_TOUCH.contains(&def)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gated_names_exist_in_shipped_assets() {
        // Every gated name must actually exist in a character's packed
        // asset set — either as a scene container or as an entry
        // (sub-scene) name inside one — otherwise the blocklist silently
        // rots.
        let mut checked = 0;
        for c in ["tahni", "maeka", "kahli", "fatb"] {
            let dir = format!(
                "{}/assets/characters/{c}/scenes",
                env!("CARGO_MANIFEST_DIR")
            );
            let scenes = crate::scenes::Scenes::load(
                std::path::Path::new(&dir).parent().unwrap(),
            )
            .unwrap();
            for s in ADULT_SCENES {
                let is_scene = std::path::Path::new(&format!("{dir}/{s}.bin.gz")).exists();
                let is_entry = scenes
                    .scenes
                    .values()
                    .any(|sc| sc.entries.iter().any(|e| e == s));
                if is_scene || is_entry {
                    checked += 1;
                }
            }
        }
        assert_eq!(checked, ADULT_SCENES.len(), "some gated names missing from assets");
    }

    #[test]
    fn gated_touch_defs_exist_in_shipped_assets() {
        // Same rot check as scenes: every gated touch def's hotspot
        // *color* must exist in some shipped character's loaded touch
        // container. Prefix, not full name: the named def can live in
        // the unshipped `_demo` container (e.g. Kahli's A_Flasher_DEMO)
        // while the color's state aliases — which embed the animation
        // frames — ship in touch.FAS and are what the gate blocks.
        for g in ADULT_TOUCH {
            let color = &g[..9];
            let found = ["tahni", "maeka", "kahli", "fatb"].iter().any(|c| {
                let dir = format!("{}/assets/characters/{c}", env!("CARGO_MANIFEST_DIR"));
                crate::scenes::Scenes::load(std::path::Path::new(&dir))
                    .map(|sc| {
                        sc.scenes
                            .get("touch")
                            .is_some_and(|t| t.raw.keys().any(|k| k.starts_with(color)))
                    })
                    .unwrap_or(false)
            });
            assert!(found, "gated touch color {color} missing from assets");
        }
    }
}
