//! Notification-area (tray) icon of the windowless client on Windows, drawn
//! with Shell_NotifyIconW on a hidden window. The icon shows that PlasmaTrack
//! runs; its tooltip and menu show the game connection and the upload state.
//!
//! The window is a hidden top-level window, so `taskkill` without `/F` asks it
//! to close (WM_CLOSE) and the client shuts down cleanly.
//!
//! Everything here runs on the thread that calls [`run`]. Callbacks only take
//! shared borrows of the thread-local state, because menus and shell calls
//! pump messages and re-enter the window procedure.

use std::cell::{Cell, RefCell};
use std::ptr::null;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SetForegroundWindow, SetMenuDefaultItem, SetTimer, TrackPopupMenu, TranslateMessage, HICON,
    MB_ICONERROR, MB_OK, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, SW_SHOWNORMAL, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_APP, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WM_TIMER,
    WNDCLASSW,
};

const WM_TRAY: u32 = WM_APP + 1;
const WM_EXIT: u32 = WM_APP + 2;
const TIMER_REFRESH: usize = 1;
const TIMER_FORCE_EXIT: usize = 2;
const ID_STATUS_PAGE: usize = 10;
const ID_DATA_FOLDER: usize = 11;
const ID_QUIT: usize = 12;

/// What the tray shows and does. `lines` are status lines for the tooltip and
/// the menu; they are re-read every two seconds.
pub struct Callbacks {
    pub title: String,
    pub lines: Box<dyn Fn() -> Vec<String>>,
    pub open_status: Box<dyn Fn()>,
    pub open_data: Box<dyn Fn()>,
    pub quit: Box<dyn Fn()>,
}

struct State {
    cb: Callbacks,
    icon: HICON,
    taskbar_created: u32,
    quitting: Cell<bool>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow().ok().and_then(|s| s.as_ref().map(f)))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Ends the tray loop from any thread (after the client stopped).
#[derive(Clone, Copy)]
pub struct Exit(usize);

impl Exit {
    pub fn post(self) {
        unsafe { PostMessageW(self.0 as HWND, WM_EXIT, 0, 0) };
    }
}

/// Opens a URL or folder with its default program.
pub fn open(target: &str) {
    let (verb, target) = (wide("open"), wide(target));
    unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), target.as_ptr(), null(), null(), SW_SHOWNORMAL) };
}

/// Shows an error dialog (any thread).
pub fn error_box(text: &str) {
    let (text, title) = (wide(text), wide("PlasmaTrack"));
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
}

/// 32×32 RGBA: a purple disc with a white "P".
pub fn icon_rgba() -> Vec<u8> {
    const N: usize = 32;
    let mut px = vec![0u8; N * N * 4];
    for y in 0..N {
        for x in 0..N {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = ((fx - 16.0).powi(2) + (fy - 16.0).powi(2)).sqrt();
            let alpha = (15.5 - d).clamp(0.0, 1.0);
            if alpha == 0.0 {
                continue;
            }
            let stem = (10.0..14.0).contains(&fx) && (7.0..25.0).contains(&fy);
            let b = ((fx - 15.0).powi(2) + (fy - 12.5).powi(2)).sqrt();
            let bowl = fx >= 12.0 && (3.0..6.5).contains(&b);
            let rgb = if stem || bowl { [255, 255, 255] } else { [106, 61, 232] };
            let i = (y * N + x) * 4;
            px[i..i + 3].copy_from_slice(&rgb);
            px[i + 3] = (alpha * 255.0) as u8;
        }
    }
    px
}

unsafe fn make_icon() -> HICON {
    let mut bgra = icon_rgba();
    // CreateIcon wants BGRA colour bits and an AND mask (inverted alpha).
    let mask: Vec<u8> = bgra.chunks_exact(4).map(|p| 255 - p[3]).collect();
    for p in bgra.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    CreateIcon(GetModuleHandleW(null()), 32, 32, 1, 32, mask.as_ptr(), bgra.as_ptr())
}

fn notify_data(hwnd: HWND, icon: HICON, tip: &str) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = icon;
    let max = nid.szTip.len() - 1; // keep the terminating NUL
    for (dst, src) in nid.szTip.iter_mut().zip(tip.encode_utf16().take(max)) {
        *dst = src;
    }
    nid
}

fn tooltip() -> String {
    with_state(|s| {
        let mut t = s.cb.title.clone();
        for l in (s.cb.lines)() {
            t.push('\n');
            t.push_str(&l);
        }
        t
    })
    .unwrap_or_default()
}

unsafe fn set_icon(hwnd: HWND, op: u32) {
    let tip = tooltip();
    if let Some(icon) = with_state(|s| s.icon) {
        Shell_NotifyIconW(op, &notify_data(hwnd, icon, &tip));
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let Some((title, lines)) = with_state(|s| (s.cb.title.clone(), (s.cb.lines)())) else { return };
    let menu = CreatePopupMenu();
    let item = |flags, id: usize, text: &str| {
        let t = wide(text);
        AppendMenuW(menu, flags, id, t.as_ptr());
    };
    item(MF_STRING | MF_GRAYED, 0, &title);
    for l in &lines {
        item(MF_STRING | MF_GRAYED, 0, l);
    }
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    item(MF_STRING, ID_STATUS_PAGE, "Open status page");
    item(MF_STRING, ID_DATA_FOLDER, "Open data folder");
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    item(MF_STRING, ID_QUIT, "Quit PlasmaTrack");
    SetMenuDefaultItem(menu, ID_STATUS_PAGE as u32, 0);

    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd); // so the menu closes when clicking elsewhere
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, null());
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
    match cmd as usize {
        ID_STATUS_PAGE => {
            with_state(|s| (s.cb.open_status)());
        }
        ID_DATA_FOLDER => {
            with_state(|s| (s.cb.open_data)());
        }
        ID_QUIT => request_quit(hwnd),
        _ => {}
    }
}

/// Asks the client to stop; the tray goes away when it has (WM_EXIT). If the
/// client does not stop within 10 s, the process exits anyway.
unsafe fn request_quit(hwnd: HWND) {
    let first = with_state(|s| !s.quitting.replace(true)).unwrap_or(false);
    if first {
        with_state(|s| (s.cb.quit)());
        SetTimer(hwnd, TIMER_FORCE_EXIT, 10_000, None);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lparam as u32 {
                WM_LBUTTONDBLCLK => {
                    with_state(|s| (s.cb.open_status)());
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_TIMER if wparam == TIMER_REFRESH => {
            set_icon(hwnd, NIM_MODIFY);
            0
        }
        WM_TIMER if wparam == TIMER_FORCE_EXIT => {
            if let Some(icon) = with_state(|s| s.icon) {
                Shell_NotifyIconW(NIM_DELETE, &notify_data(hwnd, icon, ""));
            }
            std::process::exit(0);
        }
        WM_CLOSE => {
            request_quit(hwnd);
            0
        }
        WM_EXIT => {
            KillTimer(hwnd, TIMER_REFRESH);
            if let Some(icon) = with_state(|s| s.icon) {
                Shell_NotifyIconW(NIM_DELETE, &notify_data(hwnd, icon, ""));
            }
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        m if Some(m) == with_state(|s| s.taskbar_created) => {
            set_icon(hwnd, NIM_ADD); // Explorer restarted
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Shows the icon and runs the message loop until [`Exit::post`]. `started`
/// gets the exit handle once the window exists; start the client there.
pub fn run(cb: Callbacks, started: impl FnOnce(Exit)) -> Result<(), String> {
    unsafe {
        let hinstance = GetModuleHandleW(null());
        let class = wide("PlasmaTrackTray");
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinstance;
        wc.lpszClassName = class.as_ptr();
        if RegisterClassW(&wc) == 0 {
            return Err("cannot register the tray window class".into());
        }
        let title = wide("PlasmaTrack");
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            null(),
        );
        if hwnd.is_null() {
            return Err("cannot create the tray window".into());
        }
        let icon = make_icon();
        let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        STATE.with(|s| *s.borrow_mut() = Some(State { cb, icon, taskbar_created, quitting: Cell::new(false) }));
        set_icon(hwnd, NIM_ADD);
        SetTimer(hwnd, TIMER_REFRESH, 2000, None);

        started(Exit(hwnd as usize));

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        STATE.with(|s| s.borrow_mut().take());
        DestroyIcon(icon);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn icon_is_a_disc_with_a_letter() {
        let px = super::icon_rgba();
        assert_eq!(px.len(), 32 * 32 * 4);
        let at = |x: usize, y: usize| &px[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4];
        assert_eq!(at(0, 0)[3], 0, "corner transparent");
        assert_eq!(at(24, 24), [106, 61, 232, 255], "disc");
        assert_eq!(at(11, 20), [255, 255, 255, 255], "stem of the P");
    }
}
