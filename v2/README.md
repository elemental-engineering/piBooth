# piBooth v2 (Rust)

Rewrite of the Python booth (`../main.py`). Design: `../docs/superpowers/specs/2026-11-18-rust-v2-design.md`.

Status: milestone 1 — live preview at sensor rate. No capture yet.

## Develop on a laptop (no Pi needed)

```bash
cargo test
cargo run -- --fake-camera --windowed --bench
```

Keys: `Esc`/`q` quit, `r` toggle preview mirror. The red square marks the
image's top-left corner; with mirroring on it appears top-right.

## Build on the Raspberry Pi 5

```bash
sudo apt install -y libcamera-dev libclang-dev clang pkg-config cage
pkg-config --modversion libcamera   # must be 0.4.x .. 0.7.x for libcamera crate 0.8
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo build --release --features libcamera
```

Run from a text console (no desktop):

```bash
cage -s -- ./target/release/pibooth --bench
```

## Install as a service

```bash
sudo useradd -m -G video,render,input,gpio booth
sudo mkdir -p /opt/pibooth && sudo cp -r target/release/pibooth config.example.toml ../fonts ../effects ../images /opt/pibooth/
sudo cp /opt/pibooth/config.example.toml /opt/pibooth/config.toml
sudo chown -R booth: /opt/pibooth
sudo cp deploy/pibooth.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now pibooth
```

The unit takes over tty1 (it stops `getty@tty1` via `Conflicts=`); use another VT, e.g. Ctrl+Alt+F2, for a console.

The Immich key goes in `/opt/pibooth/.env` as `PIBOOTH_IMMICH_API_KEY=...`
(or v1's `API_KEY=...`).
