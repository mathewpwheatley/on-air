# On-Air

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
| `just sim` | Desktop panel simulator with a fake player (options below) |
| `just snapshots` | Re-render the UI snapshot images after an intended change |
| `just lint` | rustfmt check + clippy (warnings are errors), host and firmware |
| `just build` | Build the firmware image |
| `just flash` | Flash the board and open the serial monitor |
| `just monitor` | Serial monitor only |

## Simulator

`just sim` opens the 64×64 panel in a window and runs the real app logic and screens. Keys 1–4 are the buttons, and `-` / `+` turn the brightness knob. `just sim --help` lists every key and option.

- Options can also go in a `.env` file: copy [.env.example](.env.example) to `.env` and fill it in. Command-line options override it.
- `just sim --city Portland` shows real weather from Open-Meteo, and the clock uses the city's timezone.
- `just sim --live --client-id <id>` follows your real Spotify account. It needs a Spotify developer app set up as in [requirements §10](docs/requirements.md#10-spotify-app-setup-owner-one-time). The first run opens a browser to sign in, and the session is saved to `~/Library/Application Support/on-air-sim/session.json`.
- `just sim --portal` opens the setup portal at <http://127.0.0.1:8080>, the same page the device serves to your phone. WiFi is pretend (the password `wrongpassword` fails on purpose); city search and the Spotify sign-in are real. You can also hold 4 for 5 s while it runs.
- Add `--record <folder>` to save the raw Spotify responses, for use as test fixtures (scrub them first).

## Status

P0–P2 done: scaffolding, core logic, screens and the simulator. Next is P3, panel bring-up on real hardware.
