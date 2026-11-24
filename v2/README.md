# piBooth v2 (Rust)

Rewrite of the Python booth (`../main.py`). Design: `../docs/superpowers/specs/2026-11-18-rust-v2-design.md`.

Status: milestone 4 — live preview, button and countdown, screen flash,
full-quality capture (high-quality noise reduction, exposure metered under the
flash), sepia + effect overlay, JPEG with EXIF, review of the
saved photo, status icons, camera auto-restart, and background upload to
Immich that survives being offline and reboots.

## Photos

Every shot is saved under `storage.root` (default: the working directory):

- `photos/booth_YYYYMMDD-HHMMSS.jpg` — the finished photo (sepia + a random
  overlay from `effects/` when the filter is on, plain otherwise).
- `raw/booth_YYYYMMDD-HHMMSS.jpg` — the unfiltered original
  (`storage.save_original = true`).

Photos are never mirrored, are 1456×1088 (the sensor's full resolution), JPEG
quality `capture.jpeg_quality` (95) with full colour detail, and carry EXIF date,
camera, exposure, ISO and — when `immich.latitude`/`longitude` are set — GPS.

## Immich upload

Every finished photo (from `photos/`, not the `raw/` originals) is queued in
`queue/` right after it is saved and uploaded in the background, oldest first,
then added to the album. Nothing is lost while the booth is offline or
restarted; uploads resume on their own.

Configure (v1's `.env` values move as shown):

| v1 `.env` | v2 |
|---|---|
| `API_KEY` | `PIBOOTH_IMMICH_API_KEY` in the environment or `.env` (`API_KEY` still works) |
| `BASE_URL` | `[immich] base_url` in `config.toml`, e.g. `https://immich.example/api` |
| `ALBUM_ID` | `[immich] album_id` (optional) |
| `latitude` / `longitude` | `[immich] latitude` / `longitude` (written into the photo's EXIF; Immich reads it) |

The API key needs the `asset.upload` and `albumAsset.create` permissions (and
`user.read` for the startup check). At startup the log says `Immich OK user=…`
or why not. Failures retry from 5 s, doubling to 10 min; a bad key or unknown
album is retried every 10 min (a bad key or `base_url` pauses all uploads and
is retried every 10 min, and immediately after a restart) — fix the config and
queued photos go up within 10 min.
A new photo does not cut a retry wait short: after a failure the next try is at
the scheduled retry time.
`d` shows the queue length and the last upload error, or
`Upload: off (N queued)` when upload isn't configured.

Good to know:

- Photos taken before milestone 4 are not uploaded automatically.
- Queued photos go to whatever server and album are configured at the moment
  they upload. Delete `queue/` to discard queued photos.
- A reverse proxy in front of Immich must accept request bodies of a few MB
  (nginx: `client_max_body_size`).
- A bad album id still uploads the photos; only the album step is retried.
- A photo that keeps failing is retried every 10 min without blocking the
  others. A wrong key or missing permission (401/403) pauses all uploads for
  10 min; a redirect means `immich.base_url` is wrong (redirects are not
  followed); so does a 404 on the upload endpoint, and it pauses all uploads
  like 401/403.
- Recovery after a crash only looks at `booth_*.jpg` in `photos/`; other JPEGs
  placed there are ignored.
- On start, photos that were saved but not queued (power loss between the two
  steps) are queued.

Without a key or `base_url`, photos are still queued and go up once upload is
configured. HTTPS uses the Pi's certificate store, so a private CA works once
installed with `update-ca-certificates`; plain `http://` on the LAN works too.

## Develop on a laptop (no Pi needed)

```bash
cargo test
cargo run -- --config config.dev.toml --fake-camera --windowed --no-gpio --bench
```

`config.dev.toml` points `display.assets_dir` at the repo root, where v1's
`fonts/`, `images/` and `effects/` live. Photos taken while developing land in
`v2/photos/` and `v2/raw/`.

Keys: `Space` take a photo, `d` debug overlay (includes the photo count), `r`
mirror preview, `f` filter on/off for the next photos, `Esc`/`q` quit. The red
square marks the image's top-left corner; with mirroring on it appears
top-right.

## Button

Same wiring as v1: a push button between GPIO 27 (BCM) and 3.3 V. v2 enables
the internal pull-down and listens for debounced press edges. For a button to
ground instead, set `[button] active_low = true` (internal pull-up).

## Build on the Raspberry Pi 5

```bash
sudo apt install -y libcamera-dev libclang-dev clang pkg-config cage
pkg-config --modversion libcamera   # must be 0.5.x .. 0.7.x for libcamera crate 0.8 (the Pi ships 0.5.2)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
cargo build --release --features libcamera -j 2
```

On a 4 GB Pi, give the build some swap first (Raspberry Pi OS ships 200 MB),
and close the desktop/browser while it compiles:

```bash
sudo dphys-swapfile swapoff
sudo sed -i 's/^#\?CONF_SWAPSIZE=.*/CONF_SWAPSIZE=2048/' /etc/dphys-swapfile
sudo dphys-swapfile setup && sudo dphys-swapfile swapon
```

Run from a text console (no desktop), from the repo root so `fonts/` and
`images/` are found:

```bash
cd .. && NO_COLOR=1 cage -s -- v2/target/release/pibooth --bench > bench.log 2>&1
```

Check `top` while it runs: pibooth should be well under one full core.

## Install as a service

```bash
sudo useradd -m -G video,render,input,gpio booth
sudo mkdir -p /opt/pibooth && sudo cp -r target/release/pibooth config.example.toml ../fonts ../effects ../images /opt/pibooth/
sudo cp /opt/pibooth/config.example.toml /opt/pibooth/config.toml
sudo chown -R booth: /opt/pibooth
sudo touch /opt/pibooth/.env && sudo chown booth: /opt/pibooth/.env && sudo chmod 600 /opt/pibooth/.env
sudo cp deploy/pibooth.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now pibooth
```

The `gpio` group exists on Raspberry Pi OS; elsewhere create it first with `sudo groupadd gpio`.

The unit takes over tty1 (it stops `getty@tty1` via `Conflicts=`); use another VT, e.g. Ctrl+Alt+F2, for a console.

The Immich key goes in `/opt/pibooth/.env` as `PIBOOTH_IMMICH_API_KEY=...`
(or v1's `API_KEY=...`).
