use std::{
    ffi::OsStr,
    os::windows::ffi::OsStrExt,
    ptr,
    sync::{Mutex, OnceLock},
    thread,
};

use anyhow::{bail, Result};
use tokio::sync::oneshot;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{
        Shell::{
            Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
        },
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
            LoadIconW, MessageBoxW, PostQuitMessage, PostThreadMessageW, RegisterClassW,
            TranslateMessage, HWND_MESSAGE, IDI_APPLICATION, IDYES, MB_ICONINFORMATION, MB_YESNO,
            MSG, WM_APP, WM_LBUTTONUP, WM_QUIT, WM_RBUTTONUP, WNDCLASSW,
        },
    },
};

const TRAY_MESSAGE: u32 = WM_APP + 1;
const TRAY_ID: u32 = 1;
static QUIT_SENDER: OnceLock<Mutex<Option<oneshot::Sender<()>>>> = OnceLock::new();

pub struct TrayIcon {
    thread_id: u32,
    worker: Option<thread::JoinHandle<()>>,
}

impl TrayIcon {
    pub fn start() -> Result<(Self, oneshot::Receiver<()>)> {
        let (quit, receive_quit) = oneshot::channel();
        let sender = QUIT_SENDER.get_or_init(|| Mutex::new(None));
        *sender
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(quit);
        let (ready, receive_ready) = std::sync::mpsc::channel();
        let worker = thread::Builder::new()
            .name("cruisemesh-tray".into())
            .spawn(move || tray_thread(ready))?;
        let thread_id = receive_ready.recv()??;
        Ok((
            Self {
                thread_id,
                worker: Some(worker),
            },
            receive_quit,
        ))
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        unsafe {
            PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn tray_thread(ready: std::sync::mpsc::Sender<Result<u32>>) {
    let result = unsafe { create_tray() };
    let Ok((window, icon)) = result else {
        let _ = ready.send(result.map(|_| unreachable!()));
        return;
    };
    let _ = ready.send(Ok(unsafe { GetCurrentThreadId() }));
    let mut message: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    unsafe {
        Shell_NotifyIconW(NIM_DELETE, &icon);
        DestroyWindow(window);
    }
}

unsafe fn create_tray() -> Result<(HWND, NOTIFYICONDATAW)> {
    let class_name = wide("CruiseMeshHelperTrayWindow");
    let instance = GetModuleHandleW(ptr::null());
    let class = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(window_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: ptr::null_mut(),
        hCursor: ptr::null_mut(),
        hbrBackground: ptr::null_mut(),
        lpszMenuName: ptr::null(),
        lpszClassName: class_name.as_ptr(),
    };
    if RegisterClassW(&class) == 0 {
        bail!("failed to register the CruiseMesh tray window");
    }
    let window = CreateWindowExW(
        0,
        class_name.as_ptr(),
        class_name.as_ptr(),
        0,
        0,
        0,
        0,
        0,
        HWND_MESSAGE,
        ptr::null_mut(),
        instance,
        ptr::null(),
    );
    if window.is_null() {
        bail!("failed to create the CruiseMesh tray window");
    }
    let mut icon: NOTIFYICONDATAW = std::mem::zeroed();
    icon.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    icon.hWnd = window;
    icon.uID = TRAY_ID;
    icon.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    icon.uCallbackMessage = TRAY_MESSAGE;
    icon.hIcon = LoadIconW(ptr::null_mut(), IDI_APPLICATION);
    copy_wide("CruiseMesh Helper — Running", &mut icon.szTip);
    if Shell_NotifyIconW(NIM_ADD, &icon) == 0 {
        DestroyWindow(window);
        bail!("failed to add the CruiseMesh notification icon");
    }
    Ok((window, icon))
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == TRAY_MESSAGE {
        if lparam as u32 == WM_LBUTTONUP {
            let title = wide("CruiseMesh Helper");
            let body =
                wide("CruiseMesh Helper is running. Use `cruisemesh-node status` for details.");
            MessageBoxW(window, body.as_ptr(), title.as_ptr(), MB_ICONINFORMATION);
            return 0;
        }
        if lparam as u32 == WM_RBUTTONUP {
            let title = wide("CruiseMesh Helper");
            let body = wide("Quit CruiseMesh Helper?");
            if MessageBoxW(window, body.as_ptr(), title.as_ptr(), MB_YESNO) == IDYES {
                if let Some(sender) = QUIT_SENDER.get() {
                    if let Some(sender) = sender
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take()
                    {
                        let _ = sender.send(());
                    }
                }
                PostQuitMessage(0);
            }
            return 0;
        }
    }
    DefWindowProcW(window, message, wparam, lparam)
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn copy_wide(value: &str, destination: &mut [u16]) {
    let source = wide(value);
    let count = source.len().min(destination.len());
    destination[..count].copy_from_slice(&source[..count]);
    destination[destination.len() - 1] = 0;
}
