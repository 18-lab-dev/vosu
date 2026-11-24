//! Console lifecycle without changing the executable's console subsystem.
use std::io;
use windows_sys::Win32::System::{
    Console::{FreeConsole, GetConsoleProcessList},
    LibraryLoader::{GetModuleHandleA, GetProcAddress},
};

/// Older Windows versions create a private console on graphical launches.
/// Never detach an inherited console: its shell owns waiting and Ctrl-C.
pub fn detach_private_console() {
    let mut processes = [0u32; 2];
    // SAFETY: the array is writable for the supplied element count.
    let count = unsafe { GetConsoleProcessList(processes.as_mut_ptr(), processes.len() as u32) };
    if count == 1 {
        // SAFETY: detaching the calling process requires no borrowed resources.
        unsafe { FreeConsole() };
    }
}

#[repr(C)]
struct AllocConsoleOptions {
    mode: i32,
    use_show_window: i32,
    show_window: u16,
}

type AllocConsoleWithOptions =
    unsafe extern "system" fn(*const AllocConsoleOptions, *mut i32) -> i32;

/// Explicitly requests a visible console, overriding the detached manifest.
/// On older Windows, the tray's CREATE_NEW_CONSOLE already supplied it.
pub fn open_log_console() -> io::Result<()> {
    // SAFETY: kernel32 is loaded for the process lifetime; name is NUL terminated.
    let module = unsafe { GetModuleHandleA(c"kernel32.dll".as_ptr().cast()) };
    if module.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: module is live and the export name is NUL terminated.
    let Some(address) =
        (unsafe { GetProcAddress(module, c"AllocConsoleWithOptions".as_ptr().cast()) })
    else {
        return Ok(());
    };
    // SAFETY: the named Windows export has the documented ABI and signature.
    let allocate: AllocConsoleWithOptions = unsafe { std::mem::transmute(address) };
    let options = AllocConsoleOptions {
        mode: 1,
        use_show_window: 0,
        show_window: 0,
    };
    // SAFETY: options has the documented C layout; optional output is null.
    let result = unsafe { allocate(&options, std::ptr::null_mut()) };
    if result < 0 {
        return Err(io::Error::other(format!(
            "AllocConsoleWithOptions failed: {result:#x}"
        )));
    }
    Ok(())
}
