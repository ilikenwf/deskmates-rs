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

## Reversed from Sprite.exe

The engine behavior mirrors the original `Sprite.exe`, reverse-engineered
with radare2 + Ghidra (every claim address-backed in
[`analysis/PLAYER_RE.md`](analysis/PLAYER_RE.md)):

- **100 ms frame tick.** No Win32 timers anywhere: the original paced
  frames at `1000/fps` ms (fps = 10) in a `MsgWaitForMultipleObjects` +
  `GetTickCount` deadline loop. `pet.rs` ticks at the same 100 ms/frame.
- **Random-def-per-completion speech cadence.** Orphaned `dialog_*` defs
  (ambient voice lines) were fired by the script-completion handler
  picking one uniform-random def (`_rand() % N`, reshuffle-swap) from the
  whole def table each time a script finished — no dedicated timer. Here
  the `dialog_*` defs ride the same shuffle-bag as the animation scenes,
  with the Gold-upgrade nag excluded (the `no_all_list.txt` analogue).
  Entry speech (`S_Deskmate_Enter`'s `{dialog_hi|dialog_hi_baby%2|…}`)
  is the same weighted `%weight` pick, resolved by `faz::resolve_choices`.
- **Fence grammar + walk speed.** `fence_*` defs are
  `[dx,dy]<n>{l,r,c}<n>{t,b,m}` tokens: a signed int16 delta applied per
  100 ms frame tick (`[200,0]` = 200 px/frame = **2000 px/s**) plus an
  anchor point measured in pixels from screen edges; `fence_home` is the
  work area inset 1, and rest positions clamp into the work area with a
  re-center when < 2/3 of the sprite would be visible. `main.rs`
  (`work_area`/`clamp_rest`/`apply_walk`) implements exactly this, using
  the EWMH `_NET_WORKAREA` for the work area.
- **Transparency.** The original used `SetLayeredWindowAttributes` with a
  pure-black **color key** (LWA_COLORKEY, no alpha) over a black-filled
  window. This clone renders per-pixel alpha instead — cleaner on modern
  compositors — plus XShape input/bounding clipping on bare X11.
- **Sound.** `!name` tokens were fire-and-forget `waveOutWrite` calls with
  **one active sound slot** (a new sound replaces the current one; frames
  never sync to audio). `audio.rs` plays each ` !name` immediately and
  never blocks the frame loop; the WAV cache is per character, rebound on
  every character switch.
- **Telemetry / adware: none of it exists here.** The original Manager.exe
  wrote the `HKCU\...\Run\DeskMateAutoUpdate` autorun value, dropped
  Startup-folder `.LNK`s (" DeskMate.LNK", "DeskMates 2005.LNK", …),
  dropped and executed `%TEMP%\RegisterDeskMate.exe`, and built
  `WWWParams` + `Rand=%u` tracking URLs for browser launches
  (full inventory: PLAYER_RE §6). This binary has no registry writes, no
  autorun, no dropped helpers, and no network code outside the opt-in
  chat endpoint.

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
  pack_classic.py  packs the early-set classic scenes -> scenes/*.bin.gz
  classic_survey.py md5 dedup of early sets vs shipped sets
  CLASSIC_MODE.md  classic-mode provenance, period label table, dedup stats
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

Right-click: Touch her / Ask Tahni… / Next scene / Next character /
Settings… / Hide / About / Visit <character> / Quit — plus **Classic** where a character has an
early set (Tahni: the v1.04 demo from the Czech cover CD + dropped v1.04
scenes, original period labels; see analysis/CLASSIC_MODE.md).
Left-drag moves her.
She walks along the bottom edge of the screen between scenes. The tray icon's **Character**
submenu (or "Next character") hot-swaps between all 11 shipped characters:
Tahni, FatB, Maeka, Kahli, Tavia, Sirens, Fantasy, DM05, SnS, Arianna,
Dancers — the choice persists in the config. `--character <name>` selects
one at startup. `--matte` draws an opaque background (useful over remote
desktops without a compositor). On Linux a compositor is required for true
transparency.

**Scene review.** `--scenes` prints every scene name and exits;
`--scene NAME` loops a single scene; and `--review` steps through the whole
set one scene at a time — **→/Space** next, **←** back, **Esc** quit — with
the scene name shown in the window title, the speech bubble, and on stdout
(`[review 42/132 tahni_kiss]`) so you can copy names as you go. Review mode
shows gated scenes too (the adults-only gate is forced open); that's what
it's for — it's how the `src/adult.rs` blocklist gets curated. The same
flag works in all the sibling repos (deskmates3d-rs, arianna-rs, …).

**Visit <character>** is the original's `ShellExecuteA` menu item, kept: it
hands the earliest Wayback capture of that character's period product page
(`tahni.com/deskmate.php`, `fatb.oska.com/deskmate.php`, `kahli.com/...`,
`maeka.com/...`, `teecee.net/...`, `oska.com/deskmate.php`) to your OS
browser (`xdg-open` / `open` / `cmd /C start`). The pet itself still never
opens a socket.

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

- [x] All 11 2D characters (Tahni 114 scenes … Dancers 49) — walking,
      touch reactions, tray + in-app character switcher, zoom, shuffle-bag
      scene order (every scene plays before any repeats)
- [x] Optional OpenAI-compatible chat ("Ask Tahni…", off by default)
- [x] Adults-only scene gate (Settings checkbox, default off)
- [x] Original FAZ assets loaded natively (`.bin.gz`, no PNG dumps)
- [x] 3D characters (Tahni3D, Kahli3D) — see `deskmates3d-rs`
- Sound effects and voice lines play from
      `assets/characters/<name>/sounds/*.wav.gz` (extracted from the
      original `DeskMate.WAS` archive), rebound per character on switch.
