# piBooth v2 (Rust) — Design

Date: 2026-11-18
Status: Draft for review

## 1. Goal

Rewrite the Python photo booth (`main.py`, v1.0.1) in Rust, keeping its
behaviour (live mirrored preview, button-triggered countdown, sepia + random
effect overlay, Immich upload to album with GPS, status icons, debug OSD) while
fixing v1's defects and extracting the full performance and image quality of
the Raspberry Pi Global Shutter Camera.

### Target hardware

| Item    | Value                                                        |
|---------|--------------------------------------------------------------|
| Board   | Raspberry Pi 5                                               |
| Sensor  | Sony IMX296 **colour**, 1456×1088, global shutter, 10-bit, ~60 fps |
| Display | 1920×1080 HDMI                                               |
| Session | Wayland kiosk (`cage`) — no bare-KMS for now                 |
| Input   | Momentary switch on GPIO 27 (BCM), keyboard for maintenance  |
| Flash   | Configurable: on-screen white flash, or GPIO-driven strobe   |

### Success criteria

1. Live preview runs at the sensor rate (≥ 58 fps sustained on 1080p) with the
   OSD on, measured by `--bench`.
2. Saved photos are **not mirrored**, are produced from the full-quality ISP
   path (HQ noise reduction) and, when stacking is enabled, from a multi-frame
   average.
3. No network, disk or sensor-monitor operation can stall the preview.
4. Every captured photo is persisted locally before upload and is eventually
   uploaded once Immich is reachable, across restarts.
5. Pure logic (state machine, geometry, filters, stacking, queue, config) is
   covered by unit tests that run on x86 without camera or GPIO.

### Non-goals (for v2)

- Bare DRM/KMS rendering without a compositor (possible later).
- Zero-copy dmabuf preview import (milestone 6, optional).
- Hardware sensor-trigger strobe sync (milestone 6, optional).
- Multi-camera, printing, touch UI, web admin.

## 2. v1 defects that v2 must not reproduce

| v1 location | Defect | v2 resolution |
|---|---|---|
| `main.py:333,343,347` | Save and upload commented out | Always save; upload via queue |
| `main.py:44` | Crash if `photos/` missing | Create output dirs at startup |
| `main.py:219` | gpiod v1 API vs pinned v2; hard-coded `gpiochip4` | `gpiocdev`, line looked up by name/offset across chips |
| `main.py:379` | Level-polled button, no debounce/pull | Edge events, debounce, bias pull-up, configurable active level |
| `main.py:132,436` | Blocking HTTP without timeout in render loop | Async tasks with timeouts; render never awaits I/O |
| `main.py:175` | Letterbox by pixel deltas (overflow on small screens) | Scale = min(w ratio, h ratio) |
| `main.py:279,336` | Saved image mirrored | Mirror only in the preview shader |
| `main.py:76` | Sepia matrix applied to BGR; normalised by image max | Correct channel order, fixed clamp, no content-dependent gain |
| `main.py:169` | `fps` never applied | Frame duration set explicitly |
| `main.py:398` | Spawns `vcgencmd` every second | Read `/sys/class/thermal/thermal_zone0/temp` |
| `main.py:282–371` | ~6 full-frame CPU copies + per-frame text rasterisation | GPU composition; OSD glyphs/icons pre-rasterised |

## 3. Architecture

Single binary crate `pibooth` (in `v2/` at repo root, v1 left intact until v2
replaces it).

```
              ┌──────────── camera thread ────────────┐
              │ libcamera: preview stream (RGB, 1456×1088)
              │            capture stream (full-res, HQ NR) + raw
              └───┬───────────────────────┬───────────┘
        latest frame (triple buffer)   CaptureBurst (channel)
                  │                       │
   ┌──────────────▼─────────┐   ┌─────────▼──────────────┐
   │ render loop (main thd) │   │ pipeline worker (rayon) │
   │ GL: texture, letterbox,│   │ stack → filter → effect │
   │ mirror, OSD layers     │   │ → JPEG/EXIF (+DNG)      │
   └──────▲────────┬────────┘   └─────────┬──────────────┘
          │        │ commands             │ PhotoSaved
   ┌──────┴────────▼────────┐             │
   │     BoothState machine │◄────────────┘
   └──────▲────────▲────────┘
  InputEvent│      │StatusUpdate          ┌──────────────┐
   ┌────────┴──┐ ┌─┴──────────┐  enqueue  │ upload task  │
   │ input     │ │ monitors   │ ─────────►│ (tokio, disk │
   │ GPIO/keys │ │ temp/disk/ │           │  queue)      │
   └───────────┘ │ net        │           └──────────────┘
                 └────────────┘
```

Threads: main (winit event loop + GL render + state machine), camera
(libcamera callbacks), a rayon pool for the pipeline, plain threads for the
monitors and the GPIO button (milestone 2), and a tokio runtime for upload (milestone 4). Communication is
by `crossbeam-channel` messages; the only shared memory is the preview frame
triple buffer and an `Arc<AtomicStatus>`-style status snapshot.

### 3.1 Modules

| Module | Responsibility | Depends on |
|---|---|---|
| `config` | Load `config.toml` + env (`PIBOOTH_IMMICH_API_KEY` or `.env`); validate; defaults | serde, toml, dotenvy |
| `state` | Pure booth state machine | none |
| `geometry` | Letterbox rect, OSD layout in normalised units | none |
| `camera` | `Camera` trait; `LibcameraCamera` and `FakeCamera` (test pattern) | libcamera |
| `display` | winit window (fullscreen), glutin/glow GL context, shaders, textures, draw | winit, glutin, glow |
| `osd` | Text/icon rasterisation and scene layout, no GL | fontdue, image |
| `input` | `Button` trait; `GpioButton` (gpiocdev) and keyboard mapping → `InputEvent` | gpiocdev |
| `flash` | `Flash` trait; `ScreenFlash`, `GpioStrobe` (milestone 5; the screen flash is a view flag until then) | gpiocdev |
| `pipeline` | Align + average burst, sharpest-frame pick, sepia, effect composite, encode, EXIF, DNG | rayon, image, turbojpeg, little_exif |
| `storage` | Filenames, directory creation, disk usage (statvfs) | nix |
| `upload` | Persistent queue, Immich client, retries | tokio, reqwest |
| `monitor` | CPU temp, disk %, Immich reachability → `StatusUpdate` | std threads, nix (statvfs) |
| `app` | Wires everything; owns the render loop | all |

Crate versions are pinned in `Cargo.toml` at scaffold time; libcamera-rs
viability is verified in milestone 1 (see §7 Risks).

## 4. Behaviour

### 4.1 State machine

```
Idle ──press──► Countdown{deadline} ──deadline──► Capturing ──burst done──► Review{until}
 ▲                    │                                                       │
 └──── idle_timeout ─ Live ◄───────────────────── review_timeout ─────────────┘
                      └──press──► Countdown
```

- `Idle`: live preview with "Photo Shoot / Press to start" title. Entered at
  start and after `idle_timeout_s` (default 120) without a press.
- `Live`: live preview, no title.
- `Countdown`: shows `N … 1` (default 5 s, matches v1's visible digits).
  On entry: request AE/AWB metering. At `deadline - flash.lead_ms` trigger
  flash (screen: white frame; strobe: arm). At deadline: lock AE/AWB and
  request a capture burst.
- `Capturing`: preview continues (screen flash stays white until burst
  complete or `flash.max_ms`). Presses ignored.
- `Review`: shows the processed photo for `review_timeout_s` (default 5).
  Presses ignored. The pipeline delivers a preview-resolution image first so
  review starts without waiting for JPEG/DNG encoding.
- Button presses are edge events; one press = one event. Holding the button
  does not retrigger.

Maintenance keys (keyboard): `Esc`/`q` quit, `Space` = press, `d` debug OSD,
`r` mirror preview toggle, `f` filter toggle. Key auto-repeat is ignored; `Q` works like `q`.

### 4.2 Camera

- Configuration: two streams from the Pi 5 ISP.
  - Preview: RGB888 at 1456×1088 (sensor native; the GPU scales to 1445×1080
    letterboxed on 1080p), frame duration fixed at sensor max (~16.6 ms).
  - Capture: full-res RGB888 with `NoiseReductionMode = HighQuality`, plus the
    raw Bayer stream when `capture.save_dng = true`.
- AE/AWB: metered during countdown (with the screen flash on when
  `flash.mode = "screen"`, so the lock matches the lit scene), then locked
  (`AeEnable=false`, `AwbEnable=false` with the converged exposure/gain/colour
  gains applied manually) for the burst; unlocked on entering `Review`.
- Burst: `capture.burst_frames` (default 6) consecutive frames at 60 fps
  (~100 ms). With global shutter no rolling distortion exists, so alignment is
  translation-only.
- Tuning file: default `imx296.json` from libcamera; overridable in config.
- `FakeCamera` produces an animated test pattern at 60 fps for laptop dev and
  CI smoke tests.

### 4.3 Display

- Fullscreen winit window under `cage`; vsync on.
- One textured quad for the camera frame, letterboxed by `geometry`; mirroring
  by flipping the U coordinate (`display.mirror_preview`, default true).
- Preview upload: `glTexSubImage2D` of the newest frame from the triple buffer
  (~4.7 MB/frame; within Pi 5 budget at 60 fps). A persistent pixel buffer is
  used if profiling shows stalls. Zero-copy dmabuf import is milestone 6.
- OSD layers drawn as alpha-blended quads from pre-rasterised atlases:
  status icons (top-left, 3% of height, as v1), debug text (top-right),
  countdown digit, idle title/subtitle. Fonts: `fonts/Carnevalee Freakshow.ttf`
  and `fonts/arial.ttf`, sizes scaled from v1's values for 1080p.
- Screen flash: full-screen white quad.

### 4.4 Image pipeline

Input: N full-res RGB frames (+ optional raw). Steps, on the rayon pool:

1. **Stack** (`capture.stack = "average" | "sharpest" | "off"`):
   - `average`: estimate integer-pixel translation of each frame vs. the
     sharpest one by phase correlation on a downscaled luma; reject frames
     whose shift exceeds `capture.max_shift_px` (default 8); average the rest
     in `u16` accumulators.
   - `sharpest`: pick the frame with highest variance of Laplacian.
2. **Save original**: unfiltered stacked image → `raw/booth_<ts>.jpg`
   (`storage.save_original`, default true). DNG of the first raw frame →
   `raw/booth_<ts>.dng` when enabled.
3. **Filter** (when filter enabled): sepia with the standard RGB matrix,
   clamped to 255 (no max normalisation).
4. **Effect**: a random PNG from `effects/`, resized once at startup to the
   capture size (Lanczos), alpha-composited.
5. **Encode**: JPEG quality 95, 4:4:4, with EXIF `DateTimeOriginal`, `Make`/
   `Model`, and GPS lat/long when configured → `photos/booth_<ts>.jpg`.
   Write to a temp file and rename for atomicity.
6. Emit `PhotoSaved { path, preview_rgba }` to the state machine and enqueue
   upload.

### 4.5 Flash

`flash.mode = "screen" | "strobe" | "none"`.

- `screen`: white frame from `deadline - lead_ms` (default 300 ms, to let AE
  meter it) until the burst completes.
- `strobe`: GPIO output line (`flash.gpio_line`, active level configurable)
  driven high for `flash.pulse_ms` covering the burst, timed from the camera's
  frame-start metadata. Exact per-exposure sync via the IMX296 external
  trigger is out of scope for v2 (milestone 6).

### 4.6 Upload

- Queue: one JSON file per pending photo in `queue/` (path, timestamp,
  attempts). Survives restarts; scanned at startup.
- Worker: `POST {base_url}/assets` (multipart, `deviceAssetId`,
  `deviceId = "piBooth"`, ISO-8601 `fileCreatedAt`/`fileModifiedAt`), then
  `PUT /albums/{id}/assets` when `album_id` is set. GPS comes from EXIF;
  the separate `PUT /assets` call is kept only as a fallback if Immich does
  not populate location from EXIF (verified during milestone 4).
- Response `created` or `duplicate` both count as success.
- Timeouts: connect 5 s, total 60 s. Exponential backoff 5 s → 10 min cap.
  Paused while the network monitor reports offline.
- Queue file removed on success. Local photos are never deleted by v2.

### 4.7 Monitors

On their own threads, each updating a shared `Status` snapshot read by the render loop:

| Monitor | Interval | Warning | Critical | Source |
|---|---|---|---|---|
| CPU temp | 1 s | > 60 °C | ≥ 65 °C | `/sys/class/thermal/thermal_zone0/temp` |
| Disk | 5 min + after each save | > 70 % | ≥ 90 % | statvfs on `storage.root` |
| Network | 60 s (10 s while offline) | — | unreachable | TCP connect to the `immich.base_url` host:port, 5 s timeout |

Thresholds and intervals are configurable. Icons: `images/*.png` as in v1.

### 4.8 Configuration

`config.toml` (example committed as `config.example.toml`):

```toml
[storage]
root = "."            # photos/, raw/, queue/ under here
save_original = true

[camera]
tuning_file = ""      # empty = libcamera default

[capture]
burst_frames = 6
stack = "average"     # average | sharpest | off
max_shift_px = 8
save_dng = false
jpeg_quality = 95

[timing]
countdown_s = 5
review_s = 5
idle_s = 120

[display]
mirror_preview = true
assets_dir = "."

[filter]
enabled = true

[flash]
mode = "screen"       # screen | strobe | none
lead_ms = 300
max_ms = 800
gpio_line = 17
pulse_ms = 150
active_high = true

[button]
gpio_line = 27
active_low = false   # v1 wiring: button to 3.3 V, pull-down
debounce_ms = 30

[immich]
base_url = "https://immich.example/api"
album_id = ""
latitude = 0.0
longitude = 0.0       # GPS omitted when both are 0.0 or absent

[monitor]
temp_warn_c = 60
temp_crit_c = 65
disk_warn_pct = 70
disk_crit_pct = 90
```

Secret: `PIBOOTH_IMMICH_API_KEY` env var, falling back to `API_KEY` in
`.env` for v1 compatibility. Missing key or URL disables upload with a log
warning (as v1), queue still filled so photos upload once configured.

CLI: `pibooth [--config PATH] [--fake-camera] [--no-gpio] [--bench]`.

## 5. Error handling

- Startup: config errors, missing fonts/effects, or camera not found → log and
  exit non-zero (systemd restarts). Missing GPIO → warning, keyboard-only.
- Runtime camera error (request failure, sensor drop) → OSD error banner,
  camera restart with backoff; state returns to `Idle`.
- Pipeline failure on a capture → log, show "Oops, try again" during review,
  keep any original already written.
- Upload/monitor errors never propagate to the render loop; they surface only
  as icons and logs.
- Logging: `tracing` to stderr (journald), level from `RUST_LOG`.

## 6. Testing

- Unit (x86, CI): `state` with a fake clock (all transitions, ignored presses,
  idle timeout, hold-doesn't-retrigger), `geometry` (1080p, 800×480, 4:3,
  portrait), sepia/effect on small fixtures, stacking (synthetic shifted noisy
  frames: SNR improves, outlier rejected), queue persistence/recovery,
  config parsing/defaults, Immich client against a mock HTTP server.
- Integration (x86): `--fake-camera --no-gpio` headless smoke test runs the
  pipeline end-to-end and produces a JPEG with expected EXIF.
- On Pi (manual): `--bench` reports preview fps, frame-to-photon estimate,
  capture-to-review latency, per-stage pipeline timings.

## 7. Risks

| Risk | Mitigation |
|---|---|
| `libcamera` crate lacks needed controls/streams on PiSP | Milestone 1 spike validates; fallback is a small C++ shim via `cxx` over libcamera |
| Dual-stream config at full res + raw not accepted by PiSP | Fall back to mode switch for capture (brief preview pause during flash) |
| Texture upload at 60 fps too slow | PBO streaming, then dmabuf import (milestone 6) |
| Average stacking blurs moving subjects | Shift rejection + `sharpest` mode; default chosen after Pi testing |
| Screen-flash metering inconsistent | `lead_ms` tunable; option to fix exposure manually |

## 8. Milestones

1. **Preview**: scaffold crate, config, `FakeCamera`, GL display, libcamera
   preview at 60 fps on 1080p under `cage`, `--bench`.
2. **Booth loop**: state machine, GPIO button + keys, OSD (title, countdown,
   icons, debug), monitors, screen flash.
3. **Quality capture**: capture stream, AE/AWB lock, sepia + effect, JPEG +
   EXIF, original saving, review screen.
4. **Upload**: persistent queue, Immich client, album + GPS.
5. **Quality+**: burst stacking, DNG, GPIO strobe.
6. **Optional**: zero-copy dmabuf preview, IMX296 hardware trigger sync,
   bare-KMS mode.

## 9. Deployment

- Build natively on the Pi (`cargo build --release`) or cross-compile for
  `aarch64-unknown-linux-gnu` with `libcamera-dev` sysroot.
- System packages: `libcamera-dev`, `cage`, Mesa GLES.
- systemd unit runs `cage -- /opt/pibooth/pibooth --config /opt/pibooth/config.toml`
  with `Restart=always`. Unit file and install notes in `v2/README.md`.
