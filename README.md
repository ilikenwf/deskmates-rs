# deskmates-rs

Cross-platform (Windows / macOS / Linux) Rust reimplementation of **Oska
DeskMates** — the late-90s desktop companion starring **Tahni** — using the
original animation art, with all commercial/phone-home behavior removed.

## Why

The original DeskMates shareware (Oska, Inc.) nagged for registration, linked
to the Oska store, and shipped a demo that gated characters/scenes behind
`isUnlocked` / `DemoList` checks. This reimplementation:

- **No network code at all.** The binary contains no sockets, no HTTP client,
  no telemetry. (Verify: `cargo tree | grep -iE 'reqwest|hyper|curl|socket'`
  returns nothing.)
- **No gating.** Every one of the 114 animation scenes is playable; no
  `isUnlocked`, no serial, no nag screens.
- **100% offline.** The original `.FAZ` animation containers ship verbatim
  (gzip-compressed) and are decoded natively at runtime by `src/faz.rs`.

### Removed from the original (evidence from the reverse-engineered binaries)

| Original behavior | String found in `tahni_real/0001.bin` |
|---|---|
| "Visit us" browser launch | `http://www.oska.com/dialog/visit_us.php` |
| Store / purchase upsell | `https://secure.oska.com/store/tahni.php` |
| Feature gating | `property isUnlocked`, `DemoList` demo-scene list |

All of the above are simply absent here.

## What's here

```
src/
  main.rs      winit event loop, transparent always-on-top window, tray,
               software compositor (RGBA canvas -> premultiplied frame)
  pet.rs       FAZ script interpreter: entry defs, sub-defs with repeat /
               duration semantics, builtins (exit_wait, fence_*_out, ...),
               screen-edge walking, touch reactions
  faz.rs       native FAZ container parser/decoder (port of analysis/faz.py)
  scenes.rs    scene loader (scenes/*.bin.gz) + decoded-frame cache
  render.rs    blit / rects / ab_glyph text
analysis/
  ic5_extract.py   ClickTeam Install Creator V5 unpacker (installer -> bins)
  faz.py           FAZ container parser (DIB templates, script pairs,
                   4bpp frame blocks)
  faz_convert.py   FAZ -> PNG + scenes.json converter (superseded by src/faz.rs;
                   kept as the reference the Rust port and tests/faz_parity.rs
                   are validated against)
  pack_assets.py   packs original FAZ scenes -> assets/characters/<name>/scenes/*.bin.gz
assets/characters/<name>/
  scenes/<scene>.bin.gz   original FAZ container, gzip-compressed
                          (200x200 classic; 8bpp scenes up to 316x200 etc.)
```

## Running

```sh
cargo run --release          # expects ./assets next to the binary or in cwd
```

If the `assets/` tree is missing, rebuild it from the extracted installer
payloads under `analysis/`:

```sh
python3 analysis/pack_assets.py            # all characters
python3 analysis/pack_assets.py tahni      # or just one
```

Right-click: Touch her / Ask Tahni… / Next scene / Settings… / Hide / About /
Quit. Left-drag moves her. She walks along the bottom edge of the screen
between scenes. `--character fatb` runs the FatB mascot instead of Tahni.
`--matte` draws an opaque background (useful over remote desktops without a
compositor). On Linux a compositor is required for true transparency.

## Optional: chat with an AI (off by default)

Right-click → **Settings…** points Tahni at any OpenAI-compatible
`/chat/completions` endpoint (OpenAI, Ollama, LM Studio, …). Until you do,
the app performs **zero network activity** — a request only ever fires from
the "Ask Tahni…" menu item, and only when an endpoint is configured.

```sh
DESKMATES_AGENT_ENDPOINT=http://localhost:11434/v1 \
DESKMATES_AGENT_KEY=sk-... \
cargo run --release
```

Replies appear in a speech bubble; the model may embed a stage direction
(`[anim:karate]`, `[anim:tahni_tease]`, …) which plays that animation while
she "speaks". Settings persist in the config file (`agent_endpoint`,
`agent_key`, `agent_model`). The chat client lives behind the `agent` cargo
feature (default on); `--no-default-features` builds a strictly-offline pet
and is what cross-target checks use (ring cannot cross-compile from Linux).

## Provenance & license

Installer obtained from the Internet Archive item `Deskmates`
(<https://archive.org/details/Deskmates>). Animation art and scripts are
© Oska, Inc.; this project is abandonware preservation — do not redistribute
the `assets/` tree. Engine code in `src/` and `analysis/` is MIT.

## Status

- [x] Tahni (2D) — 114 scenes, walking, touch reactions, tray, zoom
- [x] FatB (2D) — 30 scenes (`--character fatb`)
- [x] Optional OpenAI-compatible chat ("Ask Tahni…", off by default)
- [ ] Other 2D DeskMates (Maeka, Kahli, Tavia, Arianna, Dancers, Sirens) —
      newer installer container, RE in progress
- [ ] 3D characters (Tahni3D, Kahli3D) — separate repo, needs a 3D renderer
