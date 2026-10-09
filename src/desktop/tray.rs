//! Optional desktop affordance; failures never prevent the daemon from serving.
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

/// Called on each activation, so listener reloads do not leave a stale URL.
pub type DashboardUrl = Arc<dyn Fn() -> String + Send + Sync>;

#[derive(Clone)]
struct Actions {
    open: mpsc::Sender<()>,
    quit: CancellationToken,
}

impl Actions {
    fn open(&self) {
        // Coalesce repeated clicks while a browser launch is pending.
        let _ = self.open.try_send(());
    }

    fn quit(&self) {
        self.quit.cancel();
    }
}

/// Owned by the ordinary daemon role. Drop requests teardown without blocking.
pub struct TrayHandle {
    stop: CancellationToken,
    service: Option<JoinHandle<()>>,
    opener: JoinHandle<()>,
}

impl TrayHandle {
    /// Best-effort removal, with a hard bound even if the desktop is unresponsive.
    pub async fn shutdown(&mut self) {
        self.stop.cancel();
        self.opener.abort();
        if let Some(mut task) = self.service.take()
            && tokio::time::timeout(Duration::from_secs(1), &mut task)
                .await
                .is_err()
        {
            task.abort();
        }
    }
}

impl Drop for TrayHandle {
    fn drop(&mut self) {
        self.stop.cancel();
        self.opener.abort();
        // Do not abort the service here: it owns the icon and must remove it.
        // Its cancellation path is independently bounded on Linux.
    }
}

/// Start inside a Tokio runtime, after the listener is ready. No desktop is required.
pub fn start(dashboard_url: DashboardUrl, shutdown: CancellationToken) -> TrayHandle {
    let stop = CancellationToken::new();
    let (open, mut requests) = mpsc::channel(1);
    let opener_stop = stop.clone();
    let opener = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = opener_stop.cancelled() => break,
                request = requests.recv() => {
                    if request.is_none() { break; }
                    let url = dashboard_url();
                    // OS launch may block; never run it on a runtime/UI thread.
                    let _ = tokio::task::spawn_blocking(move || {
                        if let Err(error) = open::that_detached(url) {
                            tracing::debug!(%error, "Tray dashboard launch unavailable");
                        }
                    }).await;
                }
            }
        }
    });
    let actions = Actions {
        open,
        quit: shutdown,
    };
    let service = platform::start(actions, stop.clone());
    TrayHandle {
        stop,
        service: Some(service),
        opener,
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use ksni::{Tray, TrayMethods};
    use std::io::Cursor;

    struct LinuxTray {
        actions: Actions,
        icon: Vec<ksni::Icon>,
    }

    impl Tray for LinuxTray {
        fn id(&self) -> String {
            "vosu".into()
        }
        fn title(&self) -> String {
            "vosu".into()
        }
        fn icon_name(&self) -> String {
            "vosu".into()
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            self.icon.clone()
        }
        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                icon_name: "vosu".into(),
                title: "vosu".into(),
                ..Default::default()
            }
        }
        fn activate(&mut self, _: i32, _: i32) {
            self.actions.open();
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            vec![
                ksni::menu::StandardItem {
                    label: "Open dashboard".into(),
                    activate: Box::new(|tray: &mut Self| tray.actions.open()),
                    ..Default::default()
                }
                .into(),
                ksni::menu::StandardItem {
                    label: "Quit".into(),
                    activate: Box::new(|tray: &mut Self| tray.actions.quit()),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    pub(super) fn start(actions: Actions, stop: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(async move {
            let icon = match embedded_icon() {
                Ok(icon) => vec![icon],
                Err(error) => {
                    tracing::debug!(%error, "Tray pixmap unavailable; using theme icon");
                    Vec::new()
                }
            };
            let tray = LinuxTray { actions, icon };
            let result = tokio::select! {
                biased;
                _ = stop.cancelled() => return,
                result = tray.spawn() => result,
            };
            match result {
                Ok(handle) => {
                    stop.cancelled().await;
                    let _ =
                        tokio::time::timeout(Duration::from_millis(900), handle.shutdown()).await;
                }
                Err(error) => tracing::debug!(%error, "Desktop tray unavailable"),
            }
        })
    }

    fn embedded_icon() -> Result<ksni::Icon, Box<dyn std::error::Error>> {
        let mut decoder = png::Decoder::new(Cursor::new(include_bytes!(
            "../../assets/branding/vosu-app-icon.png"
        )));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info()?;
        let mut bytes = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
        let info = reader.next_frame(&mut bytes)?;
        let channels = match info.color_type {
            png::ColorType::Rgba => 4,
            png::ColorType::Rgb => 3,
            _ => return Err("Expected RGB branding icon".into()),
        };
        let data = downscale_argb(
            &bytes[..info.buffer_size()],
            info.width as usize,
            info.height as usize,
            channels,
        );
        Ok(ksni::Icon {
            width: 64,
            height: 64,
            data,
        })
    }

    // StatusNotifierItem uses network byte order ARGB, not native-endian u32s.
    fn downscale_argb(bytes: &[u8], width: usize, height: usize, channels: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(64 * 64 * 4);
        for y in 0..64 {
            for x in 0..64 {
                let x0 = x * width / 64;
                let y0 = y * height / 64;
                let x1 = ((x + 1) * width / 64).max(x0 + 1).min(width);
                let y1 = ((y + 1) * height / 64).max(y0 + 1).min(height);
                let mut sums = [0_u64; 4];
                let mut count = 0;
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let pixel = &bytes[(sy * width + sx) * channels..];
                        sums[0] += if channels == 4 {
                            u64::from(pixel[3])
                        } else {
                            255
                        };
                        for c in 0..3 {
                            sums[c + 1] += u64::from(pixel[c]);
                        }
                        count += 1;
                    }
                }
                data.extend(sums.map(|sum| (sum / count) as u8));
            }
        }
        data
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn embedded_pixmap_has_sni_shape() {
            let icon = embedded_icon().unwrap();
            assert_eq!(
                (icon.width, icon.height, icon.data.len()),
                (64, 64, 64 * 64 * 4)
            );
        }
        #[test]
        fn rgba_is_network_order_argb() {
            assert_eq!(
                &downscale_argb(&[10, 20, 30, 40], 1, 1, 4)[..4],
                &[40, 10, 20, 30]
            );
        }
        #[test]
        fn menu_actions_are_bounded_and_cancel() {
            let (open, mut receiver) = mpsc::channel(1);
            let quit = CancellationToken::new();
            let mut tray = LinuxTray {
                actions: Actions {
                    open,
                    quit: quit.clone(),
                },
                icon: vec![],
            };
            let menu = tray.menu();
            if let ksni::MenuItem::Standard(item) = &menu[0] {
                (item.activate)(&mut tray);
                (item.activate)(&mut tray);
            } else {
                panic!("Expected Open dashboard");
            }
            assert!(receiver.try_recv().is_ok());
            assert!(receiver.try_recv().is_err());
            if let ksni::MenuItem::Standard(item) = &menu[1] {
                (item.activate)(&mut tray);
            }
            assert!(quit.is_cancelled());
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{
        ffi::c_void,
        mem::size_of,
        ptr::{null, null_mut},
    };
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{Shell::*, WindowsAndMessaging::*},
    };

    const CALLBACK: u32 = WM_APP + 1;
    const OPEN: usize = 1;
    const QUIT: usize = 2;
    struct State {
        actions: Actions,
        stop: CancellationToken,
        taskbar_created: u32,
        icon: NOTIFYICONDATAW,
    }

    pub(super) fn start(actions: Actions, stop: CancellationToken) -> JoinHandle<()> {
        let (done, finished) = tokio::sync::oneshot::channel();
        let result = std::thread::Builder::new()
            .name("vosu-tray".into())
            .spawn(move || {
                // All HWND/HICON/menu ownership remains on this dedicated thread.
                unsafe {
                    run(actions, stop);
                }
                let _ = done.send(());
            });
        if let Err(error) = result {
            tracing::debug!(%error, "Desktop tray thread unavailable");
        }
        tokio::spawn(async move {
            let _ = finished.await;
        })
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    unsafe fn run(actions: Actions, stop: CancellationToken) {
        // SAFETY: Win32 handles are used only on their owning thread; window state
        // outlives the window and is reclaimed after DestroyWindow returns.
        unsafe {
            let instance = GetModuleHandleW(null());
            let class_name = wide("vosu-tray");
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class_name.as_ptr(),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return;
            }
            let mut state = Box::new(State {
                actions,
                stop,
                taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
                icon: NOTIFYICONDATAW::default(),
            });
            // An invisible top-level window (not HWND_MESSAGE) receives Explorer restart broadcasts.
            let window = CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                instance,
                (&mut *state as *mut State).cast::<c_void>(),
            );
            if window.is_null() {
                UnregisterClassW(class_name.as_ptr(), instance);
                return;
            }
            // Extract the first embedded executable icon; debug binaries may lack it.
            let mut icon = null_mut();
            let mut executable = vec![0_u16; 32768];
            let length = windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW(
                instance,
                executable.as_mut_ptr(),
                executable.len() as u32,
            );
            if length > 0 && (length as usize) < executable.len() {
                ExtractIconExW(executable.as_ptr(), 0, null_mut(), &mut icon, 1);
            }
            let owned_icon = !icon.is_null();
            if icon.is_null() {
                icon = LoadIconW(null_mut(), IDI_APPLICATION);
            }
            state.icon.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
            state.icon.hWnd = window;
            state.icon.uID = 1;
            state.icon.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            state.icon.uCallbackMessage = CALLBACK;
            state.icon.hIcon = icon;
            state.icon.szTip[..5].copy_from_slice(&[118, 111, 115, 117, 0]);
            if Shell_NotifyIconW(NIM_ADD, &state.icon) != 0 && SetTimer(window, 1, 100, None) != 0 {
                let mut message = MSG::default();
                while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            Shell_NotifyIconW(NIM_DELETE, &state.icon);
            KillTimer(window, 1);
            DestroyWindow(window);
            if owned_icon {
                DestroyIcon(icon);
            }
            UnregisterClassW(class_name.as_ptr(), instance);
        }
    }

    unsafe extern "system" fn window_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: GWLP_USERDATA points to run's Box until after DestroyWindow;
        // callbacks are serial on this thread. No pointer escapes this module.
        unsafe {
            if message == WM_NCCREATE {
                let create = &*(lparam as *const CREATESTRUCTW);
                SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
            }
            let pointer = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut State;
            if !pointer.is_null() {
                let state = &*pointer;
                if message == WM_TIMER && state.stop.is_cancelled() {
                    EndMenu();
                    PostQuitMessage(0);
                    return 0;
                }
                if state.taskbar_created != 0 && message == state.taskbar_created {
                    Shell_NotifyIconW(NIM_ADD, &state.icon);
                    return 0;
                }
                if message == CALLBACK {
                    match lparam as u32 {
                        WM_LBUTTONUP => state.actions.open(),
                        WM_RBUTTONUP | WM_CONTEXTMENU => {
                            let menu = CreatePopupMenu();
                            if !menu.is_null() {
                                AppendMenuW(menu, MF_STRING, OPEN, wide("Open dashboard").as_ptr());
                                AppendMenuW(menu, MF_STRING, QUIT, wide("Quit").as_ptr());
                                let mut point = POINT::default();
                                GetCursorPos(&mut point);
                                SetForegroundWindow(window);
                                let command = TrackPopupMenu(
                                    menu,
                                    TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                                    point.x,
                                    point.y,
                                    0,
                                    window,
                                    null(),
                                );
                                DestroyMenu(menu);
                                PostMessageW(window, WM_NULL, 0, 0);
                                match command as usize {
                                    OPEN => state.actions.open(),
                                    QUIT => state.actions.quit(),
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                    return 0;
                }
            }
            DefWindowProcW(window, message, wparam, lparam)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn windows_strings_are_terminated() {
            assert_eq!(wide("vosu"), vec![118, 111, 115, 117, 0]);
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use super::*;
    pub(super) fn start(_: Actions, _: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(async {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_coalesce_and_cancel_without_a_desktop() {
        let (open, mut requests) = mpsc::channel(1);
        let quit = CancellationToken::new();
        let actions = Actions {
            open,
            quit: quit.clone(),
        };
        actions.open();
        actions.open();
        assert!(requests.try_recv().is_ok());
        assert!(requests.try_recv().is_err());
        actions.quit();
        assert!(quit.is_cancelled());
    }

    #[tokio::test]
    async fn owner_shutdown_signals_and_waits_for_teardown() {
        let stop = CancellationToken::new();
        let service_stop = stop.clone();
        let removed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let service_removed = removed.clone();
        let service = tokio::spawn(async move {
            service_stop.cancelled().await;
            service_removed.store(true, std::sync::atomic::Ordering::Release);
        });
        let opener = tokio::spawn(std::future::pending());
        let mut handle = TrayHandle {
            stop,
            service: Some(service),
            opener,
        };
        handle.shutdown().await;
        assert!(removed.load(std::sync::atomic::Ordering::Acquire));
        assert!(handle.service.is_none());
        handle.shutdown().await;
    }
}
