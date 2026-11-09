# On Air

A wall-powered 64×64 RGB LED panel that shows the album art of whatever is playing on Spotify. It's a DIY take on the [Tuneshine](https://www.tuneshine.rocks/).

- **Hardware:** Waveshare P2.5 64×64 HUB75 panel + Adafruit MatrixPortal S3 (ESP32-S3)
- **Firmware:** Rust (`esp-hal` + Embassy)
- **Setup:** captive WiFi portal for WiFi and Spotify sign-in

See [docs/requirements.md](docs/requirements.md) for the full spec, bill of materials, wiring and milestones, and [docs/implementation-plan.md](docs/implementation-plan.md) for the software build plan.

## Development setup

One-time setup on macOS:

```sh
brew install just sdl2-compat           # task runner, SDL2 for the simulator
cargo install espup espflash esp-generate
espup install --targets esp32s3         # Intel Macs: add --toolchain-version 1.90.0.0
```

Espressif stopped publishing Xtensa Rust builds for Intel Macs after 1.90, so on an Intel Mac the firmware toolchain is pinned there.

Day to day:

| Command | What it does |
|---|---|
| `just test` | Host tests for the shared crates |
| `just sim` | Desktop panel simulator |
| `just lint` | rustfmt check + clippy (warnings are errors), host and firmware |
| `just build` | Build the firmware image |
| `just flash` | Flash the board and open the serial monitor |
| `just monitor` | Serial monitor only |

## Status

P0 (scaffolding) done. The host crates test, the simulator shows a test pattern, and the firmware builds. Next is P1, core logic on the host.
