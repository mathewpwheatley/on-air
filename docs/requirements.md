# On-Air — Product Requirements

**Status:** Draft v0.1 · **Date:** 2026-11-07 · **Owner:** Mathew Wheatley

## 1. Overview

On-Air is a DIY, wall-powered 64×64 RGB LED panel that shows the album art of whatever is playing on the owner's Spotify account. It also has a brightness knob and four playback buttons. Think of it as a home-built [Tuneshine](https://www.tuneshine.rocks/).

### Goals

- Show the current track's album art within a few seconds of the track changing.
- Set up WiFi and Spotify from a phone through a captive portal, with no reflashing.
- Physical controls: brightness knob, play/pause, previous, next, mode. Powered from a single USB-C supply; unplug to turn off.
- When no music is playing, show a clock, the local weather, both, or nothing.
- Firmware written in Rust.
- Built from off-the-shelf modules, with no custom PCB in v1. The enclosure is 3D printed and designed separately.

### Non-goals (v1)

- Battery power. The device always runs from the wall.
- Streaming services other than Spotify.
- Multiple Spotify accounts per device.
- Over-the-air firmware updates (candidate for v2).
- A companion mobile app.

## 2. User stories

1. As the owner, I plug the device in for the first time and the panel tells me how to join its setup network.
2. As the owner, I enter my WiFi and Spotify details on my phone, and the panel starts showing album art.
3. As a listener, when the song changes, the art on the panel changes too.
4. As a listener, I turn the knob to dim or brighten the panel. Turning it fully down gives a minimum brightness that is still visible; the knob never turns the screen off.
5. As a listener, I press buttons on the device to pause, skip or go back.
6. As the owner, when no music is playing, the panel shows a clock (analog or digital), the weather, a clock plus the weather, or nothing, instead of stale art. I pick which with the mode button.
7. As the owner, if my WiFi password changes, I hold a button to reopen the setup portal.
8. As a listener, I hold Play/Pause to see the song title, artist and album.

## 3. Functional requirements

### 3.1 Display

| ID | Requirement |
|---|---|
| D-1 | Drive a 64×64 HUB75 panel (1/32 scan, A–E address lines) at a refresh rate of at least 100 Hz with no visible flicker. |
| D-2 | Render a 64×64 RGB888 framebuffer with gamma correction and at least 6 bits per color channel. |
| D-3 | Fetch the 300×300 JPEG that Spotify provides for each album and shrink it to 64×64 with a box filter, which looks sharper on the panel than Spotify's own 64 px image. If no 300 px image exists, use the smallest larger one, else the largest available. |
| D-4 | Crossfade between covers over about 300 ms. |
| D-5 | When playback is paused, dim the art to about 40% of the current brightness. |
| D-6 | Idle screens, cycled with the mode button: **Analog clock**, **Digital clock**, **Weather**, **Clock + weather**, **Weather + tides** (only where the city has tide data, see W-6), **Last song** (the most recent cover stays on screen, dimmed like a paused cover; blank until something has played since power-up), or **None** (panel dark). Each Mode tap first shows "IDLE MODE", the mode's name and a row of position dots for about 2 s. Enter the idle screen after nothing has played for the owner's idle time (set in the portal, 1–30 min, default 5 min), and return to album art as soon as playback starts. The selected screen is saved and survives reboots. |
| D-7 | Status screens: setup instructions plus a WiFi-join QR code, "Connecting…", "Spotify sign-in needed", and a "No WiFi" error icon. |
| D-8 | Brightness from the knob scales the whole framebuffer. Firmware caps the total estimated current below the power supply's rating (see §5.2). |
| D-9 | **Clock:** time comes from SNTP in the configured timezone. Digital shows HH:MM (12 h or 24 h, configurable) with the date underneath. Analog shows a round face with hour and minute hands, updated every minute. Before the first SNTP sync, show "--:--" instead of a wrong time. |
| D-10 | **Weather:** a condition icon (sun, cloud, rain, snow, storm, fog), the current temperature, and today's high and low. Units are °C or °F (configurable). |
| D-11 | **Clock + weather:** a digital clock on top, the date under it, and a weather icon plus current temperature at the bottom. |
| D-14 | **Weather + tides:** a weather icon plus current temperature on top, the date under it, then the next high and low tide times, each after a small wave icon with an up or down arrow. Shows "--:--" until tide times arrive. |
| D-12 | If weather data is unavailable or older than 2 h, the weather screens show the last known data with a small "stale" marker, or a "No weather" icon if there is none. The clock keeps working. |
| D-13 | **Track info:** title, artist and album (or show, for podcasts) over the cover dimmed to about 20%, for 5 s. Lines too wide for the panel scroll. Shown by holding Play/Pause for 2 s. |

```mermaid
stateDiagram-v2
    [*] --> Idle: boot, nothing playing
    [*] --> Playing: boot, track playing
    Playing: Album art
    Paused: Album art dimmed to about 40%
    Playing --> Paused: pause
    Paused --> Playing: play
    Playing --> Idle: nothing playing for the idle time
    Paused --> Idle: nothing played for the idle time
    Idle --> Playing: playback starts

    state Idle {
        [*] --> Digital: default, or the last saved choice
        Digital: Digital clock
        Analog: Analog clock
        Weather: Weather
        ClockWeather: Clock + weather
        WeatherTides: Weather + tides (only with tide data)
        LastSong: Last song (most recent cover)
        Off: None (panel dark)
        Analog --> Digital: Mode tap
        Digital --> Weather: Mode tap
        Weather --> ClockWeather: Mode tap
        ClockWeather --> WeatherTides: Mode tap, with tide data
        ClockWeather --> LastSong: Mode tap, without tide data
        WeatherTides --> LastSong: Mode tap
        LastSong --> Off: Mode tap
        Off --> Analog: Mode tap
    }
```

### 3.2 Spotify

| ID | Requirement |
|---|---|
| S-1 | Authenticate with OAuth 2.0 Authorization Code + PKCE. The device never stores a client secret. |
| S-2 | Store the refresh token in flash. Spotify may return a new refresh token on each refresh, so save it whenever it does. |
| S-3 | Poll `GET /v1/me/player/currently-playing` every 5 s while playing, every 30 s while idle, and every 60 s after 30 min with nothing playing. After any playback button, poll quickly (0.4 s, then 0.6, 1, 1.5 and 2 s apart) until Spotify reports the change, since it can take a second or two to show a skip. Also schedule a poll for the moment the current track should end, using `progress_ms` and `duration_ms`. |
| S-4 | Handle `204 No Content` (nothing playing), `401` (refresh the token and retry once), and `429` (honour the `Retry-After` header). |
| S-5 | Re-download art only when the album image URL changes. Cache the last cover in RAM. |
| S-6 | Buttons call `PUT /me/player/play`, `PUT /me/player/pause`, `POST /me/player/next` and `POST /me/player/previous`. Requires Spotify Premium. Fail gracefully without it. |
| S-7 | Required scopes: `user-read-currently-playing user-read-playback-state user-modify-playback-state`. |

### 3.3 Weather

| ID | Requirement |
|---|---|
| W-1 | Get weather from [Open-Meteo](https://open-meteo.com/) (`GET /v1/forecast` with `current=temperature_2m,weather_code` and `daily=temperature_2m_max,temperature_2m_min`). It is free for personal use and needs no API key. |
| W-2 | Refresh every 15 min while an idle screen that shows weather is selected. Don't fetch weather at all when the selected idle screen doesn't show it. |
| W-3 | Map WMO `weather_code` values to the small set of icons in D-10. |
| W-4 | Location is set in the portal as a city name. The device turns it into latitude and longitude once, using the Open-Meteo geocoding API, and stores the result. |
| W-5 | On errors, retry with exponential backoff (max 15 min) and keep showing the last good data (see D-12). |
| W-6 | When a new city is saved in the portal, check it for tide data once with the Open-Meteo marine API (`GET /v1/marine` with `hourly=sea_level_height_msl` and `cell_selection=sea`) and store the answer. Offer the Weather + tides screen only when tide data exists. Inland places, and some sheltered water such as Puget Sound, have none. The portal says under the city whether the tide screen is on offer. If the check can't reach Open-Meteo, the answer stays unknown, the tide screen stays hidden, and the device checks again later. |
| W-7 | Find high and low tides as the peaks and dips of the hourly sea level height, placing each between the hours with a parabola fit. Fetch 3 days ahead, every 6 h, only while the Weather + tides screen is selected. |

### 3.4 Setup portal

| ID | Requirement |
|---|---|
| P-1 | If no WiFi credentials are stored, or the stored network can't be joined after 30 s, start a soft AP named `OnAir-XXXX` (last 4 hex digits of the MAC), short enough to fit across the panel. |
| P-2 | Run a DNS responder that answers every query with the device's IP, so phones open the captive-portal page automatically. |
| P-3 | The portal page has four steps: **WiFi** (nearby networks or a typed name, and the password), **Spotify** (Client ID, the app setup instructions and a **Connect Spotify** link), **Settings** (city search, required since it sets the clock's timezone; temperature units; 12/24 h clock; idle time, 1–30 min) and **Finish**. The clock's timezone comes from the chosen city. |
| P-4 | **Spotify sign-in (paste-back flow):** the link opens Spotify's authorize page with `redirect_uri=http://127.0.0.1:8888/callback`. After approval the browser lands on a page that fails to load. The user copies that full URL into the portal. The device extracts `code` and exchanges it, together with its stored PKCE verifier, for tokens. To reach Spotify, the device joins the home WiFi while keeping the setup AP running (AP+STA mode). See Q-4. *(This is needed because Spotify only accepts HTTPS or loopback redirect URIs, and the device can't serve HTTPS with a trusted certificate.)* |
| P-5 | On success, save the settings, show a confirmation, and reboot into normal mode. |
| P-6 | Holding **Mode** for 5 s at any time reopens the portal. Holding **Mode + Play** for 10 s erases all settings. |
| P-7 | The panel shows a QR code for joining the setup network, with the network name centered underneath. |

```mermaid
sequenceDiagram
    actor Owner as Owner's phone
    participant Dev as On-Air
    participant Acc as accounts.spotify.com

    Dev->>Dev: Start soft AP, generate PKCE verifier + challenge
    Owner->>Dev: Join OnAir-XXXX (captive portal opens)
    Owner->>Dev: Submit WiFi, Client ID, timezone, city, units
    Dev->>Dev: Join home WiFi while keeping the soft AP up
    Owner->>Acc: Tap Connect Spotify (authorize URL with code_challenge)
    Acc-->>Owner: Redirect to http://127.0.0.1:8888/callback?code=...
    Note over Owner: Page fails to load. Owner copies the full URL.
    Owner->>Dev: Paste callback URL into portal
    Dev->>Acc: Exchange code + PKCE verifier for tokens (over home WiFi)
    Acc-->>Dev: Access token + refresh token
    Dev->>Dev: Save settings, show confirmation, reboot
```

### 3.5 Controls

| Control | Input | Behaviour |
|---|---|---|
| Knob (10 kΩ linear) | ADC1 pin | Brightness. Read at 20 Hz, smoothed, with hysteresis and a perceptual curve. Fully down sets a minimum brightness floor (configurable, default about 5%) and never blanks the screen. |
| Button 1 | GPIO, internal pull-up | Play/Pause: tap to play or pause, hold 2 s to show track info |
| Button 2 | GPIO | Previous |
| Button 3 | GPIO | Next |
| Button 4 | GPIO | Mode: tap to cycle the idle screen (analog clock → digital clock → weather → clock + weather → weather + tides, where available → last song → none), hold 5 s for the portal |

Buttons are debounced in software (20 ms) and give quick visual feedback on the panel (a small icon for about 500 ms).

Tapping **Mode** while music is playing shows a preview of the next idle screen for about 4 s, then returns to the album art, so the owner can pick a screen without stopping the music.

## 4. Non-functional requirements

| ID | Requirement |
|---|---|
| N-1 | **Latency:** when a track ends on its own, new art shows within 2 s (thanks to the end-of-track poll in S-3). After a skip from another device, new art shows within 7 s. When music starts from idle, art shows within 32 s, or within 62 s after 30 min with nothing playing. After a playback button on the device, art updates within about 2 s. |
| N-2 | **Boot:** from power-on to art in under 10 s on a known network. |
| N-3 | **Resilience:** reconnect automatically after WiFi drops, DNS failures and Spotify outages, with exponential backoff (max 60 s). No manual reboot should ever be needed. |
| N-4 | **Watchdog:** a hardware watchdog resets the device if the main loop stalls for more than 30 s. |
| N-5 | **Security:** settings live in a dedicated flash partition. The portal is only reachable on the setup AP, never on the home network. HTTPS to Spotify verifies certificates (see risk R-2). |
| N-6 | **Logging:** structured logs over USB serial using `defmt` or `log`. |
| N-7 | **Testability:** Spotify JSON parsing, the state machine and image handling live in a `no_std` crate with host-side unit tests. |

## 5. Hardware

### 5.1 Bill of materials

What's still to buy, with links, is in [shopping-list.md](shopping-list.md).

| # | Part | Notes | Have? |
|---|---|---|---|
| 1 | Waveshare RGB-Matrix-P2.5-64×64 | HUB75, 1/32 scan, 160×160 mm | ✅ |
| 2 | Adafruit MatrixPortal S3 (#5778) | ESP32-S3, 8 MB flash, 2 MB PSRAM, HUB75 socket, level shifters, NeoPixel, 2 onboard buttons | Buy |
| 3 | 5 V 3 A USB-C power supply (15 W) | Must supply 3 A at 5 V. Plain USB-C, no PD negotiation needed. | Buy |
| 4 | 10 kΩ linear potentiometer + knob | Brightness | ✅ |
| 5 | 4-button momentary board | Check whether it has its own pull-ups or an LED, and the voltage it expects | ✅ |
| 6 | 26 AWG wire (signals), Dupont/JST-PH leads | | COTS |
| 7 | Enclosure plus LED diffuser | 3D printed, including the diffuser (for example a printed grid or a thin translucent layer). Designed separately, outside this doc. Leave an opening for the MatrixPortal's USB-C port. | Print |

### 5.2 Power budget

| Load | Typical | Worst case |
|---|---|---|
| Panel showing album art, mid brightness | 1.0–1.5 A | — |
| Panel at full white, full brightness | — | ~4 A *(confirm with the Waveshare datasheet or a bench measurement)* |
| MatrixPortal S3 with WiFi | 0.1–0.25 A | 0.35 A |
| **Total at 5 V, uncapped** | **~1.5 A** | **~4.3 A** |
| **Total at 5 V, with firmware cap** | **~1.5 A** | **≤ 2.5 A** |

Firmware enforces a current cap (D-8). It estimates draw from the sum of pixel values multiplied by brightness and scales the brightness down to stay under about 2.5 A, which leaves headroom on a 3 A USB-C supply. Typical album art never reaches the cap; only very bright, mostly white images get dimmed.

If the cap ever dims images too much (or a second panel is added), power the panel from a separate 5 V 4 A+ supply and keep the MatrixPortal on USB-C, as Adafruit does in its larger builds.

### 5.3 Wiring

```mermaid
flowchart LR
    PSU["5 V 3 A USB-C supply"] --> MP["MatrixPortal S3<br/>USB-C port"]
    MP -- "5 V / GND screw terminals" --> PANEL["Panel power lead<br/>VCC / GND"]

    MP == "HUB75 socket plugs directly into" ==> HUB["Panel HUB75 IN"]

    subgraph header["MatrixPortal S3 header"]
        V33["3V3"]
        GND["GND"]
        A0["A0 (ADC1 pin)"]
        A1["A1"]
        A2["A2"]
        A3["A3"]
        A4["A4"]
    end

    subgraph pot["10 kΩ pot"]
        P1["End 1"]
        PW["Wiper"]
        P2["End 2"]
    end

    subgraph buttons["Button board<br/>(buttons pull the pin to GND;<br/>firmware enables internal pull-ups)"]
        COM["COM / GND"]
        B1["BTN1 Play/Pause"]
        B2["BTN2 Previous"]
        B3["BTN3 Next"]
        B4["BTN4 Mode"]
    end

    V33 --- P1
    GND --- P2
    PW --> A0
    GND --- COM
    B1 --> A1
    B2 --> A2
    B3 --> A3
    B4 --> A4
```

Notes:

- **Power path:** USB-C is the only power input. The MatrixPortal passes 5 V from USB-C out through its screw terminals to the panel, which is how Adafruit intends the board to be powered. Never feed 5 V *into* the screw terminals.
- **No fuse or diode:** a UL-listed USB-C supply has its own short-circuit and overload protection, and with only one power input there is nothing for a diode to isolate.
- **No power switch:** unplug the USB-C cable to turn the device off.
- **No extra parts:** no bulk capacitor, matching Adafruit's builds. If the board ever resets or the panel flickers on bright screens, a 1000 µF (10 V or more) capacitor across the panel's VCC/GND is the first fix to try.
- **Address E jumper:** 64-row panels need the HUB75 E line. Set the MatrixPortal's address-E jumper as described in Adafruit's guide.
- **Pin mapping:** map the GPIO numbers for A0–A4 from the Adafruit pinout before writing firmware. The pot must be on an ADC1 pin (GPIO 1–10 on the S3), because ADC2 conflicts with WiFi.
- **Button board:** if it has a VCC pin, power it from 3V3, never 5 V.

## 6. Firmware

### 6.1 Stack

| Concern | Choice |
|---|---|
| Toolchain | Rust Xtensa toolchain installed with `espup`. Build with `cargo` and flash with `espflash`. *(Not PlatformIO, which doesn't support Rust.)* |
| HAL / runtime | `esp-hal` (`no_std`) + `embassy-executor`, `embassy-time`, `embassy-sync` |
| Display | `esp-hub75`, which uses the S3's LCD_CAM peripheral with DMA. `embedded-graphics` for drawing, `u8g2-fonts` for text, and `tinyqoi` or `tinybmp` for icons stored as image files. |
| WiFi / net | `esp-radio` (formerly `esp-wifi`) + `embassy-net` |
| HTTP(S) | `reqwless` + TLS (see R-2) |
| JSON | `serde` + `serde_json` with `alloc` (PSRAM heap) |
| JPEG | `zune-jpeg` (`no_std` + `alloc`) |
| QR | `qrcodegen-no-heap` |
| Setup portal | `edge-captive` (captive-portal DNS) + `picoserve` (HTTP server) |
| Buttons | `debouncr` |
| Time | `sntpc` for SNTP; `jiff` with its bundled IANA timezone database for local time |
| Weather | Open-Meteo over HTTPS with `reqwless`; same JSON stack as Spotify |
| Settings storage | `sequential-storage` on a dedicated flash partition |
| Heap | `esp-alloc`, backed by PSRAM |

### 6.2 Tasks

```mermaid
flowchart LR
    spotify["spotify_task"] -- "art / status" --> display["display_task"]
    weather["weather_task"] -- "weather" --> display
    input["input_task"] -- "brightness / mode" --> display
    input -- "playback commands" --> spotify
    net["net_task<br/>WiFi connect / reconnect / portal mode"] -. "network up / down" .-> spotify
    net -. "network up / down" .-> weather
```

- **display_task:** owns the HUB75 driver and framebuffers, and handles crossfades, status screens, idle screens (clocks and weather) and the current cap.
- **spotify_task:** token refresh, polling, art download and decode, and playback commands.
- **input_task:** knob sampling and button debouncing. Sends brightness, mode and command events.
- **weather_task:** fetches Open-Meteo data on its schedule (W-2) and publishes the latest reading.
- **net_task:** runs the WiFi state machine (`Connecting → Online → Lost → Portal`). Starts the AP, DNS server and HTTP server in portal mode.
- Tasks talk over `embassy_sync` channels and signals. Shared state is a small `Watch` holding the app state.

**net_task states:**

```mermaid
stateDiagram-v2
    [*] --> Connecting: credentials stored
    [*] --> Portal: no credentials
    Connecting --> Online: joined
    Connecting --> Portal: can't join within 30 s at boot
    Online --> Lost: WiFi drops
    Lost --> Online: rejoined
    Lost --> Lost: retry with backoff (max 60 s)
    Online --> Portal: hold Mode 5 s
    Portal --> [*]: settings saved, reboot
```

### 6.3 Stored settings

| Key | Value |
|---|---|
| `wifi.ssid` / `wifi.pass` | WiFi credentials |
| `spotify.client_id` | From the owner's Spotify developer app |
| `spotify.refresh_token` | Rewritten whenever Spotify rotates it |
| `ui.idle_after_min` | Minutes with nothing playing before the idle screen shows, 1–30 (default 5) |
| `ui.idle_mode` | `clock_digital` / `clock_analog` / `weather` / `clock_weather` / `weather_tides` / `last_song` / `off` |
| `ui.timezone` | IANA timezone name for the clock, e.g. `America/Los_Angeles` |
| `ui.clock_24h` | `true` / `false` |
| `weather.city` | City name as typed in the portal |
| `weather.lat` / `weather.lon` | Coordinates from geocoding |
| `weather.tides` | Whether the city has tide data: `unknown` / `available` / `unavailable` |
| `weather.units` | `c` / `f` |

## 7. Repository layout

```
on-air/
├── README.md
├── Cargo.toml               workspace for the host-buildable crates
├── docs/
│   ├── requirements.md      ← this document
│   ├── shopping-list.md     parts to buy, with links
│   ├── implementation-plan.md  software build plan
│   └── hardware.md          (build guide, photos; later)
├── crates/
│   ├── core/                crate `on-air-core`: no_std logic: Spotify and weather models, state machine, image utils (host-tested)
│   └── ui/                  crate `on-air-ui`: no_std screens drawn with embedded-graphics (shared by simulator and firmware)
├── firmware/                esp32s3 binary crate (esp-hal + embassy), outside the workspace
│   ├── .cargo/config.toml
│   ├── rust-toolchain.toml  channel = "esp"
│   └── src/
└── tools/
    └── sim/                 desktop panel simulator
```

## 8. Milestones

The software side of each milestone is broken down in [implementation-plan.md](implementation-plan.md).

| # | Milestone | Done when |
|---|---|---|
| M0 | **Panel spike** | `esp-hub75` shows a solid color and then a test pattern on the panel, with the correct E line and no flicker. |
| M1 | **Inputs** | The knob changes brightness and the 4 buttons log events. |
| M2 | **Network spike** | Joins WiFi from hard-coded credentials and completes an HTTPS GET to `api.spotify.com` with certificate checks. |
| M3 | **Spotify happy path** | A hard-coded refresh token gets the current track, then downloads, decodes and shows the cover. |
| M4 | **Portal** | Soft AP, DNS capture, form, PKCE paste-back flow and saved settings. |
| M5 | **Idle screens** | Digital and analog clocks, weather fetch and icons, clock + weather layout, mode button cycling. |
| M6 | **Polish** | Crossfade, status screens, backoff, watchdog, playback buttons. |
| M7 | **Enclosure** | Electronics fit the 3D-printed case (designed separately), with the knob mounted, the diffuser fitted and the MatrixPortal's USB-C port reachable from outside. |

## 9. Risks and open questions

| ID | Item | Mitigation |
|---|---|---|
| R-1 | `esp-hub75` may not support this panel's scan pattern, or may need driver-chip setup (some panels use FM6126A-type chips that need an init sequence). | M0 spike comes first. Fallback is a thin Rust wrapper around the C `ESP32-HUB75-MatrixPanel-DMA` library via `esp-idf`, which would mean switching to the `std` stack. |
| R-2 | TLS: `embedded-tls` supports TLS 1.3 only and has limited certificate verification; `esp-mbedtls` is heavier but more complete. | Decided in M2 (2026-11-16): `mbedtls-rs`, the published form of `esp-mbedtls`, verifying certificates against pinned root CAs (`firmware/certs/roots.pem`) with dates checked against SNTP time. TLS 1.2 and 1.3. The device needs `accounts.spotify.com`, `api.spotify.com`, `i.scdn.co`, `api.open-meteo.com` and `geocoding-api.open-meteo.com`; all five pass (`api.open-meteo.com` after a brief outage on its side). Handshakes take about 1–2 s in software. |
| R-3 | Rust captive-portal crates are young. | Use `edge-captive` for the DNS side and `picoserve` for HTTP; only the form page and sign-in flow are written in-repo. |
| R-4 | Spotify developer-mode limits: only allowlisted users, possible Premium requirement for the app owner, and future API policy changes. | Personal use only. Document the app setup in the README. |
| R-5 | Memory: TLS buffers, JSON and JPEG decoding together. | Put the heap in PSRAM, read large responses as streams, and download the 300 px image (about 20 KB, 270 KB decoded) rather than the 640 px one (1.2 MB decoded). |
| R-6 | A readable analog clock and weather layout at 64×64 px. | Mock the screens in the host panel simulator (`tools/`) before M5. |
| Q-1 | Can the MatrixPortal's USB-C input and screw terminals carry about 2.5 A to the panel continuously? Adafruit doesn't publish a rating. | Check the schematic in Adafruit's PCB repo, and check that the board stays cool at the cap during the M0 bench test. Lower the cap if needed. |
| Q-2 | Pinout and pull-ups on the button board. | Check the board before wiring. |
| Q-3 | Measured maximum current draw of the panel. | Bench-measure in M0 and tune the firmware cap (default 2.5 A). |
| Q-4 | During sign-in the phone is on the setup network, which has no internet, so it may not reach Spotify's login page. iOS and Android captive-portal windows may also make copying the callback URL hard. | Test in M4. Options: let the phone fall back to mobile data, have the device bridge traffic while it is on home WiFi, or tell the owner to open the portal in the full browser. |

## 10. Spotify app setup (owner, one time)

1. Create an app at <https://developer.spotify.com/dashboard>.
2. Add the redirect URI `http://127.0.0.1:8888/callback`.
3. Select **Web API**.
4. Copy the **Client ID** into the On-Air setup portal.

## 11. Stretch goals (after v1)

Ideas for after v1 works end to end. None of these are in the v1 milestones.

### 11.1 Larger panel

A bigger display, for example 128×128 pixels, for sharper covers and more room on the clock and info screens.

| ID | Goal | Notes |
|---|---|---|
| SG-1 | **Panel size as a setting:** one value for the panel's width and height, used everywhere instead of the fixed 64. | Today 64 is fixed in a few constants (`ui::WIDTH`/`HEIGHT`, `core::image::SIZE`, `core::spotify::ART_SIZE`) and in about 45 hand-placed screen coordinates. |
| SG-2 | **Screens that lay themselves out:** each screen positions its parts from the panel size, with icon and font sizes chosen to suit it. | Most of the work. Snapshot tests at both 64 and 128. u8g2 has larger pixel fonts. |
| SG-3 | **Larger covers:** at 128 px, download Spotify's 300 px cover and shrink it. | The resize code already handles this. Decoding takes more memory, which is fine with PSRAM. |
| SG-4 | **Simulator at any size:** e.g. `ON_AIR_PANEL=128`, to design and check screens before buying panels. | Comes almost free with SG-1 and SG-2. |
| SG-5 | **Hardware:** a single 128×128 HUB75 panel is rare; usually four 64×64 panels are chained in a 2×2 grid. | 4× the pixels to refresh, so check `esp-hub75` chaining and flicker on the MatrixPortal S3 (as in M0). Up to 4× the current at full white, so a bigger 5 V supply than USB-C and a separate panel feed. Needs a bigger enclosure. |

### 11.2 Live visualizations

Alternatives to the album art while music is playing: a waveform or other visualizations that move with the music.

**Where the audio comes from:** Spotify doesn't provide it. The Web API has never given access to the audio stream, and its audio-analysis and audio-features data (beats, tempo, loudness) were switched off for new developer apps in late 2024. So a real visualization needs a **microphone on the device** that listens to the room.

| ID | Goal | Notes |
|---|---|---|
| SG-6 | **Waveform:** an oscilloscope-style line of the sound, across the panel. | Simplest to draw. Needs the microphone (SG-11). |
| SG-7 | **Spectrum bars:** frequency bands as vertical bars, with slowly falling peak dots. | Needs an FFT; `microfft` (`no_std`) on the ESP32-S3. A 512-point FFT at about 30 frames per second is well within the chip's budget. |
| SG-8 | **Cover-coloured visualizations:** waveform or bars drawn in colours taken from the current cover, so they still feel tied to the song. | Pick 2–4 main colours from the 64×64 cover once per track. |
| SG-9 | **Art + visualizer:** the cover dimmed, with a thin spectrum or VU strip along the bottom edge. | Keeps the cover as the main thing. |
| SG-10 | **Beat pulse:** the cover gently brightens on loud beats. | Peak detection on the microphone level; no FFT needed. |
| SG-11 | **Microphone hardware:** an I2S MEMS microphone (for example the Adafruit SPH0645 or an INMP441 breakout). | Adds a part to the bill of materials, 3 signal pins (clock, word select, data) plus 3V3 and GND, and a small opening in the enclosure. Check which MatrixPortal S3 pins are free (Q-5). |
| SG-12 | **Choosing a live mode:** how the owner picks art, visualizer, or both while music plays. | Options: a portal setting; or Mode cycling live modes while music plays (it currently previews idle screens); or a new hold gesture. Decide when building it. |

**Visualizations without a microphone:** possible from data Spotify does give, but they don't react to the sound:
- a progress bar for the current track
- a slow ambient animation in the cover's colours

These could ship earlier as simpler live modes.

**Risks:**
- **Room noise:** the microphone hears everything in the room, including talking and a dishwasher. Use automatic gain and a noise floor, and accept that it shows the room's sound rather than Spotify's.
- **Placement:** inside the case, the microphone may hear the case more than the room. It needs an opening in the printed enclosure.
- **CPU time:** sampling and the FFT run alongside the display refresh and WiFi. Keep the FFT on its own task and check the panel doesn't flicker (P3).

**Before the hardware:** the simulator could use the Mac's microphone (the `cpal` crate) to try each visualization at 64×64, the same way the screens were designed.

### 11.3 Reporting failures

v1 shows a short notice on the panel when something stops working (No WiFi, Premium needed, and an "Update needed" notice for certificate failures, added in P6). Beyond that, the device could say *what* failed and *why*, so a problem can be fixed without plugging in a laptop to read the serial log.

| ID | Goal | Notes |
|---|---|---|
| SG-13 | **Tell failures apart:** an API that's down or erroring (Spotify or Open-Meteo unreachable, timing out, or answering 5xx), a certificate that's expired or chains to a root we don't pin, a revoked Spotify sign-in, and the network itself being down. | The Spotify and weather loops in core already see each case; they'd report a reason rather than just "offline". mbedtls says why a certificate failed (expired, untrusted, wrong host name). |
| SG-14 | **A details screen:** a button hold on the notice shows the failing host, the reason and how long it's been failing. | Small text at 64×64, scrolling like the track info screen. |
| SG-15 | **Certificate expiry warning:** warn ahead of time when a pinned root is close to expiring. | The roots in `firmware/certs/` run to 2035–2040, so this matters more if a host moves to a root with a shorter life. The expiry dates can be read from the PEMs at build time. |
| SG-16 | **A failure log:** the last few failures, with times, kept across restarts. | A small ring buffer in the settings partition, written rarely to spare the flash. |
| SG-17 | **Notifications:** optionally push a message to the owner's phone when a failure lasts more than a while. | For example an `ntfy.sh` topic or a webhook, set in the portal. Off by default; it sends data off the device, so the portal should say so. |

The status page can't simply be served on the home network, since N-5 keeps the portal on the setup AP only. Reporting goes through the panel, the log and, if enabled, notifications.

| ID | Open question |
|---|---|
| Q-5 | Which MatrixPortal S3 pins are free for an I2S microphone once the knob and buttons use A0–A4? Check the Adafruit pinout. |
| Q-6 | For a 2×2 chain of 64×64 panels, can the MatrixPortal S3 refresh 128×128 without flicker, and how much current does it draw at the brightness we'd use? |
