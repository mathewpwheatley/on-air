# On-Air task runner. Run `just` to list recipes.

set shell := ["bash", "-euo", "pipefail", "-c"]

# Firmware recipes need the Xtensa toolchain environment from espup.
fw := "cd firmware && . \"$HOME/export-esp.sh\" &&"

_default:
    @just --list

# Run host tests for the shared crates and simulator
test:
    cargo test --workspace

# Open the desktop panel simulator. Pass options after, e.g. `just sim --live --city Portland`
sim *ARGS:
    cargo run -p on-air-sim -- {{ARGS}}

# Re-render the UI snapshot images after an intended change
snapshots:
    UPDATE_SNAPSHOTS=1 cargo test -p on-air-ui --test snapshots

# Format all code
fmt:
    cargo fmt --all
    {{fw}} cargo fmt

# Check formatting and run clippy (warnings are errors) on host and firmware
lint:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    {{fw}} cargo fmt --check
    {{fw}} cargo clippy --release -- -D warnings

# Build the firmware image
build:
    {{fw}} cargo build --release

# Build, flash and open the serial monitor
flash:
    {{fw}} cargo run --release

# Open the serial monitor without flashing
monitor:
    espflash monitor --chip esp32s3
