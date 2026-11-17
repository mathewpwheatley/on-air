# Pinned root certificates

`roots.pem` holds the only certificate authorities the firmware trusts for
HTTPS. Each one is the root that a host the device talks to chains to:

| Root | Hosts | Expires |
|---|---|---|
| Starfield Root Certificate Authority - G2 | `accounts.spotify.com` | 2037-12-31 |
| DigiCert Global Root G2 | `api.spotify.com` | 2038-01-15 |
| DigiCert Global Root G3 | `i.scdn.co` (cover art) | 2038-01-15 |
| ISRG Root X1 | `api.open-meteo.com`, `geocoding-api.open-meteo.com` | 2035-06-04 |
| ISRG Root X2 | Let's Encrypt's newer root, in case Open-Meteo moves to it | 2040-09-17 |

To see which root a host uses now:

```sh
echo | openssl s_client -connect api.spotify.com:443 -servername api.spotify.com -showcerts | grep -E "s:|i:"
```

The certificates came from macOS's system roots, for example:

```sh
security find-certificate -c "DigiCert Global Root G2" -p /System/Library/Keychains/SystemRootCertificates.keychain
```

If a host changes to a root that isn't listed here, its requests fail with a
certificate error until the root is added and the firmware rebuilt.
