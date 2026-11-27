//! Native CPU times in the process that actually runs the reader.
#[cfg(target_os = "linux")]
fn cpu(clock: libc::clockid_t) -> u64 {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: the selected CPU clock is valid and time points to writable storage.
    assert_eq!(unsafe { libc::clock_gettime(clock, time.as_mut_ptr()) }, 0);
    // SAFETY: a successful clock_gettime initializes the output.
    let time = unsafe { time.assume_init() };
    time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
}

#[cfg(windows)]
fn cpu(thread: bool) -> u64 {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentProcess, GetCurrentThread, GetProcessTimes, GetThreadTimes},
    };
    let [mut created, mut exited, mut kernel, mut user] = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    // SAFETY: the pseudo handles identify this thread/process and the four outputs are valid.
    let result = unsafe {
        if thread {
            GetThreadTimes(
                GetCurrentThread(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        } else {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        }
    };
    assert_ne!(result, 0, "{}", std::io::Error::last_os_error());
    let count = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    (count(kernel) + count(user)) * 100
}

pub fn thread_cpu() -> (u64, u64) {
    #[cfg(target_os = "linux")]
    return (cpu(libc::CLOCK_THREAD_CPUTIME_ID), 0);
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::{
            Threading::GetCurrentThread, WindowsProgramming::QueryThreadCycleTime,
        };
        let mut cycles = 0;
        // SAFETY: the pseudo handle is valid and cycles is writable.
        let success = unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) };
        assert_ne!(success, 0);
        (cpu(true), cycles)
    }
}

pub fn process_cpu_ns() -> u64 {
    #[cfg(target_os = "linux")]
    return cpu(libc::CLOCK_PROCESS_CPUTIME_ID);
    #[cfg(windows)]
    return cpu(false);
}
