# Spotify sign-in relay

Spotify only redirects to `https://` or loopback addresses after sign-in, and
On-Air serves plain `http://` on your home network. This one static page
bridges the gap: Spotify redirects here, and the page sends your browser on
to On-Air's `/spotify/callback` with the sign-in code. The page stores
nothing. The code is single-use and only works with the PKCE verifier,
which never leaves On-Air.

## Publish it with GitHub Pages

The workflow `.github/workflows/spotify-relay.yml` publishes just this page
from the On-Air repository:

1. Push the repository to GitHub as a **public** repository, for example
   `on-air`.
2. In the repository's **Settings → Pages**, set **Source** to
   *GitHub Actions*.
3. Run the workflow: **Actions → Publish the Spotify sign-in relay → Run
   workflow** (it also runs on any push that changes this page). The page is
   then at `https://<your-github-name>.github.io/on-air/`.
4. In your Spotify app on
   [developer.spotify.com/dashboard](https://developer.spotify.com/dashboard),
   add that address as a **Redirect URI**, exactly as shown (with the
   trailing `/`). Keep `http://127.0.0.1:8888/callback` too: the simulator
   uses it.
5. Put the same address in On-Air's `.env` as `ON_AIR_SPOTIFY_RELAY` and
   flash.

Without `ON_AIR_SPOTIFY_RELAY`, sign-in falls back to pasting the callback
address by hand.
