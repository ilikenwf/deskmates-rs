# Classic mode — early-version DeskMate scene sets

Classic mode wires the early-version ("classic") DeskMate scene sets in
as extra scenes plus a per-character **Classic** submenu (in-window menu
+ tray mirror), with the original period menu labels. Adult gating is
unchanged: a classic scene named in `crate::adult::ADULT_SCENES` is
hidden from the submenu, the random pools, and `play_named` exactly like
a current scene.

## Provenance: where each classic scene came from

Sources (static file reads only; no `.exe` was ever executed):

- **CD** — `ia-finds/computer200008cd/extracted/Computer/hry/tahni-v104-cd/`:
  the original **Tahni v1.04 demo** as shipped on the Czech *Computer*
  magazine cover CD 2000/08 (Apr 2000). The earliest Tahni artifacts
  known. Demo cuts differ from the unlock data: extra entry variants
  (`all_demo`, `rollerblade_ac..af`, `scuba_af..ak`, `board_ad..ai`, …)
  and, for `space_hopper`, a completely different early scene (51 frames
  vs the later 123).
- **T104** — `ia-finds/tahni-104/` ("Tahni Data", v1.04 full unlock,
  hash-verified v1.04).
- **BUNDLE** — `ia-finds/deskmate_202601/…/Deskmate - All Full
  Deskmates Cracked/<Char>/Data/` (~2000-scene multi-character bundle).

| Scene asset | Source | Original name | Period label |
|---|---|---|---|
| `tahni/scenes/classic_bull.bin.gz` | CD | `bull` (v1.04 demo cut) | Bull |
| `tahni/scenes/classic_rollerblade.bin.gz` | CD | `rollerblade` (v1.04 demo cut) | Rollerblade |
| `tahni/scenes/classic_sailing.bin.gz` | CD | `sailing` (v1.04 demo cut) | Sailing |
| `tahni/scenes/classic_scuba.bin.gz` | CD | `scuba` (v1.04 demo cut) | Scuba |
| `tahni/scenes/classic_snowboard.bin.gz` | CD | `snowboard` (v1.04 demo cut) | Snowboard |
| `tahni/scenes/classic_space_hopper.bin.gz` | CD | `space_hopper` (early scene, later redrawn) | Space Hopper |
| `tahni/scenes/classic_buggy.bin.gz` | T104 | `buggy` (v1.04 cut) | Buggy |
| `tahni/scenes/classic_dustbuster.bin.gz` | T104 | `dustbuster` (v1.04 cut) | Dustbuster |
| `tahni/scenes/bambi_flash.bin.gz` | T104 | `bambi_flash` (dropped from later sets) | Bambi Flash |
| `tahni/scenes/bambii_champ.bin.gz` | T104 | `bambii_champ` (dropped from later sets) | Bambii Champ |
| `tahni/scenes/bambii_promo.bin.gz` | T104 | `bambii_promo` (dropped from later sets) | Bambii Promo |

`classic_`-prefixed assets carry the FAZ-internal name of a scene that
is *still shipped* (the v1.04 `buggy` next to the current `buggy`).
`Scenes::load` keys scenes by **filename stem** (not the FAZ-internal
name) so both versions coexist; the Classic submenu plays the stem key.

## Menu labels: the `.mnu` reality

The contract expected the original scene menu labels from the `.mnu`
files. Verified against all five in the bundle (`Data.mnu`, `22371.mnu`,
`48245.mnu`, `52750.mnu`, `euniverse.mnu`; schema per
`~/reference/oska-deskmates/analysis-notes/original_menu_settings_schema.md`):
they contain **only DeskMate Manager menus** (store/URL/settings
entries) — zero scene entries. There are no per-scene labels anywhere in
the bundle (checked `Sprite.exe`/`Tahni.exe` strings, `DeskMate.WAS`,
and the FAZ script string pairs). The original sprite menu rendered each
scene by its **filename**; that IS the period behavior, so the period
label = original filename, title-cased (table above). The v1.04
variants keep the plain period label ("Buggy"); the `classic_` asset
name and this table carry the disambiguation.

## Dedup stats (md5 of FAZ bytes, `analysis/classic_survey.py`)

The cracked BUNDLE sets are **byte-identical to the shipped sets for
every character** — which independently proves our asset provenance is
the authentic period set:

| Char | Shipped | Bundle identical | Bundle new | Early-set variants shipped as classic |
|---|---|---|---|---|
| tahni | 115 | 115/115 | 0 | 11 (6 CD + 5 T104) |
| teecee | 84 | 84/84 | 0 | 0 |
| fatb | 30 | 30/30 | 0 | 0 |
| kahli | 52 | 52/52 | 0 | 0 (+1 unplayable, below) |
| maeka | 15 | 15/15 | 0 | 0 (+10 unplayable, below) |
| oska | 84 | 84/84 | 0 | 0 |
| dancers (`dancing-girls-rs`) | 54 | 54/54 | 0 | 0 |
| sns (`sugar-n-spice-rs`) | 82 | 82/82 | 0 | 0 |

Skipped (dedup / not playable):

- byte-identical duplicates: everything in the BUNDLE (see table);
- T104 `scuba`: superseded by the earlier CD cut (one classic variant
  per scene name, earliest source wins);
- T104 `arnie`: 1-frame demo placeholder (`faz::is_placeholder`);
- BUNDLE `Kahli/Data/kahli_s_archive_a` and 10 `Maeka/Data/maeka_*`
  files: 0-frame def libraries (script banks, no frames) — the loader
  would strip their entries as placeholders; they are not scenes.

## Mechanism

- `analysis/pack_classic.py` packs the whitelist above (gzip of the
  original stream-unwrapped FAZ bytes, same convention as
  `pack_assets.py`).
- `src/classic.rs` — the menu surface: per-character classic scene list +
  period labels.
- `src/main.rs` — "Classic" row in the in-window menu (opens the submenu
  in place of the main list, "Back" returns; rows shrink to fit the
  canvas), tray "Classic" submenu mirror with live adult-gate
  enable/disable on config reload, `classic:<scene>` tray events via
  `Pet::play_named` (pool-gated, so the adult rule is enforced).
- `--scenes` lists classic scenes like any scene; `--scene classic_bull`
  plays them.
- Regression tests: `scenes::tests::classic_scenes_ship_and_parse`,
  `classic_variants_do_not_overwrite_current_scenes`.

## Not done (remaining candidates)

- Sibling repos `dancing-girls-rs` (BUNDLE `Dancers`) and
  `sugar-n-spice-rs` (BUNDLE `SnS`) were dedup-surveyed: every scene in
  those bundle dirs is byte-identical to what those repos already ship,
  so there is nothing to wire (no classic set exists for them).
- `deskmates3d-rs` — surveyed later (see below); no classic scene delta.
- `extracted/Deskmate/Tahni 3.2 Full Unlock/Tahni Data` is md5-identical
  to the shipped set except `arnie.FAZ`/`scuba.FAZ`, which are the same
  v1.04 cuts as T104 (already covered above: placeholder / superseded by
  the CD cut). `unpack-kahli/…/Kahli Data` is md5-identical to the
  shipped set except `kahli_s_archive_a` (the unplayable def library
  above). The `Tahni Deskmate 1.4.exe` / `Deskmate Tahni Full Pack.exe`
  installers were not unpacked (no `.exe` execution; IC payloads would
  need the static unpackers).

## 3D line (`deskmates3d-rs`): zero classic delta

Dedup of the 3D early sets against what `deskmates3d-rs` ships
(same md5-of-FAZ-bytes method, `analysis/classic_survey.py`):

| Source | Files | Byte-identical to shipped | New scenes |
|---|---|---|---|
| BUNDLE `Tahni3D/Data` | 76 | 75 | 0 |
| BUNDLE `Johlee/Data` + IA `Johlee/…/Data` | 8 | 2 | 0 |
| BUNDLE `Kahli/Data` (no `Kahli3D` dir exists) | 61 | 52 | 0 playable |

Every `T3D_*` and `johlee_s_*` container in those dirs is already shipped
as a scene or a `behaviors/` def bank — the provenance proof extends to
the 3D line, so there is nothing to wire and no classic mechanism was
ported into `deskmates3d-rs`.

The only non-matches are def banks, not scenes: BUNDLE
`Tahni3D/Data/common_enter.FAS` and the IA Johlee package's
`common{,_demo,_enter,_enter_demo}.FAS` / `touch{,_demo}.FAS` (Johlee
ships only `johlee_s_spidershootout_b` as a behavior). They are shared
script banks whose defs the shipped scenes already resolve from the
character's own banks, so they are not classic-mode material.
