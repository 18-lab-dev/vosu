//! Coarse monotonic time for repeated execution-budget checks, not capture timestamps.
use super::MemoryError;

/// Milliseconds since an arbitrary monotonic origin. Resolution is typically
/// about 16 ms on Windows and 1–4 ms on Linux; callers must use multi-tick budgets.
#[cfg(windows)]
pub fn coarse_monotonic_millis() -> Result<u64, MemoryError> {
    // SAFETY: GetTickCount64 takes no pointers and reads the system uptime clock.
    Ok(unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() })
}

#[cfg(target_os = "linux")]
pub fn coarse_monotonic_millis() -> Result<u64, MemoryError> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: time is writable storage of the required size. The coarse clock
    // is monotonic and uses the vDSO where the kernel provides it.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC_COARSE, &mut time) } != 0 {
        return Err(MemoryError::Backend {
            operation: "clock_gettime(CLOCK_MONOTONIC_COARSE)",
            message: std::io::Error::last_os_error().to_string(),
        });
    }
    let seconds = u64::try_from(time.tv_sec).map_err(|_| MemoryError::Backend {
        operation: "clock_gettime(CLOCK_MONOTONIC_COARSE)",
        message: "negative monotonic time".into(),
    })?;
    Ok(seconds
        .saturating_mul(1_000)
        .saturating_add(u64::try_from(time.tv_nsec).unwrap_or(0) / 1_000_000))
}

#[cfg(test)]
mod tests {
    #[test]
    fn coarse_clock_is_monotonic() {
        let before = super::coarse_monotonic_millis().unwrap();
        let after = super::coarse_monotonic_millis().unwrap();
        assert!(after >= before);
    }
}
