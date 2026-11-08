# On Air

A wall-powered 64×64 RGB LED panel that shows the album art of whatever is playing on Spotify. It's a DIY take on the [Tuneshine](https://www.tuneshine.rocks/).

- **Hardware:** Waveshare P2.5 64×64 HUB75 panel + Adafruit MatrixPortal S3 (ESP32-S3)
- **Firmware:** Rust (`esp-hal` + Embassy)
- **Setup:** captive WiFi portal for WiFi and Spotify sign-in

See [docs/requirements.md](docs/requirements.md) for the full spec, bill of materials, wiring and milestones, and [docs/implementation-plan.md](docs/implementation-plan.md) for the software build plan.

## Status

Planning. Next step is milestone M0, getting the panel lit from Rust.
