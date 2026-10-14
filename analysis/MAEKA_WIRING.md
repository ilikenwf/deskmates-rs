# Maeka scene wiring audit (sword-training fallback)

User report: menu entries "dance", "emerge" (and `maeka_dummy`) all played
the SAME animation — the `common_sword_training_*` frames.

## Root cause

The shipped Maeka FAZ scenes contain **stub defs**: defs whose entire body
is the choice group

    {common_sword_training_long_flipped|common_sword_training_long_flipped|common_sword_training_short|common_sword_training_short_flipped}

i.e. "play sword training". The original author shipped the real
choreography in **trailing-underscore twin defs** (`<stub>_`) that nothing
called — the `all` entry scripts name the stubs, not the twins. Every scene
with such a stub pair therefore played sword training for its whole menu
entry. (dd114305 diagnosed this for `maeka_dancer` but only overrode `all`;
the idle pool plays the *expanded* entry names, so the stubs still played —
fixed properly now by rewiring the stub defs themselves.)

Audit method: parsed every `assets/characters/maeka/scenes/*.bin.gz`,
walked defs reachable from each `all` entry (scene-local defs first, then
behavior masters in runtime order common, common_demo, common_enter,
common_enter_demo, touch, touch_demo), and classified every frame token as
scene-local vs behavior-resolved.

## Per-scene verdicts

| scene | verdict |
|---|---|
| email, anaconda, chimp, creeper, cub, hammer, icecap, log, sled, staff, sword | **OK** — entries reach scene-local defs referencing the scene's own frames (ids 4000+). `maeka_sword` IS sword training by design (S017_Sword_Training_02). |
| dark, firedance, ignius, knife, rune, strikes, veridius, white | **OK** — palette-swap variants recovered by 0637a072; their own defs reference their own spliced touch-master frames (ids 5000-7051), NOT sword frames. Verified: zero sword-range refs; frames byte-identical to the touch masters they name. |
| **dancer** | **FIXED** (stub rewiring, completes dd114305): `maeka_S021_Big_Dance_sequence_aa/_ab` stubs → `_aa_/_ab_` twins (frames 4000-4154 choreography). |
| **emerge** | **FIXED**: `maeka_CE005_startup` stub → `maeka_CE005_startup_` (frames 4000-4332: emerges from a hole — matches the adult.rs comment "full frontal from the hole"). |
| **float** | **FIXED**: `maeka_S002_sequence_aa/_ab` stubs → twins (float start/loop frames 4000-4178). |
| **form** | **FIXED**: all 8 `maeka_S015_Training_Bare_Hands_a[a-h]` stubs → twins (bare-hands training frames 4000-4194). |
| **splash** | **FIXED**: `maeka_splash_aa/_ab` stubs → twins (spliced touch frames 5900-6094). |
| **dummy** | **ORIGINAL BEHAVIOR — left as-is.** Its entire script is `all`/`all_demo`/`all_slow`/`all_slow_demo` = the sword-training choice group; there are NO other defs and no orphaned twins. The installer shipped it frameless; the splice (0637a072) correctly gave it the 165 frames its defs name (byte-identical to common_demo 100-263 + base poses, palettes byte-identical to common_demo's 18 DIBs). A training dummy that trains with a sword: sword training IS its only content in the original data. Contract §3 applies. |

## Fix

Runtime wiring overrides in `src/scenes.rs` (`MAEKA_REAL_DEFS` table in
`Scenes::load`): each stub def's body is replaced by its orphaned twin's
body when the twin exists. No assets re-spliced — all 25 scene files stay
byte-identical (the splice pulled the RIGHT frames; only the def wiring was
wrong). `maeka_dummy` deliberately absent from the table.

Regression pin: `scenes::tests::maeka_rewired_scenes_are_not_sword_training`
— decodes every frame the four `common_sword_training_*` defs play, then
asserts each fixed scene's entry-reachable frames are (a) free of any
`sword_training` reference in its raw defs and (b) ≥50 frames whose decoded
bytes differ from every sword-training frame.
