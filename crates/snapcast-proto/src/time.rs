//! Monotonic clock — the single source of truth for time synchronization.
//!
//! Both ends of a Snapcast connection MUST sample the *same* clock domain or
//! time sync silently produces wrong offsets. This module is that shared
//! source: `snapcast-client` and `snapcast-server` both call [`now_usec`]
//! rather than each carrying their own copy of the platform FFI (which could
//! drift out of sync with no compile error).
//!
//! Matches C++ `std::chrono::steady_clock`:
//! - macOS: `mach_continuous_time` (note: differs from `CLOCK_MONOTONIC` by
//!   ~2 s, so the choice matters), scaled by `mach_timebase_info`.
//! - Linux / other Unix: `CLOCK_MONOTONIC` via `clock_gettime`.
//! - Non-Unix: falls back to wall-clock `SystemTime` (best effort).

/// Current monotonic time in microseconds.
///
/// Equivalent to the C++ `chronos::steadytimeofday` — microseconds on a
/// monotonic timeline (since boot on Unix). Use this for any cross-endpoint
/// timestamp so client and server agree on the clock.
pub fn now_usec() -> i64 {
    monotonic_usec()
}

/// Monotonic microsecond clock.
#[allow(unsafe_code)] // FFI: mach_continuous_time (macOS), clock_gettime (Linux)
fn monotonic_usec() -> i64 {
    #[cfg(target_os = "macos")]
    {
        // macOS: C++ steady_clock uses mach_continuous_time, not CLOCK_MONOTONIC.
        // These differ by ~2s on macOS; we must match the peer's clock exactly.
        unsafe extern "C" {
            fn mach_continuous_time() -> u64;
            fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
        }
        #[repr(C)]
        struct MachTimebaseInfo {
            numer: u32,
            denom: u32,
        }
        static TIMEBASE: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
        let (numer, denom) = *TIMEBASE.get_or_init(|| {
            let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
            unsafe {
                mach_timebase_info(&mut info);
            }
            (info.numer, info.denom)
        });
        let ticks = unsafe { mach_continuous_time() };
        let nanos = ticks as i128 * numer as i128 / denom as i128;
        (nanos / 1_000) as i64
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock_gettime with CLOCK_MONOTONIC and a valid timespec pointer is sound.
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
        }
        ts.tv_sec * 1_000_000 + ts.tv_nsec / 1_000
    }
    #[cfg(not(unix))]
    {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_usec_is_positive_and_monotonic() {
        let a = now_usec();
        let b = now_usec();
        assert!(a > 0, "monotonic clock should be positive: {a}");
        assert!(b >= a, "monotonic clock must not go backwards: {a} -> {b}");
    }
}
