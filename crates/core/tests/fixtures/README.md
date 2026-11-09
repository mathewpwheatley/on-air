# Test fixtures

API responses used by the `on-air-core` unit tests.

| File | Source |
|---|---|
| `openmeteo_*.json` | Recorded from the live Open-Meteo API on 2026-11-08 (Portland, OR). |
| `spotify_*.json` | Hand-written from Spotify's Web API reference. Replace with recorded, scrubbed responses once `tools/sim --live` exists (P2). |
| `art_*.jpg` | Generated test images (solid color blocks), made with `sips`. |

Scrub tokens, user IDs and anything personal before checking in a recorded response.
