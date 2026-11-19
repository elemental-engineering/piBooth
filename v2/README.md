# piBooth v2 (Rust)

Rewrite of the Python booth (`../main.py`). Design: `../docs/superpowers/specs/2026-11-18-rust-v2-design.md`.

Status: milestone 3 — live preview, button and countdown, screen flash,
full-quality capture (high-quality noise reduction, exposure and white balance
locked for the shot), sepia + effect overlay, JPEG with EXIF, review of the
saved photo, status icons and camera auto-restart. Upload to Immich arrives in
milestone 4.

## Photos

Every shot is saved under `storage.root` (default: the working directory):

- `photos/booth_YYYYMMDD-HHMMSS.jpg` — the finished photo (sepia + a random
  overlay from `effects/` when the filter is on, plain otherwise).
- `raw/booth_YYYYMMDD-HHMMSS.jpg` — the unfiltered original
  (`storage.save_original = true`).

Photos are never mirrored, are 1456×1088 (the sensor's full resolution), JPEG
quality `capture.jpeg_quality` (95) with full colour detail, and carry EXIF date,
camera, exposure, ISO and — when `immich.latitude`/`longitude` are set — GPS.

## Develop on a laptop (no Pi needed)

```bash
cargo test
cargo run -- --config config.dev.toml --fake-camera --windowed --no-gpio --bench
```

`config.dev.toml` points `display.assets_dir` at the repo root, where v1's
`fonts/`, `images/` and `effects/` live. Photos taken while developing land in
`v2/photos/` and `v2/raw/`.

Keys: `Space` take a photo, `d` debug overlay (includes the photo count), `r`
mirror preview, `f` filter on/off for the next photos, `Esc`/`q` quit. The red square marks the image's top-left corner; with
mirroring on it appears top-right.

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
