//! Adults-only scene gating.
//!
//! The original Deskmates shipped a handful of nude scenes (and the whole
//! adult line). They stay in the asset set but are hidden from every random
//! pool (idle, Next scene, agent stage directions) unless the user enables
//! "Adults-only content" in the tray Settings window.
//!
//! Curated by visual inspection of every scene's mid-frame (contact sheets,
//! 2026-10-11): exposed breasts/buttocks/genitals only — merely suggestive
//! but clothed scenes (g-string, bikinis) are NOT gated.

/// Scene names considered adults-only. Scene names are character-prefixed
/// and globally unique within a character's asset set.
pub const ADULT_SCENES: &[&str] = &[
    // Tahni (2D)
    "shower",          // rear nude shower
    "tahni_tease",     // topless tease
    "tahni_underwater",// topless swim
    "swimming_pool",   // nude male swimmer
    "surfing",         // nude male surfer
    "waking_up",       // nude male from behind
    // Maeka
    "maeka_creeper",   // topless
    "maeka_dancer",    // topless
    // Dancers
    "dg_t_naughty",
    "dg_t_playboy",
    "dg_t_secretboobies",
    "dg_t_secretnude",
    // Salt-n-Salsa (SnS)
    "SnS_t_both_boobsqueeze",
    "SnS_t_spice_gongboobs",
    "SnS_t_spice_nipple",
    "SnS_t_sugar_boobyrap",
    "SnS_t_sugar_nipple",
    "SnS_t_sugar_puppetboobs",
    "SnS_s_spice_pu_topless",
    "SnS_s_sugar_pu_nude",
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
        for c in ["tahni", "maeka"] {
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
