fn main() {
    forward_env();
    linker_be_nice();
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                what if what.starts_with("_defmt_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`"
                    );
                    eprintln!();
                }
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `linkall.x` missing?");
                    eprintln!();
                }
                what if what.starts_with("esp_rtos_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `esp-radio` has no scheduler enabled. Make sure you have initialized `esp-rtos` or provided an external scheduler."
                    );
                    eprintln!();
                }
                "embedded_test_linker_file_not_added_to_rustflags" => {
                    eprintln!();
                    eprintln!(
                        "💡 `embedded-test` not found - make sure `embedded-test.x` is added as a linker script for tests"
                    );
                    eprintln!();
                }
                "free"
                | "malloc"
                | "calloc"
                | "get_free_internal_heap_size"
                | "malloc_internal"
                | "realloc_internal"
                | "calloc_internal"
                | "free_internal" => {
                    eprintln!();
                    eprintln!(
                        "💡 Did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
                    );
                    eprintln!();
                }
                _ => (),
            },
            // we don't have anything helpful for "missing-lib" yet
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=-Wl,--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}

/// Pass the WiFi name and password from the repo's `.env` to the build, for
/// `option_env!`. The environment wins over the file.
fn forward_env() {
    const KEYS: [&str; 4] = [
        "ON_AIR_WIFI_SSID",
        "ON_AIR_WIFI_PASSWORD",
        "SPOTIFY_CLIENT_ID",
        "ON_AIR_SPOTIFY_REFRESH_TOKEN",
    ];
    let path = std::path::Path::new("../.env");
    println!("cargo:rerun-if-changed={}", path.display());
    for key in KEYS {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let from_file: Vec<(String, String)> = dotenvy::from_path_iter(path)
        .map(|entries| entries.filter_map(Result::ok).collect())
        .unwrap_or_default();
    for key in KEYS {
        let value = std::env::var(key).ok().or_else(|| {
            from_file
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        });
        let value = value.filter(|v| !v.is_empty()).or_else(|| match key {
            "ON_AIR_SPOTIFY_REFRESH_TOKEN" => simulator_refresh_token(&from_file),
            _ => None,
        });
        if let Some(value) = value {
            println!("cargo:rustc-env={key}={value}");
        }
    }
}

/// The refresh token `just sim --live` saved when you signed in, if it was
/// for the same Spotify app. macOS only, where the simulator keeps it.
fn simulator_refresh_token(from_file: &[(String, String)]) -> Option<String> {
    let client_id = std::env::var("SPOTIFY_CLIENT_ID").ok().or_else(|| {
        from_file
            .iter()
            .find(|(k, _)| k == "SPOTIFY_CLIENT_ID")
            .map(|(_, v)| v.clone())
    })?;
    let path = std::path::Path::new(&std::env::var("HOME").ok()?)
        .join("Library/Application Support/on-air-sim/session.json");
    println!("cargo:rerun-if-changed={}", path.display());
    let session: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (session["client_id"].as_str()? == client_id)
        .then(|| session["refresh_token"].as_str().map(String::from))
        .flatten()
}
