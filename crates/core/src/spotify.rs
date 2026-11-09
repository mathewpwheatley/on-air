//! Spotify Web API: the currently-playing response and playback
//! commands.

use alloc::string::String;
use alloc::vec::Vec;
use serde::Deserialize;

/// `market=from_token` drops the long `available_markets` lists, which keeps
/// responses small. `additional_types=episode` includes podcasts.
pub const CURRENTLY_PLAYING_URL: &str = "https://api.spotify.com/v1/me/player/currently-playing?market=from_token&additional_types=episode";
/// Preferred cover size.
pub const ART_SIZE: u32 = 64;
/// Used when a 429 has no usable `Retry-After`.
pub const DEFAULT_RETRY_AFTER_S: u32 = 30;

/// What's playing, trimmed to what the panel needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NowPlaying {
    /// Stable identity for the item, used to spot track changes.
    pub item_id: String,
    pub title: String,
    /// Artists joined with ", ", or the show name for a podcast.
    pub artist: String,
    /// Album name. `None` for podcasts.
    pub album: Option<String>,
    pub is_playing: bool,
    pub progress_ms: u32,
    pub duration_ms: u32,
    /// The cover to download. Re-download only when this changes.
    pub art_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerResponse {
    Playing(NowPlaying),
    /// `204`, or an item Spotify won't describe (ads, private sessions).
    Nothing,
    /// `401`: refresh the token and retry once.
    Unauthorized,
    /// `429`.
    RateLimited {
        retry_after_s: u32,
    },
    /// Anything else, including a 200 with a body we couldn't read.
    Failed {
        status: u16,
    },
}

#[derive(Deserialize)]
struct RawCurrentlyPlaying {
    #[serde(default)]
    progress_ms: Option<u32>,
    #[serde(default)]
    is_playing: bool,
    #[serde(default)]
    item: Option<RawItem>,
}

#[derive(Deserialize)]
struct RawItem {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    uri: Option<String>,
    name: String,
    duration_ms: u32,
    #[serde(default)]
    album: Option<RawAlbum>,
    #[serde(default)]
    artists: Vec<RawArtist>,
    // Podcast episodes carry images and a show instead of album and artists.
    #[serde(default)]
    images: Vec<RawImage>,
    #[serde(default)]
    show: Option<RawShow>,
}

#[derive(Deserialize)]
struct RawAlbum {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    images: Vec<RawImage>,
}

#[derive(Deserialize)]
struct RawArtist {
    name: String,
}

#[derive(Deserialize)]
struct RawShow {
    name: String,
    #[serde(default)]
    images: Vec<RawImage>,
}

#[derive(Deserialize)]
struct RawImage {
    url: String,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

/// Read a `GET /me/player/currently-playing` reply.
pub fn parse_player_response(
    status: u16,
    retry_after: Option<&str>,
    body: &[u8],
) -> PlayerResponse {
    match status {
        200 => match serde_json::from_slice::<RawCurrentlyPlaying>(body) {
            Ok(raw) => match raw.item {
                Some(item) => {
                    PlayerResponse::Playing(now_playing(item, raw.is_playing, raw.progress_ms))
                }
                None => PlayerResponse::Nothing,
            },
            Err(_) => PlayerResponse::Failed { status },
        },
        204 => PlayerResponse::Nothing,
        401 => PlayerResponse::Unauthorized,
        429 => PlayerResponse::RateLimited {
            retry_after_s: parse_retry_after(retry_after),
        },
        _ => PlayerResponse::Failed { status },
    }
}

fn now_playing(item: RawItem, is_playing: bool, progress_ms: Option<u32>) -> NowPlaying {
    let artist = match &item.show {
        Some(show) => show.name.clone(),
        None => join_names(&item.artists),
    };
    let art_url = pick_image(&item.images)
        .or_else(|| item.album.as_ref().and_then(|a| pick_image(&a.images)))
        .or_else(|| item.show.as_ref().and_then(|s| pick_image(&s.images)))
        .map(String::from);
    let item_id = item.id.or(item.uri).unwrap_or_else(|| item.name.clone());
    let album = item
        .album
        .and_then(|a| a.name)
        .filter(|name| !name.is_empty());
    NowPlaying {
        item_id,
        title: item.name,
        artist,
        album,
        is_playing,
        progress_ms: progress_ms.unwrap_or(0),
        duration_ms: item.duration_ms,
        art_url,
    }
}

fn join_names(artists: &[RawArtist]) -> String {
    let mut out = String::new();
    for (i, a) in artists.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&a.name);
    }
    out
}

/// Pick the cover to download: exactly 64 px if offered, else the
/// smallest that's at least 64 px, else the largest available.
fn pick_image(images: &[RawImage]) -> Option<&str> {
    let size = |img: &RawImage| img.width.zip(img.height).map(|(w, h)| w.min(h));
    if let Some(img) = images.iter().find(|i| size(i) == Some(ART_SIZE)) {
        return Some(&img.url);
    }
    let smallest_big_enough = images
        .iter()
        .filter_map(|i| size(i).filter(|&s| s >= ART_SIZE).map(|s| (s, i)))
        .min_by_key(|(s, _)| *s);
    if let Some((_, img)) = smallest_big_enough {
        return Some(&img.url);
    }
    images
        .iter()
        .max_by_key(|i| size(i).unwrap_or(0))
        .map(|i| i.url.as_str())
}

/// Read a `Retry-After` header in seconds, clamped to 1 s–1 h.
pub fn parse_retry_after(header: Option<&str>) -> u32 {
    header
        .and_then(|h| h.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_RETRY_AFTER_S)
        .clamp(1, 3600)
}

/// Playback buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Play,
    Pause,
    Next,
    Previous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Put,
    Post,
}

impl Command {
    /// HTTP method and URL. The request body is empty.
    pub fn request(self) -> (Method, &'static str) {
        match self {
            Command::Play => (Method::Put, "https://api.spotify.com/v1/me/player/play"),
            Command::Pause => (Method::Put, "https://api.spotify.com/v1/me/player/pause"),
            Command::Next => (Method::Post, "https://api.spotify.com/v1/me/player/next"),
            Command::Previous => (
                Method::Post,
                "https://api.spotify.com/v1/me/player/previous",
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandResult {
    Ok,
    /// Refresh the token and retry once.
    Unauthorized,
    /// `403`: playback control needs Spotify Premium.
    PremiumRequired,
    /// `404`: no device is active, so there's nothing to control.
    NoActiveDevice,
    RateLimited {
        retry_after_s: u32,
    },
    Failed {
        status: u16,
    },
}

/// Read the reply to a playback command.
pub fn parse_command_response(status: u16, retry_after: Option<&str>) -> CommandResult {
    match status {
        200..=204 => CommandResult::Ok,
        401 => CommandResult::Unauthorized,
        403 => CommandResult::PremiumRequired,
        404 => CommandResult::NoActiveDevice,
        429 => CommandResult::RateLimited {
            retry_after_s: parse_retry_after(retry_after),
        },
        _ => CommandResult::Failed { status },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing(body: &[u8]) -> NowPlaying {
        match parse_player_response(200, None, body) {
            PlayerResponse::Playing(np) => np,
            other => panic!("expected Playing, got {other:?}"),
        }
    }

    #[test]
    fn track_playing_picks_64px_cover() {
        let np = playing(include_bytes!(
            "../tests/fixtures/spotify_track_playing.json"
        ));
        assert_eq!(
            np,
            NowPlaying {
                item_id: "3n3Ppam7vgaVa1iaRUc9Lp".into(),
                title: "Mr. Brightside".into(),
                artist: "The Killers".into(),
                album: Some("Hot Fuss".into()),
                is_playing: true,
                progress_ms: 61_234,
                duration_ms: 222_973,
                art_url: Some("https://i.scdn.co/image/ab67616d00004851aaaa".into()),
            }
        );
    }

    #[test]
    fn paused_track_without_64px_picks_smallest_big_enough() {
        let np = playing(include_bytes!(
            "../tests/fixtures/spotify_track_paused_two_artists.json"
        ));
        assert!(!np.is_playing);
        assert_eq!(np.artist, "Queen, David Bowie");
        assert_eq!(np.art_url.as_deref(), Some("https://i.scdn.co/image/mid"));
    }

    #[test]
    fn episode_uses_episode_image_and_show_name() {
        let np = playing(include_bytes!(
            "../tests/fixtures/spotify_episode_playing.json"
        ));
        assert_eq!(np.title, "Episode 42");
        assert_eq!(np.artist, "A Podcast");
        assert_eq!(np.album, None);
        assert_eq!(np.art_url.as_deref(), Some("https://i.scdn.co/image/ep64"));
    }

    #[test]
    fn local_file_has_no_art_and_falls_back_to_uri() {
        let np = playing(include_bytes!("../tests/fixtures/spotify_local_file.json"));
        assert_eq!(np.item_id, "spotify:local:Artist:Album:Song:180");
        assert_eq!(np.art_url, None);
    }

    #[test]
    fn ad_without_item_is_nothing() {
        let body = include_bytes!("../tests/fixtures/spotify_ad_no_item.json");
        assert_eq!(
            parse_player_response(200, None, body),
            PlayerResponse::Nothing
        );
    }

    #[test]
    fn status_codes() {
        assert_eq!(
            parse_player_response(204, None, b""),
            PlayerResponse::Nothing
        );
        assert_eq!(
            parse_player_response(401, None, b""),
            PlayerResponse::Unauthorized
        );
        assert_eq!(
            parse_player_response(429, Some(" 12 "), b""),
            PlayerResponse::RateLimited { retry_after_s: 12 }
        );
        assert_eq!(
            parse_player_response(429, None, b""),
            PlayerResponse::RateLimited { retry_after_s: 30 }
        );
        assert_eq!(
            parse_player_response(502, None, b""),
            PlayerResponse::Failed { status: 502 }
        );
        assert_eq!(
            parse_player_response(200, None, b"{oops"),
            PlayerResponse::Failed { status: 200 }
        );
    }

    #[test]
    fn retry_after_is_clamped() {
        assert_eq!(parse_retry_after(Some("0")), 1);
        assert_eq!(parse_retry_after(Some("99999")), 3600);
        assert_eq!(parse_retry_after(Some("soon")), 30);
    }

    #[test]
    fn commands() {
        assert_eq!(
            Command::Pause.request(),
            (Method::Put, "https://api.spotify.com/v1/me/player/pause")
        );
        assert_eq!(Command::Next.request().0, Method::Post);
        assert_eq!(parse_command_response(204, None), CommandResult::Ok);
        assert_eq!(
            parse_command_response(403, None),
            CommandResult::PremiumRequired
        );
        assert_eq!(
            parse_command_response(404, None),
            CommandResult::NoActiveDevice
        );
        assert_eq!(
            parse_command_response(401, None),
            CommandResult::Unauthorized
        );
        assert_eq!(
            parse_command_response(429, Some("5")),
            CommandResult::RateLimited { retry_after_s: 5 }
        );
    }
}
