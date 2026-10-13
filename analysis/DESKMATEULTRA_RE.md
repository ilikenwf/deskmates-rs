# DeskMateUltra.air — reverse-engineering report

Third-generation DeskMate (2010), analyzed 2026-10-13 by static analysis only —
nothing from the package was executed. Inputs:
`~/reference/oska-deskmates/cdx-findings/raw/app__dl__dmu__DeskMateUltra.air`
(3,323,301 B, ZIP/AIR) and its main SWF. Everything unpacked + extracted under
`~/reference/oska-deskmates/ultra/` (`swf_parse.py` = hand-written FLAML/ABC
parser, `lossless2png.py` = DefineBitsLossless2→PNG converter). Machine-readable
companion: [`DESKMATEULTRA_inventory.txt`](DESKMATEULTRA_inventory.txt).

**TL;DR verdict.** DeskMateUltra is not a sprite-based DeskMate at all: it is a
Flex 3 / Adobe AIR 1.5 **live-action video jukebox** for one character
(Trinity Porter, "Trinity: Sunset Boulevard Edition"). The package contains **no
character animation** — the "scenes" are server-side FLV video clips gated by a
DRM/purchase gateway. The main SWF is unrunnable in Ruffle (pure-AIR app:
transparent always-on-top `NativeWindow`s, `EncryptedLocalStore`, `HTMLLoader`,
raw `Socket`); the two companion SWFs *are* plain Flash and Ruffle-compatible.
Recommended path: reimplement the thin "Ultra shell" in Rust reusing the
extracted skins/wallpapers, and hunt the lost FLV catalog on the Wayback Machine
before committing to anything bigger.

---

## 1. Package layout

`.air` = ZIP (`mimetype`: `application/vnd.adobe.air-application-installer-package+zip`).
Note: `META-INF/AIR/hash` has a **bad CRC** in the archive — Python `zipfile`
aborts on extraction; `7z` tolerates it. Contents (315 files):

```
DeskMateUltra.swf                    694,828 B   main app (Flex 3, AS3)
META-INF/AIR/application.xml         AIR 1.5 descriptor, v1.4.6
META-INF/AIR/hash                    32 B (bad CRC)
META-INF/signatures.xml              166 KB Verisign signing (Oska Educational
                                     Systems Pty Ltd, © 2010)
mimetype
assets/flash/DMU_intro.swf            65,675 B   tutorial overlay
assets/flash/ProgressBar.swf         135,872 B   update-progress animation
assets/icons/icon_{16,32,48,128}.png
assets/dialog/                       HTML/XSL/CSS support dialogs (About, Faqs,
                                     Support, BuyInfo, Unlock*, UpdateReady,
                                     Uninstall, Notices, TakeRight.xsl, …)
assets/dialog/js/                    prototype.js 126 KB, script.aculo.us
                                     effects.js, dmu.js (app dialog glue), …
assets/dialog/images/                TrinityPose.png, Uninstall.png, dmu_about.png,
                                     cvv.png, DialogBackground.png, Heading.png,
                                     Verisign.png, tsb_*.jpg promo shots
assets/dialog/wallpapers/TrinitySB/  60 JPG wallpapers (001_bloodcells … 060_steppingout)
assets/dialog/catalog.xml            product/clip-set manifest (see §3)
```

`application.xml` key facts: id `com.deskmateultra.DeskMateUltra`, version
**v1.4.6**, description "Trinity Sunset Boulevard Edition turns your desktop
into a private stripclub stage", `systemChrome=none`, `transparent=true`,
fixed 296×62 window, `allowBrowserInvocation=true`. The 296×62 transparent
window is the **control bar** — the character herself lives in separate
utility windows created at runtime.

## 2. SWF / tag inventory (md5s)

| file | md5 | size | fps × frames | notes |
|---|---|---|---|---|
| DeskMateUltra.swf | `22b58a6c241722dc88429932717d0a4b` | 694,828 → 1,369,002 | 24 × 2 | CWS v10, Flex 3, RDF date Jan 29 2010 |
| DMU_intro.swf | `0ce8a67d8c742330b960b0b4c401c1e9` | 65,675 → 80,050 | 24 × 161 | tutorial, pure Flash |
| ProgressBar.swf | `2cf021b5dc4eae202b3642c658ca3305` | 135,872 → 170,264 | 20 × 236 | `UpdateProgressIndicator3_fla`, pure Flash |

**DeskMateUltra.swf tags** (164 total): 2× `DoAbc2` (113,130 B + 945,569 B of
AVM2 bytecode, ABC version 46.16 — Flash CS3/CS4 era); 102×
`DefineBitsLossless2` (UI skins: mostly 30×30 button skins, 12×13 glyphs,
97×22/123×27 labels, one 296×40 bar); 22× `DefineSprite` (empty 1-frame
wrappers); 2× `DefineFont3` (193 KB embedded "VerdanaEmbedded" bold+medium);
1× `DefineSound` (3,340 B **MP3** UI click, `embed_audio_click_mp3`);
`SymbolClass` (128 entries), `ExportAssets`, `ScriptLimits`, `Metadata` (RDF:
"Adobe Flex 3 Application"), `ProductInfo`, `FileAttributes` (0x19).

**Protector traces** (all three SWFs): the SWF header is nonstandard — a junk
RECT nibble with the tag stream actually starting at offset 13; `FrameLabel`
tags have their UI16 frame numbers stripped; `DoAbc2` payloads carry extra
padding before the ABC header. The Flex compiler output was post-processed by
some protector/packer. `swf_parse.py` auto-detects the real stream start by
requiring a tag walk that lands exactly on EOF and begins with `FileAttributes`.

**ABC blocks** (extracted to `ultra/extracted/*.abc`, string pools dumped to
`*.strings.txt`): `frame1` = Flex framework shell (1,897 strings, 560 `mx.*`
classes); `frame2` = application code (8,804 strings, ~200 `com.deskmates.*`
classes, full class list in the inventory). Intro/PBar ABCs are tiny and import
only `flash.display/events/media` — **no AIR APIs**.

**Companion SWFs.** `DMU_intro.swf` = the first-run tutorial overlay: frame
labels `tutorialStart`, `tutorial`, `quitting`; frame scripts at 20/40/55/…/161;
three `DefineButton2` (Next / Quit / "trinVids" → opens
`strip-tease-videos.php` in the browser); dispatches `TutorialStarted`; one
143×170 Trinity pose bitmap; 730 B MP3 click. `ProgressBar.swf` = 201
progressive 206×12 strip frames, frame scripts at 21/236 dispatching
`DownDone`/`UpDone` — loaded by the updater flow (`UpdaterStage2`).

305 embedded bitmaps were converted to RGBA PNGs (`ultra/extracted/*.png`).
**Nothing resembling a character animation exists in the package** — this is the
central fact for §4–§6.

## 3. Scene / behavior / trigger catalogue

DeskMateUltra's "scenes" are **video sequences**, not timeline animation. The
local `catalog.xml` only declares entitlements:

```xml
<product id="1" name="TrinitySB" displayName="Trinity: Sunset Boulevard Edition">
  <clip_set id="1" name="TrinitySBDemo" isFree="true"/>
  <clip_set id="2" name="TrinitySB" displayName="Full Version" isFree="false" price="29.95"/>
</product>
```

The actual clip list ("Strips & Pole Dances" — copy from the unlock nag:
*"You can Unlock Trinity for $29.95 and get ALL of her Strips & Pole Dances"*)
lives server-side and arrives via `GetListing` as a `ListingNode/ListingLeaf`
tree with `downloadUrl`s; `ClipSet`/`ClipExtract` VOs map clip-set ids to
versions (`mapClipSetIdToCurrentVersion`). Recovered behavior surface:

**Playback engine** — `NetConnection`/`NetStream` playing **progressive local
FLVs** (`.flv`, `.flv2`) downloaded by `RemoteFile`/`DownloadDetail` into
`applicationStorageDirectory` after unlock ("The sequences will now download",
`UnlockedOK.html`). `VideoWindow` + `WatchdogVideoDisplay` (stall watchdog) in
transparent utility windows (`NativeWindowType=UTILITY`); `VideoLibrary`,
`RecentlyPlayList`, `NowPlaying` display; playlist options:
`playSequencesAutomatically`, `playSequencesImmediatelyOnStartup`,
`fastSequenceGap`, "How often do you want to see sequences?",
`showSequencesContaining{General,Mature}Content` (content filter),
`pruneVideosWeWantToKeep`.

**Control bar** (the 296×62 main window) — `ControlBar` with skin states for
buttons: **Play, Pause, Stop, Next, Zoom, Audio, Alert, Options, More, Exit**
(`ControlBarButton_{up,over,down,selected*}Skin*`); `DragWindow`/`DragWindowWard`
for moving it; `ChromelessWindow`, `FadingPopup` for transient messages.

**Dialogs/panels** (Flex `Dialog` + `HTMLLoader` over `assets/dialog/*.html`):
About, Help, Message, More, Options (incl. `launchAtStartUpCheckBox`,
`useAutoUpdateCheckBox`), Order, Uninstall (`dmu_uninstall.php`), **Wallpaper**
(6-up grid of the bundled TrinitySB JPGs, `TakeRightRequest` to consume an
entitlement, `strip-tease-wallpaper.php?wallpaperIdx=` online gallery), Buy
(`BuyForm` with credit-card form items + CVV), Unlock (`UnlockForm`,
`UnlockCodeTextInput`, "Manual Unlock").

**Triggers / timers** — `DEFAULT_IDLE_TIMEOUT_INTERVAL` / `idleTimeout` /
`doIdleTimeout` (idle → play a sequence), `stage_mouseMoveHandler` /
`stage_mouseLeaveHandler` / `keyInteraction`, native window drag
(`NATIVE_DRAG_*`), `APPLICATION_ACTIVATE/DEACTIVATE`,
`FULL_SCREEN_INTERACTIVE`, `Heartbeat` polling, notifications
(`GetNotifications`/`PutNotification`/`RecordNotificationProcessed`,
`areSomeNoticesUnread`), `StartupLock` (single-instance lock file),
`Updater`/`UpdaterStage2` (download new `.air` → `launchUpdate`),
`installApplication`/`uninstallApplication` native menu items.

**DRM/licensing** — `MachineId` (hardware fingerprint, "MachineId can only be
accessed through MachineId.getId"), `Crypto`/`CryptoConnection` +
`CryptoService.getCrypto` (encrypted gateway channel, base64 payloads),
`EncryptedLocalStore` (key/rights storage), `CheckCustomerCode`,
`CheckRights`/`TakeRight` (entitlement consumption), AMF0 serialization over
`gateway.php` (`AccessService.callService`). Gateway endpoints:
`http://deskmateultra.com/app/gateway.php`, `https://secure.oska.com/dmu/app/gateway.php`
(secure), plus a **dev-server leak**: `dmu_dev.smileware.com.au` ports 45002/44002
— the app was built by **Smileware (AU)** for Oska.

**Tutorial** (`DMU_intro.swf`, §2): stepped overlay with Next/Quit, "trinVids"
browser shortcut, `quitting` screen. **Not recovered:** the tutorial's prose
(glyph-indexed `DefineText`) and the exact local wallpaper-application syscall —
no `SystemParametersInfo`/helper-exe/`openWithDefaultApplication` strings exist,
so the final wallpaper-set step is either browser-mediated or in a component
not shipped in this package [INFERENCE].

## 4. Relation to the 2D FAS/FAZ line

**Engine: completely new; zero format or asset overlap.** The 1st/2nd-gen line
(see `ARCHAEO_DESKMATES.md` §2) is custom binary containers (FAZ scene files,
FAS behavior scripts, WAS/Wa3 audio) replayed by a native Windows runtime
(`Deskmate.exe`/`Sprite.exe` + `spritemgr.dll`). Ultra is Adobe AIR 1.5 / Flex 3
AS3 with AVM2 bytecode; no FAZ/FAS/WAS magic appears anywhere in the package.
The "animation" is live-action video of a real entertainer (Trinity Porter,
Miss Nude Australia 2009 — bio in `AboutTrinity.html`) instead of indexed-color
sprite frames.

What *does* carry over is the business architecture:

| 1st/2nd-gen (FAS/FAZ) | Ultra (AIR) |
|---|---|
| `secure.oska.com/gateway.php?Product=DeskMates` (setup stubs) | `secure.oska.com/dmu/app/gateway.php` — same gateway lineage, new path |
| Demo vs unlocked product, unlock codes | `TrinitySBDemo` (free clip set) vs full set $29.95, `CheckCustomerCode`/`UnlockForm` |
| `Update.exe`/`NewUpdate.exe` fetch logic | AIR `Updater` + `ProgressBar.swf` + `.air` relaunch |
| Local scene packs in setup EXE | Server-listed FLV catalog downloaded post-unlock |
| Cartoon characters (Oska, Tahni, Kahli…) | One live-action character per "edition" (productized by celebrity) |

So Ultra is the **3rd generation** exactly as flagged in `ARCHAEO_DESKMATES.md`
§2 ("engine unrelated to FAS/FAZ") — a video-on-demand pet, closer in spirit to
the pre-rendered `deskmates3d-rs` line (playback of canned visual sequences)
than to the vector/tween originals. The cards line (`card.FAS`) is likewise
unrelated.

## 5. Ruffle feasibility verdict

**Main app: not feasible.** Ruffle implements Flash Player, not AIR. DeskMateUltra
is *nothing but* AIR (string evidence, full list in inventory):

- `flash.desktop.*` — `NativeApplication`, multiple transparent, frameless,
  always-on-top `NativeWindow`s (UTILITY type) for the video + control bar.
  Transparent desktop-overlay windows are Ruffle's known structural gap; it has
  no AIR `NativeWindow` implementation at all.
- `EncryptedLocalStore` — rights/keys: unimplemented.
- `HTMLLoader`/`FlexHTMLLoader` — the entire support/buy/unlock UI: unimplemented.
- `flash.net.Socket` + `air.net.SocketMonitor` — heartbeat/DRM channel: unimplemented.
- `flash.filesystem` (`File`, `FileStream`, `applicationStorageDirectory`) —
  AIR file IO: unimplemented (Ruffle sandbox has no local FS).
- `NetStream` progressive local-FLV playback — Ruffle's NetStream covers
  progressive HTTP FLV only, and here it would be moot without the file layer.
- `allowBrowserInvocation`, native menus, native drag-drop: unimplemented.

Even with every gap papered over, the app's payload (the FLV catalog) is served
by dead servers, so a faithful emulation would be an empty control bar begging
for a gateway that no longer exists.

**Companion SWFs: feasible and actually runnable.** `DMU_intro.swf` (tutorial,
pure `flash.display/events/media`, `gotoAndPlay` frame scripts, 3 buttons) and
`ProgressBar.swf` (pure display/events) are ordinary AS3/AVM2 movies within
current Ruffle support — they can run as standalone artifacts (e.g. as museum
pieces alongside the extraction).

## 6. Recommended path

**Reimplement-in-Rust, subset scope — mirroring what worked for deskmates-rs /
deskmates3d-rs — but only after a content recovery sweep.** Concretely:

1. **Content first (blocking).** The package ships no video. Before writing an
   engine, sweep the existing Wayback CDX dumps (`cdx-findings/`) plus new
   queries for `dl.deskmateultra.com` and `*.flv`/`*.mp4` under
   deskmateultra.com/oska.com; also check archive.org items for
   "Trinity Sunset Boulevard" user uploads. If no clips surface, the only
   honest deliverable is the shell + wallpapers, and effort should be capped.
2. **Shell reimplementation** (if clips are recovered, or as a small standalone):
   a winit/wgpu (or even SDL) transparent always-on-top video pet: control bar
   (Play/Pause/Next/Zoom/Audio/Exit) reusing the 305 extracted PNG skins,
   playlist over local video files (decode via ffmpeg — recovered clips are
   FLV/H.264-era formats), the 60 bundled wallpapers as a wallpaper
   panel (set via platform API: `SystemParametersInfoW` on Windows), idle-timeout
   sequence trigger, and the tutorial text re-typeset from the intro SWF layout.
   All DRM/gateway/telemetry is dropped, consistent with the zero-network policy
   of `deskmates3d-rs`.
3. **Ruffle's role is marginal**: embed it only to replay `DMU_intro.swf` /
   `ProgressBar.swf` as original artifacts; do not plan the product around it.
4. Assets stay in `~/reference/oska-deskmates/ultra/` (PNGs, ABC string dumps,
   parser scripts); only this report + inventory are committed to
   `deskmates-rs/analysis/`.

### Verification performed
Static only: ZIP/AIR unpack (7z), FLAML tag walk validated to land exactly on
EOF for all three SWFs, ABC v46.16 constant-pool string extraction (10,782
strings total), DefineBitsLossless2→PNG round-trip (305 PNGs, `file(1)`
confirms dimensions), MP3 frame-sync check on extracted audio. No Flash, AIR,
or ActionScript code was executed at any point.

## 7. Content recovery (2026-10-13 Wayback/IA sweep)

Follow-up to §6 step 1. Static + network-fetch only (raw Wayback `id_` fetches;
nothing executed). Corpus: `~/reference/oska-deskmates/ultra/flvs/` (353 media
files, 126.3 MB) + `flvs/pages/` (29 captured site pages); per-file
name/size/md5/sha256/capture-ts/source-url in
[`DESKMATEULTRA_recovery_inventory.txt`](DESKMATEULTRA_recovery_inventory.txt)
(382 rows; `flvs/manifest.tsv` is the resumable fetch log).

### Sweep stats (CDX, `collapse=urlkey` unless noted)

| sweep | rows | outcome |
|---|---|---|
| `deskmateultra.com` (domain) | 514 | 316 status-200 media rows → 353 OK files after multi-ts fetches |
| `dl.deskmateultra.com` (domain) | 4 | only `DeskMateUltra.air` (302) + robots — clip CDN never crawled |
| `secure.oska.com/dmu` (prefix) | 0 | gateway + store paths never archived |
| `dmu_dev.smileware.com.au`, `smileware.com.au` | 0 | dev-leak host absent from index (ports 45002/44002 uncrawled) |
| `oska.com` (domain, prior 74,273-row dump) | — | no `.flv`/`.mp4` anywhere; `dmu.oska.com` = 3 affiliate banners; `images.oska.com` is an unrelated fashion brand |

`gateway.php`: **zero captures on every DMU host** (`deskmateultra.com/app/*`
was never crawled; the lone 2001-era `secure.oska.com:80/gateway.php` capture
is a dead soft-404). No `GetListing` response body exists in the index. A
no-`collapse` `filter=original:.*\.flv` query over the whole domain returns
**empty — not one `.flv` capture exists** for this product line. IA item
searches (`deskmateultra`, `"trinity sb"`, `stripsaver`, `"trinity sunset
boulevard"`, `"trinity porter"`, `oska AND ultra`): **0 relevant items** —
the 112 `deskmate`/10 `oska` software hits are all 1st/2nd-gen line
(`Deskmates`, `more-deskmates`, `tahni-deskmate-1.04`, `tucows_287874`, …).

### The one hard clip-naming win

`tsb_videos.swf` (174 KB, recovered; AS2/AVM1, `mx.video.FLVPlayback` — the
player embedded by `strip-tease-videos.php`) contains the only concrete FLV
paths ever recovered:

```
http://www.deskmateultra.com/tsb_ov_sailor.flv
http://www.deskmateultra.com/tsb_ov_speedy.flv
http://www.deskmateultra.com/tsb_ov_white.flv
```

None of the three was ever archived (CDX empty for each). The `tsb_ov_<clip>.flv`
pattern ("ov" = overview) confirms desktop FLVs sat at the web root; the
paywalled full set would have streamed from the never-crawled
`dl.deskmateultra.com` / `…/app/dl/dmu/` path.

### Recovered video — the free demo clips (12 files)

`strip-tease-mobile-videos.php` gave away 3 demo sequences × 2 rating cuts
(base = **Mature**, `_g` = **General**), each in iPhone (352×288 H.264/AAC)
and 3GP (176×144) transcodes — all 12 captured 2010-09-20 and recovered:

| sequence | cut | m4v (size, md5) | 3gp (size, md5) | dur |
|---|---|---|---|---|
| Ship Ahoy! (sailor) | Mature | 11,831,741 `fab18973…` | 2,965,211 `acf08854…` | 110.0 s |
| Ship Ahoy! | General | 6,515,914 `89270f5f…` | 1,667,141 `316e778b…` | 61.0 s |
| Go Go Speedy (speedy) | Mature | 6,968,863 `bf471fb4…` | 1,765,686 `f7005f81…` | 65.7 s |
| Go Go Speedy | General | 4,883,988 `082beec0…` | 1,219,591 `ed67469c…` | 44.9 s |
| White Pearl (white) | Mature | 18,036,545 `771a5dfc…` | 4,491,368 `e748866a…` | 165.0 s |
| White Pearl | General | 11,108,200 `4fccd8ca…` | 2,821,840 `497ac120…` | 101.8 s |

(full sha256 + capture ts in the inventory; magic-validated `ftyp`, ffprobe'd).
~9.1 min of unique footage (548 s across both cuts). Sequence titles also
confirmed by `strip-tease-screenshots.php` alts: Pole Dancing, Schoolgirl,
White Fur, German Girl, Speed Racer Girl, Sailor Girl.

### Recovered non-video content

- **54 full-resolution 1600×1200 wallpapers** (`TrinitySB_<title>.jpg` +
  thumbs) from `deskmateultra.com/wallpapers/` — the `.air` ships only
  158×138 preview thumbnails, so these are genuinely new (0 byte-identical
  matches; same shoots, dialog-grid titles map 1:1 to the numbered thumbs).
- **9 SWFs**: `tsb_videos.swf` (the web jukebox), `DMU_ControlBox.swf`
  (web mock of the control bar), `tsb_flash_buttons(_trailer).swf` (store
  CTAs), `tsb_flash_seal.swf`, `AIRInstallBadge.swf`, `expressinstall.swf`,
  2 parked-domain junk `Promo_*_embed.swf` (bodisparking artifacts, kept for
  the record).
- **~35 `tsb_support_*.jpg` app-UI screenshots** (install/uninstall/options/
  unlock flows — direct reference art for a Rust shell), 6 sequence promo
  banners (`strip-tease-{pole,schoolgirl,white,german,speedy,sailor}.png`),
  promo/store jpgs, affiliate banners (incl. 3 from `dmu.oska.com`), site chrome.
- **29 site HTML pages** incl. `strip-tease-videos.php` (player embed),
  `dl1d.php` (the `dl.deskmateultra.com/DeskMateUltra.air` funnel), and the
  110 KB `strip-tease-support_temp.php` user manual.

### Still lost

- **The entire paywalled TrinitySB full set** (the $29.95 "Strips & Pole
  Dances" — clip count unknown): served post-unlock from never-crawled
  hosts, no IA copies, no user-uploaded pack found. Gone.
- The three `tsb_ov_*.flv` desktop demo FLVs (names known, bytes gone — only
  the mobile transcodes above survive).
- `GetListing`/gateway payloads, tutorial prose (unchanged from §3).

### Verdict: is a Rust Ultra shell worth building?

**Yes, as a demo-grade shell.** §6's blocking condition is met: there is now
real payload — 3 sequences × 2 cuts of playable H.264 (ffmpeg-decodable),
54 real 1600×1200 wallpapers, 61 preview thumbs, the 305 extracted UI skins,
~35 UI screenshots, and the web control-box mock. A faithful reimplementation
of the *free TrinitySBDemo experience* (transparent control bar, playlist over
the 6 m4v clips, mature/general content filter — the cuts are literally
different files, idle-timeout trigger, wallpaper panel) is fully buildable
offline. The full-set experience is not: the shell must ship demo-only with no
unlock path, and that is the honest ceiling.

**Licensing**: every recovered asset remains © Oska Educational Systems Pty
Ltd (trademarks notice on every page; Verisign-signed 2010 package). This is
local preservation/research material under `~/reference/` — do not
redistribute clips or wallpapers with any shell.

### §7 verification performed

CDX sweeps cached under `/tmp/cdx/dmu_*.txt`; every download magic-validated
(ftyp/FLV/GIF/PNG/JPEG/FWS-CWS-ZWS signatures; 170 soft-404/parked rows logged
FAIL and discarded), sha256-deduped (5 dupes), videos ffprobe'd (codecs,
durations above). SWF string mining was static (zlib inflate + constant-pool
scan / `swf_parse.py`). No archived code was executed.
