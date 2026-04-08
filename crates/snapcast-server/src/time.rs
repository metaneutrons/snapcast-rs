//! Time utilities — server timestamp generation using monotonic clock.
//!
//! Must use the same clock as the client (`mach_continuous_time` on macOS,
//! `CLOCK_MONOTONIC` on Linux) for time sync to work.

/// Current monotonic time in microseconds since boot.
#[allow(unsafe_code)]
pub fn now_usec() -> i64 {
    #[cfg(target_os = "macos")]
    {
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
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
        }
        ts.tv_sec as i64 * 1_000_000 + ts.tv_nsec as i64 / 1_000
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
