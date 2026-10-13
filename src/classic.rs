//! Classic mode: the early-version DeskMate scene sets, wired in as an
//! extra "Classic" submenu per character (in-window menu + tray mirror).
//!
//! The scene assets themselves are ordinary `scenes/*.bin.gz` FAZ
//! containers (packed by `analysis/pack_classic.py`; provenance and the
//! label table in `analysis/CLASSIC_MODE.md`). This module only carries
//! the menu surface: which scenes are the classic set, and the original
//! period menu labels. The shipped `.mnu` files (all five in the
//! deskmate_202601 bundle) contain only DeskMate Manager menus — zero
//! scene entries — so the period label for a scene is its original
//! filename, title-cased (that is how the original sprite menu rendered
//! it). v1.04 variants that share a name with a current scene are
//! disambiguated by the `classic_` asset name; the label stays period.
//!
//! Adult gating is unchanged: a classic scene named in
//! `crate::adult::ADULT_SCENES` is hidden from the submenu and
//! unplayable exactly like a current scene.

pub struct ClassicScene {
    /// Scene key as loaded from `scenes/<scene>.bin.gz`.
    pub scene: &'static str,
    /// Original period menu label.
    pub label: &'static str,
}

/// Characters that ship a classic set (regression-test driver).
#[cfg(test)]
pub const CHARACTERS_WITH_CLASSICS: &[&str] = &["tahni"];
const TAHI: &[ClassicScene] = &[
    // Czech Computer 2000/08 cover CD — the original v1.04 demo (Apr 2000).
    ClassicScene { scene: "classic_bull", label: "Bull" },
    ClassicScene { scene: "classic_rollerblade", label: "Rollerblade" },
    ClassicScene { scene: "classic_sailing", label: "Sailing" },
    ClassicScene { scene: "classic_scuba", label: "Scuba" },
    ClassicScene { scene: "classic_snowboard", label: "Snowboard" },
    ClassicScene { scene: "classic_space_hopper", label: "Space Hopper" },
    // tahni-104 full unlock data (v1.04): scenes dropped from later sets
    // plus the v1.04 cuts of buggy/dustbuster.
    ClassicScene { scene: "classic_buggy", label: "Buggy" },
    ClassicScene { scene: "classic_dustbuster", label: "Dustbuster" },
    ClassicScene { scene: "bambi_flash", label: "Bambi Flash" },
    ClassicScene { scene: "bambii_champ", label: "Bambii Champ" },
    ClassicScene { scene: "bambii_promo", label: "Bambii Promo" },
];

/// The classic set for a character (empty when the character has none).
pub fn classic_set(character: &str) -> &'static [ClassicScene] {
    match character {
        "tahni" => TAHI,
        _ => &[],
    }
}
