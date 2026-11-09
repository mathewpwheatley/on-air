//! Hardware-independent On Air logic.
//!
//! Everything here builds for both the host (for tests and the simulator) and
//! the ESP32-S3. Keep it free of hardware and network code.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
