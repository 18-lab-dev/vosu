//! Scoped Windows multimedia-timer resolution request for active state workers.

use windows_sys::Win32::Media::{timeBeginPeriod, timeEndPeriod};
const REQUESTED_RESOLUTION_MS: u32 = 1;

pub(super) struct TimerResolution {
    acquired: bool,
}

impl TimerResolution {
    pub(super) fn request() -> Self {
        // SAFETY: These WinMM calls take a scalar resolution and have no pointer
        // arguments. The successful begin request is paired in Drop.
        let acquired = unsafe { timeBeginPeriod(REQUESTED_RESOLUTION_MS) == 0 };
        Self { acquired }
    }
}

impl Drop for TimerResolution {
    fn drop(&mut self) {
        if self.acquired {
            // SAFETY: A successful request was made with this exact resolution.
            let _ = unsafe { timeEndPeriod(REQUESTED_RESOLUTION_MS) };
        }
    }
}
