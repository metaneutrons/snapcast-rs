//! Time utilities — server timestamp generation.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current wall-clock time in microseconds since Unix epoch.
pub fn now_usec() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as i64
}
