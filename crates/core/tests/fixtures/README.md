# Test fixtures

API responses used by the `on-air-core` unit tests.

| File | Source |
|---|---|
| `openmeteo_*.json` | Recorded from the live Open-Meteo API on 2026-11-08 (Portland, OR). |
| `spotify_recorded_track.json` | Recorded from the real API with `just sim --record` on 2026-11-08. The playlist ID was replaced with a placeholder. |
| `spotify_*.json` (others) | Hand-written from Spotify's Web API reference, for cases not recorded yet: paused, podcasts, local files, ads, several artists, token replies. |
| `art_*.jpg` | Generated test images (solid color blocks), made with `sips`. |

Raw recordings go to `recorded/`, which is git-ignored. Remove anything personal (tokens, user IDs, your own playlist IDs) before copying one up here.
