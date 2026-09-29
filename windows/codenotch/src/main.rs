#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod autostart;
mod config;
mod doctor;
mod focus;
mod hooks_install;
mod i18n;
mod server;
mod state;
mod tray;
mod usage;
mod codex;
mod cursor;
mod antigravity;
mod agy_cli;
mod glyphs;
mod trayicon;
mod activity;
mod diag;
mod watcher;
mod hardware;

use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

/// Logical size of the notch window: the 70 pt pill column on the right plus room for the hover card on the left.
pub const NOTCH_W: f64 = 360.0;
/// Hand-bumped build tag, written to run.log at startup so a log can always be matched to the exe that wrote it.
pub const BUILD: &str = "r32";
pub const NOTCH_H: f64 = 720.0; // 720 allows all multi-meter AI tools and telemetry cards to expand without clipping

pub struct AppState {
    pub store: Mutex<state::Store>,
    pub cfg: Mutex<config::Config>,
    pub usage: Mutex<usage::UsageSnapshot>,
    /// Codex snapshot (same UsageSnapshot shape; status may also be none/absent)
    pub codex: Mutex<usage::UsageSnapshot>,
    pub cursor: Mutex<usage::UsageSnapshot>,
    pub antigravity: Mutex<usage::UsageSnapshot>,
    /// Provider glyph cache, collected at launch and again on a tray refresh
    pub glyphs: Mutex<std::collections::HashMap<String, glyphs::Glyph>>,
    /// Working state of the non-Claude providers (Cursor reports it; Codex and Antigravity are inferred from recent writes)
    pub activity: Mutex<Vec<activity::Activity>>,
}

fn resolved_lang(raw: &str) -> String {
    if raw == "auto" {
        i18n::resolve_auto().to_string()
    } else {
        raw.to_string()
    }
}

/// The size multiplier chosen with the slider, clamped to what the config allows.
pub fn ui_scale(app: &AppHandle) -> f64 {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    c.scale.clamp(config::SCALE_MIN, config::SCALE_MAX)
}

pub fn broadcast(app: &AppHandle) {
    let st = app.state::<AppState>();
    let snap = {
        let store = st.store.lock().unwrap();
        let cfg = st.cfg.lock().unwrap();
        store.snapshot(&cfg.lang, &resolved_lang(&cfg.lang), false)
    };
    let _ = app.emit("state", &snap);
}

/// Pins the notch to the right edge of the primary monitor; the other edges are a later milestone.
pub fn place_notch(app: &AppHandle) {
    let Some(w) = app.get_webview_window("notch") else {
        return;
    };
    let scale = w.scale_factor().unwrap_or(1.0);
    if let Ok(Some(mon)) = w.primary_monitor() {
        let ms = mon.scale_factor();
        let (edge, ratio) = {
            let st = app.state::<AppState>();
            let c = st.cfg.lock().unwrap();
            (c.screen_edge.clone(), c.notch_y.clamp(0.0, 1.0))
        };
        let (nw, nh) = if edge == "top" { (500.0, 340.0) } else { (NOTCH_W, NOTCH_H) };
        let target = tauri::PhysicalSize::new((nw * ms).round() as u32, (nh * ms).round() as u32);
        let _ = w.set_size(target);

        let (ww, wh) = w
            .outer_size()
            .map(|s| (s.width as i32, s.height as i32))
            .unwrap_or(((nw * scale) as i32, (nh * scale) as i32));

        let (x, y) = if edge == "left" {
            let x = mon.position().x;
            let mh = mon.size().height as i32;
            let y = (mon.position().y as f64 + mh as f64 * ratio - wh as f64 / 2.0).round() as i32;
            let y = y.clamp(mon.position().y, mon.position().y + (mh - wh).max(0));
            (x, y)
        } else if edge == "top" {
            let mw = mon.size().width as i32;
            let x = (mon.position().x as f64 + mw as f64 * ratio - ww as f64 / 2.0).round() as i32;
            let x = x.clamp(mon.position().x, mon.position().x + (mw - ww).max(0));
            let y = mon.position().y;
            (x, y)
        } else {
            // "right" edge (default)
            let x = mon.position().x + mon.size().width as i32 - ww;
            let mh = mon.size().height as i32;
            let y = (mon.position().y as f64 + mh as f64 * ratio - wh as f64 / 2.0).round() as i32;
            let y = y.clamp(mon.position().y, mon.position().y + (mh - wh).max(0));
            (x, y)
        };

        let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
        if w.outer_size().map(|s| s.width != target.width).unwrap_or(false) {
            let _ = w.set_size(target);
            let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
        }
        // Placement log line: the first thing to check when the notch is not visible
        let log = config::config_path().with_file_name("run.log");
        let _ = std::fs::write(
            log,
            format!(
                "notch placed build={BUILD}: pos=({x},{y}) size=({ww}x{wh}) inner={:?} win_scale={scale} mon_scale={ms} monitor=({},{} {}x{})\n",
                w.inner_size().map(|s| (s.width, s.height)).unwrap_or((0, 0)),
                mon.position().x,
                mon.position().y,
                mon.size().width,
                mon.size().height
            ),
        );
    }
}

/// Older entry point name still used by tray.rs
pub fn reset_bar(app: &AppHandle) {
    {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.notch_y = 0.5;
        config::save(&c);
    }
    place_notch(app);
}

/// Drag along the right edge. The page calls this once after a press on the pill moves more than
/// 4 px; from then on a Rust thread follows the system cursor (WebView mousemove is unreliable
/// once the window itself starts moving). Releasing the left button ends the drag and the centre
/// ratio is written back to the config.
static DRAGGING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
fn left_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}
#[cfg(not(windows))]
fn left_button_down() -> bool {
    false
}

#[cfg(windows)]
fn any_mouse_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
    unsafe {
        ((GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0)
            || ((GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16 & 0x8000) != 0)
            || ((GetAsyncKeyState(VK_MBUTTON.0 as i32) as u16 & 0x8000) != 0)
    }
}
#[cfg(not(windows))]
fn any_mouse_button_down() -> bool {
    false
}

#[tauri::command]
fn drag_begin(app: AppHandle) {
    if DRAGGING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let Some(w) = app.get_webview_window("notch") else {
            DRAGGING.store(false, std::sync::atomic::Ordering::SeqCst);
            return;
        };
        let (Ok(start_cur), Ok(start_pos), Ok(size), Ok(Some(mon))) =
            (app.cursor_position(), w.outer_position(), w.outer_size(), w.primary_monitor())
        else {
            DRAGGING.store(false, std::sync::atomic::Ordering::SeqCst);
            return;
        };
        let (my, mh) = (mon.position().y, mon.size().height as i32);
        let wh = size.height as i32;
        let lo = my;
        let hi = my + (mh - wh).max(0);
        let mut last_y = start_pos.y;
        let mut moved = false;
        loop {
            if !left_button_down() {
                break;
            }
            if let Ok(cur) = app.cursor_position() {
                let ny = (start_pos.y as f64 + (cur.y - start_cur.y)).round() as i32;
                let ny = ny.clamp(lo, hi);
                if ny != last_y {
                    last_y = ny;
                    moved = true;
                    let _ = w.set_position(tauri::PhysicalPosition::new(start_pos.x, ny));
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
        if moved {
            let ratio = ((last_y + wh / 2 - my) as f64 / mh as f64).clamp(0.0, 1.0);
            let st = app.state::<AppState>();
            let mut c = st.cfg.lock().unwrap();
            c.notch_y = ratio;
            config::save(&c);
            applog(&format!("notch drag: y={last_y} ratio={ratio:.3}"));
        }
        DRAGGING.store(false, std::sync::atomic::Ordering::SeqCst);
        let _ = app.emit("drag_end", moved);
    });
}
pub fn place_bar(app: &AppHandle) {
    place_notch(app);
}
pub fn toggle_drag(app: &AppHandle) {
    // The notch stays welded to the edge; kept as a no-op for the tray menu code path
    let _ = app;
}

pub fn apply_lang(app: &AppHandle, lang: &str) {
    let resolved = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.lang = lang.to_string();
        config::save(&c);
        resolved_lang(&c.lang)
    };
    // Through refresh_menu, which makes sure the swap happens on the main thread: doing it from the
    // settings window's thread left the tray with a menu that would never open again.
    tray::refresh_menu(app);
    broadcast(app);
    let _ = app.emit("lang", resolved);
}

/// The notch must never take focus: WS_EX_NOACTIVATE + WS_EX_TOOLWINDOW
#[cfg(windows)]
fn noactivate(app: &AppHandle) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };
    if let Some(w) = app.get_webview_window("notch") {
        if let Ok(h) = w.hwnd() {
            unsafe {
                let hwnd =
                    windows::Win32::Foundation::HWND(h.0 as isize as *mut core::ffi::c_void);
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                SetWindowLongPtrW(
                    hwnd,
                    GWL_EXSTYLE,
                    ex | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize,
                );
            }
        }
    }
}
#[cfg(not(windows))]
fn noactivate(_app: &AppHandle) {}

#[cfg(windows)]
pub mod win_backdrop {
    use std::ffi::c_void;
    use windows::Win32::Foundation::HWND;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct AccentPolicy {
        pub accent_state: u32,
        pub accent_flags: u32,
        pub gradient_color: u32,
        pub animation_id: u32,
    }

    #[repr(C)]
    pub struct WindowCompositionAttributeData {
        pub attribute: u32, // 19 = WCA_ACCENT_POLICY
        pub data: *mut c_void,
        pub size_of_data: usize,
    }

    type SetWindowCompositionAttributeFn =
        unsafe extern "system" fn(hwnd: HWND, data: *mut WindowCompositionAttributeData) -> i32;

    type DwmSetWindowAttributeFn = unsafe extern "system" fn(
        hwnd: HWND,
        dw_attribute: u32,
        pv_attribute: *const c_void,
        cb_attribute: u32,
    ) -> i32;

    unsafe fn get_set_window_composition_attribute() -> Option<SetWindowCompositionAttributeFn> {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetModuleHandleA(lpModuleName: *const u8) -> *mut c_void;
            fn GetProcAddress(hModule: *mut c_void, lpProcName: *const u8) -> *mut c_void;
        }
        let h_user32 = GetModuleHandleA(b"user32.dll\0".as_ptr());
        if h_user32.is_null() {
            return None;
        }
        let proc = GetProcAddress(h_user32, b"SetWindowCompositionAttribute\0".as_ptr());
        if proc.is_null() {
            return None;
        }
        Some(core::mem::transmute(proc))
    }

    unsafe fn get_dwm_set_window_attribute() -> Option<DwmSetWindowAttributeFn> {
        #[link(name = "kernel32")]
        extern "system" {
            fn LoadLibraryA(lpLibFileName: *const u8) -> *mut c_void;
            fn GetProcAddress(hModule: *mut c_void, lpProcName: *const u8) -> *mut c_void;
        }
        let h_dwmapi = LoadLibraryA(b"dwmapi.dll\0".as_ptr());
        if h_dwmapi.is_null() {
            return None;
        }
        let proc = GetProcAddress(h_dwmapi, b"DwmSetWindowAttribute\0".as_ptr());
        if proc.is_null() {
            return None;
        }
        Some(core::mem::transmute(proc))
    }

    pub fn set_window_backdrop(hwnd: HWND, style: &str) {
        let enable_acrylic = style != "oled" && style != "solid" && style != "none";

        unsafe {
            // 1. Try Windows 11 (22H2+ Build 22621+) DWMWA_SYSTEMBACKDROP_TYPE (38)
            // 3 = DWMSBT_TRANSIENTWINDOW (Acrylic), 1 = DWMSBT_NONE
            let mut dwm_success = false;
            if let Some(dwm_set_attr) = get_dwm_set_window_attribute() {
                let corner_pref: u32 = if enable_acrylic { 0 } else { 1 }; // 0 = DEFAULT, 1 = DONOTROUND
                let _ = dwm_set_attr(
                    hwnd,
                    33, // DWMWA_WINDOW_CORNER_PREFERENCE
                    &corner_pref as *const _ as *const c_void,
                    core::mem::size_of::<u32>() as u32,
                );

                let backdrop_type: u32 = if enable_acrylic { 3 } else { 1 };
                let hr = dwm_set_attr(
                    hwnd,
                    38, // DWMWA_SYSTEMBACKDROP_TYPE
                    &backdrop_type as *const _ as *const c_void,
                    core::mem::size_of::<u32>() as u32,
                );
                if hr == 0 {
                    dwm_success = true;
                }
            }

            // 2. Fallback to SetWindowCompositionAttribute (Windows 10 / pre-22H2 Windows 11)
            if !dwm_success || !enable_acrylic {
                if let Some(set_wca) = get_set_window_composition_attribute() {
                    let mut policy = AccentPolicy {
                        // 4 = ACCENT_ENABLE_ACRYLICBLURBEHIND, 0 = ACCENT_DISABLED
                        accent_state: if enable_acrylic { 4 } else { 0 },
                        accent_flags: if enable_acrylic { 2 } else { 0 }, // 2 = draw all borders
                        gradient_color: if enable_acrylic { 0x01101014 } else { 0 },
                        animation_id: 0,
                    };
                    let mut data = WindowCompositionAttributeData {
                        attribute: 19, // WCA_ACCENT_POLICY
                        data: &mut policy as *mut _ as *mut c_void,
                        size_of_data: core::mem::size_of::<AccentPolicy>(),
                    };
                    let _ = set_wca(hwnd, &mut data);
                }
            }
        }
    }

    pub fn set_dark_mode(hwnd: HWND, dark: bool) {
        unsafe {
            if let Some(dwm_set_attr) = get_dwm_set_window_attribute() {
                let dark_val: u32 = if dark { 1 } else { 0 };
                let _ = dwm_set_attr(
                    hwnd,
                    20, // DWMWA_USE_IMMERSIVE_DARK_MODE
                    &dark_val as *const _ as *const c_void,
                    core::mem::size_of::<u32>() as u32,
                );
            }
        }
    }
}

pub fn apply_backdrop(app: &AppHandle, label: &str, style: &str) {
    #[cfg(windows)]
    if let Some(w) = app.get_webview_window(label) {
        if let Ok(h) = w.hwnd() {
            let hwnd = windows::Win32::Foundation::HWND(h.0 as isize as *mut core::ffi::c_void);
            if label == "settings" {
                win_backdrop::set_dark_mode(hwnd, true);
            } else if label != "notch" && label != "overlay" {
                win_backdrop::set_window_backdrop(hwnd, style);
            }
        }
    }
}
#[cfg(not(windows))]
pub fn apply_backdrop(_app: &AppHandle, _label: &str, _style: &str) {}

static HOTKEY_THREAD_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const WM_RELOAD_HOTKEY: u32 = 0x8000 + 101;

pub fn notify_hotkey_change() {
    #[cfg(windows)]
    {
        let tid = HOTKEY_THREAD_ID.load(std::sync::atomic::Ordering::SeqCst);
        if tid != 0 {
            use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
            unsafe {
                let _ = PostThreadMessageW(
                    tid,
                    WM_RELOAD_HOTKEY,
                    windows::Win32::Foundation::WPARAM(0),
                    windows::Win32::Foundation::LPARAM(0),
                );
            }
        }
    }
}

#[cfg(windows)]
pub fn parse_hotkey_str(s: &str) -> Option<(windows::Win32::UI::Input::KeyboardAndMouse::HOT_KEY_MODIFIERS, u32)> {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let mut mods = MOD_NOREPEAT;
    let mut vk: Option<u32> = None;

    for part in s.split('+').map(|p| p.trim().to_uppercase()) {
        match part.as_str() {
            "CTRL" | "CONTROL" => mods |= MOD_CONTROL,
            "ALT" => mods |= MOD_ALT,
            "SHIFT" => mods |= MOD_SHIFT,
            "WIN" | "WINDOWS" => mods |= MOD_WIN,
            "SPACE" => vk = Some(VK_SPACE.0 as u32),
            "F1" => vk = Some(VK_F1.0 as u32),
            "F2" => vk = Some(VK_F2.0 as u32),
            "F3" => vk = Some(VK_F3.0 as u32),
            "F4" => vk = Some(VK_F4.0 as u32),
            "F5" => vk = Some(VK_F5.0 as u32),
            "F6" => vk = Some(VK_F6.0 as u32),
            "F7" => vk = Some(VK_F7.0 as u32),
            "F8" => vk = Some(VK_F8.0 as u32),
            "F9" => vk = Some(VK_F9.0 as u32),
            "F10" => vk = Some(VK_F10.0 as u32),
            "F11" => vk = Some(VK_F11.0 as u32),
            "F12" => vk = Some(VK_F12.0 as u32),
            other if other.len() == 1 => {
                let ch = other.chars().next().unwrap();
                if ch.is_ascii_alphanumeric() {
                    vk = Some(ch as u32);
                }
            }
            _ => {}
        }
    }
    vk.map(|k| (mods, k))
}
#[cfg(not(windows))]
pub fn parse_hotkey_str(_s: &str) -> Option<(u32, u32)> {
    None
}

/// Global hotkey listener running in a background Win32 message loop thread.
/// Toggles the notch's visibility via apply_visibility and persists the preference.
#[cfg(windows)]
fn start_hotkey_listener(app: AppHandle) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, MSG, WM_HOTKEY};

    std::thread::spawn(move || {
        HOTKEY_THREAD_ID.store(
            unsafe { windows::Win32::System::Threading::GetCurrentThreadId() },
            std::sync::atomic::Ordering::SeqCst,
        );
        const HOTKEY_ID: i32 = 0x434E; // "CN"
        const OVERLAY_HOTKEY_ID: i32 = 0x434F; // "CO"
        const LOCK_HOTKEY_ID: i32 = 0x434C; // "CL"

        let register_active = |app: &AppHandle| {
            let st = app.state::<AppState>();
            let c = st.cfg.lock().unwrap();
            if let Some((mods, vk)) = parse_hotkey_str(&c.hotkey_toggle) {
                unsafe {
                    let _ = UnregisterHotKey(HWND(std::ptr::null_mut()), HOTKEY_ID);
                    if let Err(e) = RegisterHotKey(HWND(std::ptr::null_mut()), HOTKEY_ID, mods, vk) {
                        applog(&format!("hotkey: RegisterHotKey ({}) failed: {e}", c.hotkey_toggle));
                    } else {
                        applog(&format!("hotkey: registered global shortcut {}", c.hotkey_toggle));
                    }
                }
            }
            if let Some((mods, vk)) = parse_hotkey_str(&c.hotkey_overlay) {
                unsafe {
                    let _ = UnregisterHotKey(HWND(std::ptr::null_mut()), OVERLAY_HOTKEY_ID);
                    if let Err(e) = RegisterHotKey(HWND(std::ptr::null_mut()), OVERLAY_HOTKEY_ID, mods, vk) {
                        applog(&format!("hotkey: RegisterHotKey overlay ({}) failed: {e}", c.hotkey_overlay));
                    } else {
                        applog(&format!("hotkey: registered global overlay shortcut {}", c.hotkey_overlay));
                    }
                }
            }
            if let Some((mods, vk)) = parse_hotkey_str(&c.hotkey_lock) {
                unsafe {
                    let _ = UnregisterHotKey(HWND(std::ptr::null_mut()), LOCK_HOTKEY_ID);
                    if let Err(e) = RegisterHotKey(HWND(std::ptr::null_mut()), LOCK_HOTKEY_ID, mods, vk) {
                        applog(&format!("hotkey: RegisterHotKey lock ({}) failed: {e}", c.hotkey_lock));
                    } else {
                        applog(&format!("hotkey: registered global clickthrough lock shortcut {}", c.hotkey_lock));
                    }
                }
            }
        };

        register_active(&app);

        let mut msg = MSG::default();
        unsafe {
            while GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0).as_bool() {
                if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == HOTKEY_ID {
                    let next_vis = {
                        let st = app.state::<AppState>();
                        let mut c = st.cfg.lock().unwrap();
                        c.notch_visible = !c.notch_visible;
                        // Avoid leaving app completely unreachable
                        if !c.notch_visible && !c.tray_visible {
                            c.tray_visible = true;
                        }
                        config::save(&c);
                        c.notch_visible
                    };
                    applog(&format!("hotkey: toggled notch_visible -> {next_vis}"));
                    let handle = app.clone();
                    let _ = app.run_on_main_thread(move || {
                        apply_visibility(&handle);
                    });
                } else if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == OVERLAY_HOTKEY_ID {
                    let handle = app.clone();
                    let _ = app.run_on_main_thread(move || {
                        toggle_overlay_window(&handle);
                    });
                } else if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == LOCK_HOTKEY_ID {
                    let handle = app.clone();
                    let _ = app.run_on_main_thread(move || {
                        let current_ct = {
                            let st = handle.state::<AppState>();
                            let c = st.cfg.lock().unwrap();
                            c.overlay_clickthrough
                        };
                        let next_ct = !current_ct;
                        let _ = set_overlay_clickthrough(handle.clone(), next_ct);
                        applog(&format!("hotkey: toggled overlay clickthrough -> {next_ct}"));
                    });
                } else if msg.message == WM_RELOAD_HOTKEY {
                    register_active(&app);
                }
            }
            let _ = UnregisterHotKey(HWND(std::ptr::null_mut()), HOTKEY_ID);
            let _ = UnregisterHotKey(HWND(std::ptr::null_mut()), OVERLAY_HOTKEY_ID);
            let _ = UnregisterHotKey(HWND(std::ptr::null_mut()), LOCK_HOTKEY_ID);
        }
    });
}

#[cfg(not(windows))]
fn start_hotkey_listener(_app: AppHandle) {}

// ---------------- windows 11 theme & accent ----------------

#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct SystemTheme {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub hex: String,
    pub is_dark: bool,
}

#[cfg(windows)]
pub fn get_system_theme() -> SystemTheme {
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmGetColorizationColor(pcrcolorization: *mut u32, pfopaqueblend: *mut i32) -> i32;
    }

    let mut color: u32 = 0;
    let mut opaque: i32 = 0;
    let res = unsafe { DwmGetColorizationColor(&mut color, &mut opaque) };

    let (r, g, b) = if res == 0 && color != 0 {
        (
            ((color >> 16) & 0xFF) as u8,
            ((color >> 8) & 0xFF) as u8,
            (color & 0xFF) as u8,
        )
    } else {
        (0, 120, 215) // Windows 11 default accent blue
    };

    let is_dark = check_dark_mode().unwrap_or(true);

    SystemTheme {
        r,
        g,
        b,
        hex: format!("#{r:02x}{g:02x}{b:02x}"),
        is_dark,
    }
}

#[cfg(windows)]
fn check_dark_mode() -> Option<bool> {
    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(
            hkey: usize,
            lpsubkey: *const u16,
            uloptions: u32,
            samdesired: u32,
            phkresult: *mut usize,
        ) -> i32;
        fn RegQueryValueExW(
            hkey: usize,
            lpvaluename: *const u16,
            lpreserved: *mut u32,
            lptype: *mut u32,
            lpdata: *mut u8,
            lpcbdata: *mut u32,
        ) -> i32;
        fn RegCloseKey(hkey: usize) -> i32;
    }

    const HKEY_CURRENT_USER: usize = 0x8000_0001;
    const KEY_READ: u32 = 0x20019;

    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
        .encode_utf16()
        .collect();
    let val_name: Vec<u16> = "AppsUseLightTheme\0".encode_utf16().collect();

    unsafe {
        let mut hkey: usize = 0;
        if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
            return None;
        }
        let mut val: u32 = 0;
        let mut size: u32 = std::mem::size_of::<u32>() as u32;
        let mut ty: u32 = 0;
        let q = RegQueryValueExW(
            hkey,
            val_name.as_ptr(),
            std::ptr::null_mut(),
            &mut ty,
            &mut val as *mut u32 as *mut u8,
            &mut size,
        );
        let _ = RegCloseKey(hkey);
        if q == 0 {
            Some(val == 0) // 0 = dark mode, 1 = light mode
        } else {
            None
        }
    }
}

#[cfg(not(windows))]
pub fn get_system_theme() -> SystemTheme {
    SystemTheme {
        r: 0,
        g: 120,
        b: 215,
        hex: "#0078d4".into(),
        is_dark: true,
    }
}

#[tauri::command]
fn get_theme() -> SystemTheme {
    get_system_theme()
}

fn start_theme_watcher(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last = get_system_theme();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3));
            let current = get_system_theme();
            if current != last {
                let _ = app.emit("theme", &current);
                last = current;
            }
        }
    });
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct AppearanceConfig {
    pub style: String,
    pub custom_accent: String,
    pub autohide: bool,
    pub autohide_delay: u32,
    pub screen_edge: String,
    pub autohide_peek: u32,
}

#[tauri::command]
fn get_appearance(app: AppHandle) -> AppearanceConfig {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    AppearanceConfig {
        style: c.appearance_style.clone(),
        custom_accent: c.custom_accent.clone(),
        autohide: c.autohide,
        autohide_delay: c.autohide_delay,
        screen_edge: c.screen_edge.clone(),
        autohide_peek: c.autohide_peek,
    }
}

#[tauri::command]
fn set_appearance(app: AppHandle, cfg: AppearanceConfig) {
    let edge_changed = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        let changed = c.screen_edge != cfg.screen_edge;
        c.appearance_style = cfg.style.clone();
        c.custom_accent = cfg.custom_accent.clone();
        c.autohide = cfg.autohide;
        c.autohide_delay = cfg.autohide_delay;
        c.screen_edge = cfg.screen_edge.clone();
        c.autohide_peek = cfg.autohide_peek;
        config::save(&c);
        changed
    };
    if edge_changed {
        place_notch(&app);
    }
    apply_backdrop(&app, "notch", &cfg.style);
    apply_backdrop(&app, "settings", &cfg.style);
    let _ = app.emit("appearance", &cfg);
}

#[tauri::command]
fn set_backdrop(app: AppHandle, label: String, style: String) {
    apply_backdrop(&app, &label, &style);
}

#[tauri::command]
fn get_notch_y(app: AppHandle) -> f64 {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    c.notch_y
}

#[tauri::command]
fn set_notch_y(app: AppHandle, ratio: f64) {
    let clamped = ratio.clamp(0.05, 0.95);
    {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.notch_y = clamped;
        config::save(&c);
    }
    place_notch(&app);
    let _ = app.emit("notch_y", clamped);
}

#[tauri::command]
fn get_hotkey(app: AppHandle) -> String {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    c.hotkey_toggle.clone()
}

#[tauri::command]
fn set_hotkey(app: AppHandle, hotkey: String) -> Result<String, String> {
    let clean = hotkey.trim().to_string();
    if clean.is_empty() {
        return Err("Shortcut cannot be empty.".into());
    }
    let current = {
        let st = app.state::<AppState>();
        let c = st.cfg.lock().unwrap();
        c.hotkey_toggle.clone()
    };

    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
        let (mods, vk) = parse_hotkey_str(&clean)
            .ok_or_else(|| "Invalid shortcut combination. Use modifiers like Ctrl+Alt+N or Alt+Shift+Space.".to_string())?;

        // If this shortcut is already the active registered hotkey for this process, it is validated and active
        if !clean.eq_ignore_ascii_case(&current) {
            const TEST_HOTKEY_ID: i32 = 0x4354; // "CT"
            unsafe {
                let _ = UnregisterHotKey(HWND(std::ptr::null_mut()), TEST_HOTKEY_ID);
                if let Err(e) = RegisterHotKey(HWND(std::ptr::null_mut()), TEST_HOTKEY_ID, mods, vk) {
                    return Err(format!("Shortcut '{clean}' is already reserved by Windows or another application ({e})."));
                }
                let _ = UnregisterHotKey(HWND(std::ptr::null_mut()), TEST_HOTKEY_ID);
            }
        }
    }

    {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.hotkey_toggle = clean.clone();
        config::save(&c);
    }
    notify_hotkey_change();
    let _ = app.emit("hotkey", &clean);
    Ok(format!("Shortcut '{clean}' registered successfully."))
}

// ---------------- commands ----------------

#[tauri::command]
fn get_state(state: tauri::State<AppState>) -> state::Snapshot {
    let store = state.store.lock().unwrap();
    let cfg = state.cfg.lock().unwrap();
    store.snapshot(&cfg.lang, &resolved_lang(&cfg.lang), false)
}

#[tauri::command]
fn get_usage(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.usage.lock().unwrap().clone()
}

#[tauri::command]
fn refresh_usage(app: AppHandle) {
    {
        let st = app.state::<AppState>();
        let mut u = st.usage.lock().unwrap();
        u.backoff_until = 0;
    }
    usage::request_refresh();
    codex::request_refresh();
    cursor::request_refresh();
    antigravity::request_refresh();
}

#[tauri::command]
fn get_antigravity(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.antigravity.lock().unwrap().clone()
}

#[tauri::command]
fn get_activity(state: tauri::State<AppState>) -> Vec<activity::Activity> {
    state.activity.lock().unwrap().clone()
}

#[tauri::command]
fn get_glyphs(state: tauri::State<AppState>) -> std::collections::HashMap<String, glyphs::Glyph> {
    state.glyphs.lock().unwrap().clone()
}

/// Collects the glyphs again and pushes them to the page (tray refresh, or the user just dropped in an override)
pub fn reload_glyphs(app: &AppHandle) {
    let m = glyphs::collect();
    let st = app.state::<AppState>();
    *st.glyphs.lock().unwrap() = m.clone();
    let _ = app.emit("glyphs", &m);
}

#[tauri::command]
fn open_data_dir() {
    let dir = config::config_path().parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let _ = std::fs::create_dir_all(glyphs::user_dir());
    let mut cmd = std::process::Command::new("explorer");
    cmd.arg(dir.as_os_str());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd.spawn();
}

#[tauri::command]
fn get_cursor(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.cursor.lock().unwrap().clone()
}

#[tauri::command]
fn get_codex(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.codex.lock().unwrap().clone()
}

/// A click on a cell opens that provider's usage page
#[tauri::command]
fn open_provider_page(provider: String) {
    let url = match provider.as_str() {
        "codex" => "https://chatgpt.com/#settings/Account",
        "cursor" => "https://cursor.com/dashboard",
        "gemini" => "https://antigravity.google",
        _ => "https://claude.ai/settings/usage",
    };
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/C", "start", "", url]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd.spawn();
}

/// Hot rectangles in **physical pixels**, window-relative, as x,y,w,h: the pill, plus the card
/// while it is open. The page converts by its own devicePixelRatio before reporting, so no scale
/// conversion happens here — WebView2's DPR and the window's scale_factor can disagree (see
/// report_dpr).
///
/// Empty means click-through: before the page has reported, one lost click on the notch beats
/// eating every click aimed at the window behind it.
static HOT: Mutex<Vec<[f64; 4]>> = Mutex::new(Vec::new());

/// Read only by the collapse timer — the click gate goes by the rectangles, since the pill is
/// clickable whether or not the card is up.
static EXPANDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[tauri::command]
fn set_hot(rects: Vec<[f64; 4]>, expanded: bool) {
    *HOT.lock().unwrap() = rects;
    EXPANDED.store(expanded, std::sync::atomic::Ordering::Relaxed);
    if expanded {
        antigravity::request_hover_refresh();
    }
}

/// Setting `WS_EX_TRANSPARENT` by hand instead looks like it should work, and does not: it applies
/// to the notch window, but WebView2 keeps child HWNDs that hit-testing descends into and they
/// never get the bit. `WS_EX_LAYERED` is what makes the window answer as one surface, so the helper
/// that sets both is the only route. Clearing it again is safe — the notch is not otherwise layered
/// (its transparency is DWM composition), so the window returns to the styles it had.
fn set_click_through(app: &AppHandle, on: bool) {
    let Some(w) = app.get_webview_window("notch") else { return };
    let _ = w.set_ignore_cursor_events(on);
}

/// The WebView zoom currently applied (1.0 = uncorrected)
static ZOOM: Mutex<f64> = Mutex::new(1.0);

pub fn applog(line: &str) {
    use std::io::Write;
    let log = config::config_path().with_file_name("run.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(f, "{line}");
    }
}

/// Root cause: with two monitors (150 % / 200 %) WebView2 picked a devicePixelRatio of 2.0 while
/// the window was sized for the primary monitor's 1.5, so the page was 255 CSS px wide instead of
/// the designed 340 and every coordinate conversion was off (the watchdog misfired and the card
/// flashed away). Fix: the page reports its DPR, and when it differs from the primary monitor's
/// scale, set_zoom pulls the effective DPR back to that scale, restoring the 340 px width.
#[tauri::command]
fn report_dpr(app: AppHandle, dpr: f64, w: f64, h: f64) {
    let Some(win) = app.get_webview_window("notch") else { return };
    let want = win
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or_else(|| win.scale_factor().unwrap_or(1.0));
    let mut z = ZOOM.lock().unwrap();
    let base = if *z > 0.0 { dpr / *z } else { dpr };
    let target = if base > 0.0 { want / base } else { 1.0 };
    applog(&format!(
        "dpr report: dpr={dpr:.3} viewport={w:.0}x{h:.0} monitor_scale={want:.3} zoom_applied={:.3} -> target_zoom={target:.3}",
        *z
    ));
    // Oscillation guard: at most three corrections per process (if the DPR does not follow the zoom, stop chasing it)
    static APPLIED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if (dpr - want).abs() > 0.02
        && (target - *z).abs() > 0.01
        && (0.25..=4.0).contains(&target)
        && APPLIED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 3
    {
        match win.set_zoom(target) {
            Ok(()) => {
                *z = target;
                applog(&format!("dpr correction: set_zoom({target:.3}) ok"));
            }
            Err(e) => applog(&format!("dpr correction failed: {e}")),
        }
    }
}

/// Slack around every hot rectangle: this is sampled on a timer, so a cursor arriving at the pill
/// has to count as arrived slightly early, or a quick click lands between two polls while the
/// window is still click-through and goes to whatever is behind it.
const HOT_PAD: f64 = 10.0;

/// Is the cursor on something the window is there for? `window` is the outer size in physical
/// pixels, or None when it could not be read.
fn cursor_in_hot(rects: &[[f64; 4]], lx: f64, ly: f64, window: Option<(f64, f64)>) -> bool {
    if rects.is_empty() {
        return false;
    }
    let in_window = window
        .map(|(w, h)| lx >= 0.0 && ly >= 0.0 && lx < w && ly < h)
        .unwrap_or(true);
    if !in_window {
        return false;
    }
    if rects.iter().any(|r| {
        lx >= r[0] - HOT_PAD
            && ly >= r[1] - HOT_PAD
            && lx < r[0] + r[2] + HOT_PAD
            && ly < r[1] + r[3] + HOT_PAD
    }) {
        return true;
    }
    // The gap between hot rectangles (pill and card/menu) counts as inside: use the bounding box of all of them with padding
    if rects.len() > 1 {
        let x0 = rects.iter().map(|r| r[0]).fold(f64::MAX, f64::min);
        let y0 = rects.iter().map(|r| r[1]).fold(f64::MAX, f64::min);
        let x1 = rects.iter().map(|r| r[0] + r[2]).fold(f64::MIN, f64::max);
        let y1 = rects.iter().map(|r| r[1] + r[3]).fold(f64::MIN, f64::max);
        return lx >= x0 - HOT_PAD && ly >= y0 - HOT_PAD && lx < x1 + HOT_PAD && ly < y1 + HOT_PAD;
    }
    false
}

/// Was 150 ms, when this only decided whether the card stayed up. It now also gates whether a click
/// reaches the notch, and at 150 ms a click arriving in the wrong sample went to the window behind.
const WATCHDOG_MS: u64 = 50;
/// Kept at the original 300 ms rather than falling out of the faster poll, which would make the
/// card twitchy.
const LEAVE_MS: u64 = 300;

/// WebView2's mouseleave is unreliable inside a NOACTIVATE transparent window — a cursor that
/// leaves quickly often produces no WM_MOUSELEAVE, and the card stays up. Rather than trust DOM
/// events, the Rust side watches the system cursor and emits pointer_left once it is outside; the
/// page collapses after its 250 ms grace period. "Outside the window" is not the test, though: the
/// window is mostly transparent, so the cursor is compared against the hot rectangles the page
/// reports (pill, card, and the gap between them).
///
/// It also gates click-through (#106), which is why it runs whether or not the card is open. That
/// ordering is load-bearing: the window ignores the cursor while it is click-through, so the page
/// gets no mousemove out there and cannot see the pointer arriving. This loop does, and hands the
/// window its input back in time for the page to open the card.
fn start_pointer_watchdog(app: AppHandle) {
    std::thread::spawn(move || {
        let need = (LEAVE_MS / WATCHDOG_MS).max(1) as u8;
        let mut miss = 0u8;
        // Last value pushed: this changes only when the cursor crosses an edge
        let mut click_through: Option<bool> = None;
        let mut was_inside = false;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(WATCHDOG_MS));
            let Some(w) = app.get_webview_window("notch") else { continue };
            let (Ok(pos), Ok(cur)) = (w.outer_position(), app.cursor_position()) else { continue };
            let rects = HOT.lock().unwrap().clone();
            // Cursor position relative to the window's top-left, in physical pixels; the hot rectangles are physical too, so no scale conversion
            let lx = cur.x - pos.x as f64;
            let ly = cur.y - pos.y as f64;
            let size = w.outer_size().ok().map(|s| (s.width as f64, s.height as f64));
            let inside = cursor_in_hot(&rects, lx, ly, size);

            if click_through != Some(!inside) {
                set_click_through(&app, !inside);
                click_through = Some(!inside);
                applog(&format!(
                    "click-through {} at cursor_rel=({lx:.0},{ly:.0}) rects={rects:?}",
                    if inside { "off (cursor on the notch)" } else { "on (cursor elsewhere)" }
                ));
            }

            static LOGGED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 12 {
                applog(&format!(
                    "watchdog: cursor_rel=({lx:.0},{ly:.0}) inside={inside} rects={rects:?} winpos=({},{})",
                    pos.x, pos.y
                ));
            }

            if inside {
                was_inside = true;
                miss = 0;
            } else {
                if any_mouse_button_down() && (was_inside || EXPANDED.load(std::sync::atomic::Ordering::Relaxed)) {
                    was_inside = false;
                    miss = 0;
                    EXPANDED.store(false, std::sync::atomic::Ordering::Relaxed);
                    let _ = app.emit("pointer_left", ());
                } else if was_inside || EXPANDED.load(std::sync::atomic::Ordering::Relaxed) {
                    miss += 1;
                    if miss >= need {
                        miss = 0;
                        was_inside = false;
                        EXPANDED.store(false, std::sync::atomic::Ordering::Relaxed);
                        let _ = app.emit("pointer_left", ());
                    }
                }
            }
        }
    });
}

/// Log channel for the page: JS writes key diagnostics into run.log (if invoke itself fails, the page reports on screen instead)
#[tauri::command]
fn log_js(msg: String) {
    applog(&format!("js: {}", msg.chars().take(600).collect::<String>()));
}

#[tauri::command]
fn open_usage_page() {
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/C", "start", "", "https://claude.ai/settings/usage"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let _ = cmd.spawn();
}

#[tauri::command]
fn focus_session(app: AppHandle, id: String) -> bool {
    let ppid = {
        let st = app.state::<AppState>();
        let store = st.store.lock().unwrap();
        store.ppid_of(&id)
    };
    match ppid {
        Some(p) => focus::focus_terminal(p),
        None => focus::focus_claude_desktop(),
    }
}

#[tauri::command]
fn dismiss_session(app: AppHandle, id: String) {
    {
        let st = app.state::<AppState>();
        let mut store = st.store.lock().unwrap();
        store.dismiss(&id);
    }
    broadcast(&app);
}

#[tauri::command]
fn set_lang(app: AppHandle, lang: String) {
    apply_lang(&app, &lang);
}

// ---------------- notch size ----------------

#[tauri::command]
fn get_scale(app: AppHandle) -> f64 {
    ui_scale(&app)
}

/// Called by the slider on every move. Only the value is stored here: the page scales the pill
/// itself with a CSS zoom, so the window is never resized and the hover card holding the slider
/// keeps its size — otherwise the slider would shrink away from under the cursor mid-drag.
#[tauri::command]
fn set_scale(app: AppHandle, scale: f64) {
    let value = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.scale = scale.clamp(config::SCALE_MIN, config::SCALE_MAX);
        config::save(&c);
        c.scale
    };
    // The notch draws its own size, so it has to be told. Without this the slider in the settings
    // window saved the value but nothing changed on screen until the app was restarted.
    let _ = app.emit("scale", value);
}

// ---------------- tray icon readings ----------------

/// The headline percentage of a reading: its fullest window, as a whole number 0-100.
///
/// Deliberately the same rule as `headlineOf` in ui/notch.html, so the tray icon and the ring can
/// never disagree: consider only metered windows — a `count` window (Antigravity's requests today,
/// say) has no published denominator, so its `used` is not a share of anything and averaging or
/// maximising over it would invent a number.
fn headline_pct(s: &usage::UsageSnapshot) -> Option<u32> {
    let top = s
        .windows
        .iter()
        .filter(|w| w.count.is_none())
        .map(|w| w.used)
        .fold(f64::NAN, f64::max);
    if top.is_nan() {
        return None;
    }
    Some((top * 100.0).round().clamp(0.0, 100.0) as u32)
}

fn find_in_path(exe: &str) -> Option<std::path::PathBuf> {
    if let Ok(paths) = std::env::var("PATH") {
        for p in std::env::split_paths(&paths) {
            let full = p.join(exe);
            if full.is_file() {
                return Some(full);
            }
        }
    }
    None
}

/// Ids match the ones the page uses, so the tray, the settings window and the notch all agree.
fn snapshot_of(app: &AppHandle, id: &str) -> usage::UsageSnapshot {
    let st = app.state::<AppState>();
    match id {
        "codex" => st.codex.lock().unwrap().clone(),
        "cursor" => st.cursor.lock().unwrap().clone(),
        "gemini" => st.antigravity.lock().unwrap().clone(),
        "opencode" => {
            let home = dirs::home_dir().unwrap_or_default();
            let local = dirs::data_local_dir().unwrap_or_default();
            let p_exe = find_in_path("opencode.exe").or_else(|| {
                let p = local.join("Programs").join("opencode").join("opencode.exe");
                if p.is_file() { Some(p) } else { None }
            });
            if p_exe.is_some() || home.join(".opencode").exists() {
                usage::UsageSnapshot {
                    status: "ok".into(),
                    note: "OpenCode agent active".into(),
                    ..Default::default()
                }
            } else {
                usage::UsageSnapshot {
                    status: "standby".into(),
                    note: "Open-source terminal AI agent".into(),
                    ..Default::default()
                }
            }
        }
        "deepseek" => {
            let has_key = std::env::var("DEEPSEEK_API_KEY").is_ok();
            let home = dirs::home_dir().unwrap_or_default();
            if has_key || home.join(".deepseek").exists() {
                usage::UsageSnapshot {
                    status: "ok".into(),
                    note: "DeepSeek API active".into(),
                    ..Default::default()
                }
            } else {
                usage::UsageSnapshot {
                    status: "standby".into(),
                    note: "DeepSeek Coder & R1 reasoning model".into(),
                    ..Default::default()
                }
            }
        }
        "zed" => {
            let appdata = dirs::data_dir().unwrap_or_default();
            let local = dirs::data_local_dir().unwrap_or_default();
            let p_exe = find_in_path("zed.exe").or_else(|| {
                let p = local.join("Programs").join("Zed").join("Zed.exe");
                if p.is_file() { Some(p) } else { None }
            });
            if p_exe.is_some() || appdata.join("Zed").exists() {
                usage::UsageSnapshot {
                    status: "ok".into(),
                    note: "Zed AI editor active".into(),
                    ..Default::default()
                }
            } else {
                usage::UsageSnapshot {
                    status: "standby".into(),
                    note: "High-performance editor with Zed AI".into(),
                    ..Default::default()
                }
            }
        }
        "ollama" => {
            let home = dirs::home_dir().unwrap_or_default();
            let local = dirs::data_local_dir().unwrap_or_default();
            let p_exe = find_in_path("ollama.exe").or_else(|| {
                let p = local.join("Programs").join("Ollama").join("ollama app.exe");
                if p.is_file() { Some(p) } else { None }
            });
            if p_exe.is_some() || home.join(".ollama").exists() {
                usage::UsageSnapshot {
                    status: "ok".into(),
                    note: "Ollama local runtime detected".into(),
                    ..Default::default()
                }
            } else {
                usage::UsageSnapshot {
                    status: "standby".into(),
                    note: "Local LLM model inference runner".into(),
                    ..Default::default()
                }
            }
        }
        "gemini_cli" => {
            let home = dirs::home_dir().unwrap_or_default();
            let appdata = dirs::data_dir().unwrap_or_default();
            let p_exe = find_in_path("gemini.exe")
                .or_else(|| find_in_path("gemini.cmd"))
                .or_else(|| {
                    let p = appdata.join("npm").join("gemini.cmd");
                    if p.is_file() { Some(p) } else { None }
                });
            if p_exe.is_some() || home.join(".gemini").exists() || std::env::var("GEMINI_API_KEY").is_ok() {
                usage::UsageSnapshot {
                    status: "ok".into(),
                    note: "Google Gemini CLI active".into(),
                    ..Default::default()
                }
            } else {
                usage::UsageSnapshot {
                    status: "standby".into(),
                    note: "Google Gemini developer CLI".into(),
                    ..Default::default()
                }
            }
        }
        "aider" => {
            let p_exe = find_in_path("aider.exe").or_else(|| find_in_path("aider.cmd"));
            if p_exe.is_some() {
                usage::UsageSnapshot {
                    status: "ok".into(),
                    note: "Aider terminal AI active".into(),
                    ..Default::default()
                }
            } else {
                usage::UsageSnapshot {
                    status: "standby".into(),
                    note: "AI pair programming terminal agent".into(),
                    ..Default::default()
                }
            }
        }
        _ => st.usage.lock().unwrap().clone(),
    }
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct DetectedProvider {
    pub id: String,
    pub name: String,
    pub category: String, // "agent" | "editor" | "model" | "runtime"
    pub installed: bool,
    pub running: bool,
    pub path: Option<String>,
    pub status: String,
    pub detail: String,
}

#[tauri::command]
fn detect_installed_providers(_app: AppHandle) -> Vec<DetectedProvider> {
    let procs = focus::proc_maps();
    let is_proc_running = |name_prefix: &str| -> bool {
        procs.name.values().any(|n| n.starts_with(name_prefix))
    };

    let home = dirs::home_dir().unwrap_or_default();
    let local = dirs::data_local_dir().unwrap_or_default();
    let appdata = dirs::data_dir().unwrap_or_default();

    let mut res = Vec::new();

    // 1. Claude Code
    let claude_running = is_proc_running("claude") || is_proc_running("anthropicclaude");
    let claude_path = find_in_path("claude.exe")
        .or_else(|| {
            let p = local.join("Programs").join("Claude").join("Claude.exe");
            if p.is_file() { Some(p) } else { None }
        })
        .or_else(|| {
            let p = home.join(".claude");
            if p.exists() { Some(p) } else { None }
        });
    let claude_installed = claude_path.is_some() || claude_running;
    res.push(DetectedProvider {
        id: "claude".into(),
        name: "Claude Code".into(),
        category: "agent".into(),
        installed: claude_installed,
        running: claude_running,
        path: claude_path.map(|p| p.to_string_lossy().to_string()),
        status: if claude_running { "ok".into() } else if claude_installed { "standby".into() } else { "absent".into() },
        detail: "Anthropic Claude Code CLI & Desktop".into(),
    });

    // 2. Codex
    let codex_running = is_proc_running("codex") || is_proc_running("chatgpt");
    let codex_path = find_in_path("codex.exe")
        .or_else(|| crate::codex::find_executable())
        .or_else(|| {
            let p = home.join(".codex");
            if p.exists() { Some(p) } else { None }
        });
    let codex_installed = codex_path.is_some() || codex_running;
    res.push(DetectedProvider {
        id: "codex".into(),
        name: "Codex CLI".into(),
        category: "agent".into(),
        installed: codex_installed,
        running: codex_running,
        path: codex_path.map(|p| p.to_string_lossy().to_string()),
        status: if codex_running { "ok".into() } else if codex_installed { "standby".into() } else { "absent".into() },
        detail: "OpenAI Codex Agent & ChatGPT Client".into(),
    });

    // 3. Cursor
    let cursor_running = is_proc_running("cursor");
    let cursor_path = {
        let p = local.join("Programs").join("cursor").join("Cursor.exe");
        if p.is_file() { Some(p) } else {
            let p2 = appdata.join("Cursor");
            if p2.exists() { Some(p2) } else { None }
        }
    };
    let cursor_installed = cursor_path.is_some() || cursor_running;
    res.push(DetectedProvider {
        id: "cursor".into(),
        name: "Cursor".into(),
        category: "editor".into(),
        installed: cursor_installed,
        running: cursor_running,
        path: cursor_path.map(|p| p.to_string_lossy().to_string()),
        status: if cursor_running { "ok".into() } else if cursor_installed { "standby".into() } else { "absent".into() },
        detail: "Anysphere AI Code Editor".into(),
    });

    // 4. Antigravity
    let ag_running = is_proc_running("antigravity");
    let ag_path = {
        let p = local.join("Programs").join("Antigravity").join("Antigravity.exe");
        if p.is_file() { Some(p) } else {
            let p2 = home.join(".gemini");
            if p2.exists() { Some(p2) } else {
                let p3 = appdata.join("antigravity");
                if p3.exists() { Some(p3) } else { None }
            }
        }
    };
    let ag_installed = ag_path.is_some() || ag_running;
    res.push(DetectedProvider {
        id: "gemini".into(),
        name: "Antigravity".into(),
        category: "agent".into(),
        installed: ag_installed,
        running: ag_running,
        path: ag_path.map(|p| p.to_string_lossy().to_string()),
        status: if ag_running { "ok".into() } else if ag_installed { "standby".into() } else { "absent".into() },
        detail: "Google Gemini Advanced Agent".into(),
    });

    // 5. OpenCode
    let opencode_running = is_proc_running("opencode");
    let opencode_path = find_in_path("opencode.exe")
        .or_else(|| {
            let p = home.join(".opencode");
            if p.exists() { Some(p) } else { None }
        })
        .or_else(|| {
            let p = local.join("Programs").join("opencode").join("opencode.exe");
            if p.is_file() { Some(p) } else { None }
        });
    let opencode_installed = opencode_path.is_some() || opencode_running;
    res.push(DetectedProvider {
        id: "opencode".into(),
        name: "OpenCode".into(),
        category: "agent".into(),
        installed: opencode_installed,
        running: opencode_running,
        path: opencode_path.map(|p| p.to_string_lossy().to_string()),
        status: if opencode_running { "ok".into() } else if opencode_installed { "standby".into() } else { "absent".into() },
        detail: "Open-Source Terminal AI Agent".into(),
    });

    // 6. DeepSeek
    let has_deepseek_env = std::env::var("DEEPSEEK_API_KEY").is_ok();
    let deepseek_path = {
        let p = home.join(".deepseek");
        if p.exists() { Some(p) } else if has_deepseek_env {
            Some(std::path::PathBuf::from("Environment: DEEPSEEK_API_KEY"))
        } else { None }
    };
    let deepseek_installed = deepseek_path.is_some();
    res.push(DetectedProvider {
        id: "deepseek".into(),
        name: "DeepSeek".into(),
        category: "model".into(),
        installed: deepseek_installed,
        running: has_deepseek_env,
        path: deepseek_path.map(|p| p.to_string_lossy().to_string()),
        status: if has_deepseek_env { "ok".into() } else if deepseek_installed { "standby".into() } else { "standby".into() },
        detail: "DeepSeek Coder & R1 Reasoning Engine".into(),
    });

    // 7. Zed AI (zcode)
    let zed_running = is_proc_running("zed");
    let zed_path = find_in_path("zed.exe")
        .or_else(|| {
            let p = local.join("Programs").join("Zed").join("Zed.exe");
            if p.is_file() { Some(p) } else { None }
        })
        .or_else(|| {
            let p = appdata.join("Zed");
            if p.exists() { Some(p) } else { None }
        });
    let zed_installed = zed_path.is_some() || zed_running;
    res.push(DetectedProvider {
        id: "zed".into(),
        name: "Zed AI (zcode)".into(),
        category: "editor".into(),
        installed: zed_installed,
        running: zed_running,
        path: zed_path.map(|p| p.to_string_lossy().to_string()),
        status: if zed_running { "ok".into() } else if zed_installed { "standby".into() } else { "absent".into() },
        detail: "Zed High-Performance Multiplayer Editor".into(),
    });

    // 8. Ollama / Local Models
    let ollama_running = is_proc_running("ollama");
    let ollama_path = find_in_path("ollama.exe")
        .or_else(|| {
            let p = local.join("Programs").join("Ollama").join("ollama app.exe");
            if p.is_file() { Some(p) } else { None }
        })
        .or_else(|| {
            let p = home.join(".ollama");
            if p.exists() { Some(p) } else { None }
        });
    let ollama_installed = ollama_path.is_some() || ollama_running;
    res.push(DetectedProvider {
        id: "ollama".into(),
        name: "Ollama (Local AI)".into(),
        category: "runtime".into(),
        installed: ollama_installed,
        running: ollama_running,
        path: ollama_path.map(|p| p.to_string_lossy().to_string()),
        status: if ollama_running { "ok".into() } else if ollama_installed { "standby".into() } else { "absent".into() },
        detail: "Local LLM Inference Engine & Model Server".into(),
    });

    // 9. Gemini CLI
    let gemini_running = is_proc_running("gemini");
    let gemini_path = find_in_path("gemini.exe")
        .or_else(|| find_in_path("gemini.cmd"))
        .or_else(|| {
            let p = appdata.join("npm").join("gemini.cmd");
            if p.is_file() { Some(p) } else { None }
        })
        .or_else(|| {
            let p = home.join(".gemini");
            if p.exists() { Some(p) } else { None }
        });
    let gemini_installed = gemini_path.is_some() || gemini_running;
    res.push(DetectedProvider {
        id: "gemini_cli".into(),
        name: "Gemini CLI".into(),
        category: "agent".into(),
        installed: gemini_installed,
        running: gemini_running,
        path: gemini_path.map(|p| p.to_string_lossy().to_string()),
        status: if gemini_running { "ok".into() } else if gemini_installed { "standby".into() } else { "absent".into() },
        detail: "Google Gemini Developer CLI & Agent".into(),
    });

    // 10. Aider AI
    let aider_running = is_proc_running("aider");
    let aider_path = find_in_path("aider.exe").or_else(|| find_in_path("aider.cmd"));
    let aider_installed = aider_path.is_some() || aider_running;
    res.push(DetectedProvider {
        id: "aider".into(),
        name: "Aider AI".into(),
        category: "agent".into(),
        installed: aider_installed,
        running: aider_running,
        path: aider_path.map(|p| p.to_string_lossy().to_string()),
        status: if aider_running { "ok".into() } else if aider_installed { "standby".into() } else { "absent".into() },
        detail: "AI Pair Programming in Terminal".into(),
    });

    res
}

/// What one half of the icon should show. An empty (or "top") window id means "whichever of this
/// provider's windows is fullest", which is the notch's own rule and the only choice that survives
/// a provider adding or renaming its windows.
fn reading_for_slot(app: &AppHandle, slot: &config::TraySlot) -> Option<u32> {
    let snap = snapshot_of(app, &slot.provider);
    if snap.status == "absent" {
        return None;
    }
    if slot.window.is_empty() || slot.window == "top" {
        return headline_pct(&snap);
    }
    // A pinned window missing from this snapshot falls back to the fullest one, which is what the
    // notch does in the same situation. Without this a provider that renamed or dropped a window
    // would leave the icon showing a dash while the notch still showed a number.
    snap.windows
        .iter()
        .find(|w| w.id == slot.window && w.count.is_none())
        .map(|w| (w.used * 100.0).round().clamp(0.0, 100.0) as u32)
        .or_else(|| headline_pct(&snap))
}

/// One provider and the windows it currently reports, for the settings window's pickers. Built
/// from live readings rather than a hard-coded table, so a provider that gains a window shows it.
#[derive(serde::Serialize)]
struct TrayOption {
    id: String,
    label: String,
    status: String,
    windows: Vec<TrayWindowOption>,
}

#[derive(serde::Serialize)]
struct TrayWindowOption {
    id: String,
    label: String,
    used: Option<u32>,
}

#[tauri::command]
fn get_tray_options(app: AppHandle) -> Vec<TrayOption> {
    TRAY_PROVIDER_IDS
        .iter()
        .map(|id| {
            let snap = snapshot_of(&app, id);
            TrayOption {
                id: (*id).to_string(),
                label: provider_label(id).to_string(),
                status: snap.status.clone(),
                windows: snap
                    .windows
                    .iter()
                    .filter(|w| w.count.is_none()) // a count window has no percentage to draw
                    .map(|w| TrayWindowOption {
                        id: w.id.clone(),
                        label: w.label.clone(),
                        used: Some((w.used * 100.0).round().clamp(0.0, 100.0) as u32),
                    })
                    .collect(),
            }
        })
        .collect()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TrayConfig {
    mode: String,
    slots: Vec<config::TraySlot>,
}

#[tauri::command]
fn get_tray_config(app: AppHandle) -> TrayConfig {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    TrayConfig { mode: c.tray_mode.clone(), slots: c.tray_slots.clone() }
}

#[tauri::command]
fn set_tray_config(app: AppHandle, cfg: TrayConfig) {
    {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.tray_mode = cfg.mode;
        c.tray_slots = cfg.slots;
        // Kept in step so an older build reading this file still shows something sensible
        c.tray_providers = c.tray_slots.iter().map(|s| s.provider.clone()).collect();
        config::save(&c);
    }
    repaint_tray(&app);
    tray::refresh_menu(&app);
}

/// The real icon, as a picture, for the settings preview — so what is being edited cannot drift
/// from what the taskbar actually draws.
#[tauri::command]
fn get_tray_preview(app: AppHandle, cfg: TrayConfig) -> Option<String> {
    let values: Vec<Option<u32>> = cfg.slots.iter().map(|s| reading_for_slot(&app, s)).collect();
    let rgba = match cfg.mode.as_str() {
        "bars" if !values.is_empty() => trayicon::bars_rgba(&values),
        "numbers" if !values.is_empty() => trayicon::numbers_rgba(&values),
        _ => return None, // "off" shows the app's own mark, which the page draws itself
    };
    trayicon::to_data_url(&rgba)
}

/// What each ring on the notch shows: the provider, and which of its windows. An empty list means
/// every provider, each showing whichever window is fullest — the original behaviour.
#[tauri::command]
fn get_notch_slots(app: AppHandle) -> Vec<config::TraySlot> {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    c.notch_slots.clone()
}

#[tauri::command]
fn set_notch_slots(app: AppHandle, slots: Vec<config::TraySlot>) {
    let list = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.notch_slots = slots;
        // Kept in step so an older build reading this file still shows the right providers
        c.notch_providers = c.notch_slots.iter().map(|s| s.provider.clone()).collect();
        config::save(&c);
        c.notch_slots.clone()
    };
    // The notch is a separate window and draws its own cells, so it has to be told.
    let _ = app.emit("notch_slots", list);
}

/// The application's own icon, so the settings window shows what the taskbar shows.
#[tauri::command]
fn get_app_icon() -> Option<String> {
    trayicon::app_mark_data_url()
}

// ---------------- what is on screen at all ----------------

#[derive(serde::Serialize)]
struct UiFlags {
    notch_visible: bool,
    tray_visible: bool,
}

#[tauri::command]
fn get_ui_flags(app: AppHandle) -> UiFlags {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    UiFlags { notch_visible: c.notch_visible, tray_visible: c.tray_visible }
}

/// Hiding both would leave the app running with nothing to click, so the tray icon is kept
/// whenever the notch is off. The answer says what was actually stored, so the settings window can
/// show the corrected state rather than a lie.
#[tauri::command]
fn set_ui_flags(app: AppHandle, notch_visible: bool, tray_visible: bool) -> UiFlags {
    let flags = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.notch_visible = notch_visible;
        c.tray_visible = if notch_visible { tray_visible } else { true };
        config::save(&c);
        UiFlags { notch_visible: c.notch_visible, tray_visible: c.tray_visible }
    };
    apply_visibility(&app);
    flags
}

/// Puts the two switches into effect.
pub fn apply_visibility(app: &AppHandle) {
    let (notch, tray_on) = {
        let st = app.state::<AppState>();
        let c = st.cfg.lock().unwrap();
        (c.notch_visible, c.tray_visible)
    };
    if let Some(w) = app.get_webview_window("notch") {
        if notch {
            let _ = w.show();
            place_notch(app);
        } else {
            let _ = w.hide();
        }
    }
    if let Some(t) = app.tray_by_id("main") {
        let _ = t.set_visible(tray_on);
    }
}

// ---------------- settings that used to live in the tray menu ----------------

#[tauri::command]
fn get_lang(app: AppHandle) -> String {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    c.lang.clone()
}

/// The settings WebView must use the same Windows locale as the tray. WebView2's
/// navigator.language can describe the browser runtime rather than the user locale.
#[tauri::command]
fn get_lang_resolved(app: AppHandle) -> String {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    resolved_lang(&c.lang)
}

#[tauri::command]
fn get_autostart() -> bool {
    autostart::is_enabled()
}

#[tauri::command]
fn set_autostart(on: bool) -> Result<String, String> {
    if on {
        autostart::enable()
    } else {
        autostart::disable()
    }
}

#[tauri::command]
fn get_hooks_installed() -> bool {
    hooks_install::is_installed()
}

#[tauri::command]
fn set_hooks_installed(on: bool) -> Result<String, String> {
    if on {
        hooks_install::install()
    } else {
        hooks_install::uninstall()
    }
}

#[tauri::command]
fn reset_notch_position(app: AppHandle) {
    reset_bar(&app);
}

#[cfg(windows)]
pub fn trim_memory() {
    use std::collections::HashSet;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, OpenProcess, SetProcessWorkingSetSize,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA,
    };

    unsafe {
        // Trim host process working set
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);

        // Also trim all descendant processes recursively (WebView2 browser, GPU process, renderers, utilities)
        let my_pid = GetCurrentProcessId();
        if let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            let mut proc_list: Vec<(u32, u32)> = Vec::new(); // (pid, parent_pid)
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            if Process32FirstW(snap, &mut entry).is_ok() {
                loop {
                    proc_list.push((entry.th32ProcessID, entry.th32ParentProcessID));
                    if Process32NextW(snap, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snap);

            // Traverse the tree to find all descendants (children, grandchildren, etc.)
            let mut descendants: HashSet<u32> = HashSet::new();
            let mut search_parents: HashSet<u32> = HashSet::new();
            search_parents.insert(my_pid);

            loop {
                let mut found_new = false;
                for &(pid, ppid) in &proc_list {
                    if search_parents.contains(&ppid) && !descendants.contains(&pid) {
                        descendants.insert(pid);
                        found_new = true;
                    }
                }
                if !found_new {
                    break;
                }
                search_parents.extend(&descendants);
            }

            for pid in descendants {
                if let Ok(h) = OpenProcess(PROCESS_SET_QUOTA | PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                    let _ = SetProcessWorkingSetSize(h, usize::MAX, usize::MAX);
                    let _ = CloseHandle(h);
                }
            }
        }
    }
}

#[cfg(not(windows))]
pub fn trim_memory() {}

pub fn spawn_thread<F>(name: &'static str, f: F) -> std::thread::JoinHandle<()>
where
    F: FnOnce() + Send + 'static,
{
    std::thread::Builder::new()
        .name(name.into())
        .stack_size(256 * 1024)
        .spawn(f)
        .expect("failed to spawn thread")
}

pub fn open_settings_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        #[cfg(windows)]
        if let Ok(h) = w.hwnd() {
            let hwnd = windows::Win32::Foundation::HWND(h.0 as isize as *mut core::ffi::c_void);
            win_backdrop::set_dark_mode(hwnd, true);
        }
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    } else {
        let app_handle = app.clone();
        std::thread::spawn(move || {
            if let Ok(w) = tauri::WebviewWindowBuilder::new(
                &app_handle,
                "settings",
                tauri::WebviewUrl::App("settings.html".into()),
            )
            .title("Codenotch Settings")
            .inner_size(880.0, 650.0)
            .min_inner_size(540.0, 520.0)
            .resizable(true)
            .skip_taskbar(false)
            .visible(true)
            .build()
            {
                #[cfg(windows)]
                if let Ok(h) = w.hwnd() {
                    let hwnd = windows::Win32::Foundation::HWND(h.0 as isize as *mut core::ffi::c_void);
                    win_backdrop::set_dark_mode(hwnd, true);
                }
                let _ = w.show();
                let _ = w.set_focus();
            }
        });
    }
}

#[tauri::command]
fn open_settings(app: AppHandle) {
    open_settings_window(&app);
}

#[tauri::command]
fn set_settings_dark_mode(app: AppHandle, dark: bool) {
    #[cfg(windows)]
    if let Some(w) = app.get_webview_window("settings") {
        if let Ok(h) = w.hwnd() {
            let hwnd = windows::Win32::Foundation::HWND(h.0 as isize as *mut core::ffi::c_void);
            win_backdrop::set_dark_mode(hwnd, dark);
        }
    }
}

#[tauri::command]
fn get_system_metrics() -> hardware::SystemMetrics {
    hardware::poll_system_metrics()
}

#[tauri::command]
fn toggle_overlay(app: AppHandle, show: Option<bool>) -> bool {
    if let Some(w) = app.get_webview_window("overlay") {
        let is_vis = w.is_visible().unwrap_or(false);
        let target = show.unwrap_or(!is_vis);
        if target {
            let _ = w.show();
            let _ = w.set_focus();
        } else {
            let _ = w.hide();
        }
        {
            let st = app.state::<AppState>();
            let mut c = st.cfg.lock().unwrap();
            c.overlay_enabled = target;
            config::save(&c);
        }
        let _ = app.emit("overlay_visibility", target);
        target
    } else {
        false
    }
}

#[tauri::command]
fn set_overlay_clickthrough(app: AppHandle, clickthrough: bool) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("overlay") {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::UI::WindowsAndMessaging::{
                GetWindowLongW, SetWindowLongW, GWL_EXSTYLE, WS_EX_LAYERED, WS_EX_TRANSPARENT,
            };
            if let Ok(h) = w.hwnd() {
                let hwnd = HWND(h.0 as isize as *mut core::ffi::c_void);
                unsafe {
                    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
                    let new_ex = if clickthrough {
                        ex | (WS_EX_TRANSPARENT.0 as i32) | (WS_EX_LAYERED.0 as i32)
                    } else {
                        ex & !(WS_EX_TRANSPARENT.0 as i32)
                    };
                    let _ = SetWindowLongW(hwnd, GWL_EXSTYLE, new_ex);
                }
            }
        }
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.overlay_clickthrough = clickthrough;
        config::save(&c);
        let _ = app.emit("overlay_clickthrough_state", clickthrough);
    }
    Ok(())
}

#[tauri::command]
fn set_overlay_opacity(app: AppHandle, opacity: f64) {
    let st = app.state::<AppState>();
    let mut c = st.cfg.lock().unwrap();
    c.overlay_opacity = opacity.clamp(0.1, 1.0);
    config::save(&c);
}

#[tauri::command]
fn save_overlay_position(app: AppHandle, x: i32, y: i32) {
    let st = app.state::<AppState>();
    let mut c = st.cfg.lock().unwrap();
    c.overlay_x = Some(x);
    c.overlay_y = Some(y);
    config::save(&c);
}

#[tauri::command]
fn resize_overlay_window(app: AppHandle, width: u32, height: u32) {
    if let Some(ow) = app.get_webview_window("overlay") {
        let scale = ow.scale_factor().unwrap_or(1.0);
        let target = tauri::PhysicalSize::new(
            (width as f64 * scale).round() as u32,
            (height as f64 * scale).round() as u32,
        );
        let (dock, mx, my) = {
            let st = app.state::<AppState>();
            let c = st.cfg.lock().unwrap();
            (c.overlay_dock.clone(), c.overlay_margin_x, c.overlay_margin_y)
        };
        let _ = ow.set_size(target);
        if dock != "custom" {
            snap_overlay_position(app.clone(), dock, Some(mx), Some(my));
        }
    }
}

#[tauri::command]
fn move_overlay_by(app: AppHandle, dx: i32, dy: i32) {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{HWND, RECT};
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowRect, SetWindowPos, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
        };
        if let Some(w) = app.get_webview_window("overlay") {
            if let Ok(h) = w.hwnd() {
                let hwnd = HWND(h.0 as isize as *mut core::ffi::c_void);
                unsafe {
                    let mut rect = RECT::default();
                    if GetWindowRect(hwnd, &mut rect).is_ok() {
                        let new_x = rect.left + dx;
                        let new_y = rect.top + dy;
                        let _ = SetWindowPos(
                            hwnd,
                            HWND(std::ptr::null_mut()),
                            new_x,
                            new_y,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                }
            }
        }
    }
}

#[tauri::command]
fn start_overlay_drag(app: AppHandle) {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{HWND, WPARAM, LPARAM};
        use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
        use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_NCLBUTTONDOWN, HTCAPTION};
        if let Some(w) = app.get_webview_window("overlay") {
            if let Ok(h) = w.hwnd() {
                let hwnd = HWND(h.0 as isize as *mut core::ffi::c_void);
                unsafe {
                    let _ = ReleaseCapture();
                    SendMessageW(hwnd, WM_NCLBUTTONDOWN, WPARAM(HTCAPTION as usize), LPARAM(0));
                }
            }
        }
    }
}

#[tauri::command]
fn snap_overlay_position(app: AppHandle, corner: String, margin_x: Option<i32>, margin_y: Option<i32>) {
    if let Some(ow) = app.get_webview_window("overlay") {
        if let Ok(Some(mon)) = ow.primary_monitor() {
            let mon_pos = mon.position();
            let mon_size = mon.size();
            let win_size = ow.outer_size().unwrap_or(tauri::PhysicalSize::new(260, 240));

            let (mx, my) = {
                let st = app.state::<AppState>();
                let c = st.cfg.lock().unwrap();
                (
                    margin_x.unwrap_or(c.overlay_margin_x).max(0),
                    margin_y.unwrap_or(c.overlay_margin_y).max(0),
                )
            };

            let corner_lower = corner.to_lowercase();
            let (target_x, target_y) = match corner_lower.as_str() {
                "top-left" => (mon_pos.x + mx, mon_pos.y + my),
                "top-right" => (
                    mon_pos.x + (mon_size.width as i32) - (win_size.width as i32) - mx,
                    mon_pos.y + my,
                ),
                "bottom-left" => (
                    mon_pos.x + mx,
                    mon_pos.y + (mon_size.height as i32) - (win_size.height as i32) - my,
                ),
                "bottom-right" => (
                    mon_pos.x + (mon_size.width as i32) - (win_size.width as i32) - mx,
                    mon_pos.y + (mon_size.height as i32) - (win_size.height as i32) - my,
                ),
                "center" => (
                    mon_pos.x + ((mon_size.width as i32 - win_size.width as i32) / 2),
                    mon_pos.y + ((mon_size.height as i32 - win_size.height as i32) / 2),
                ),
                _ => (mon_pos.x + mx, mon_pos.y + my),
            };

            let _ = ow.set_position(tauri::PhysicalPosition::new(target_x, target_y));
            let st = app.state::<AppState>();
            let mut c = st.cfg.lock().unwrap();
            c.overlay_dock = corner_lower.clone();
            c.overlay_margin_x = mx;
            c.overlay_margin_y = my;
            c.overlay_x = Some(target_x);
            c.overlay_y = Some(target_y);
            config::save(&c);
            let _ = app.emit("overlay_dock_changed", serde_json::json!({
                "dock": corner_lower,
                "margin_x": mx,
                "margin_y": my,
                "x": target_x,
                "y": target_y
            }));
        }
    }
}

#[tauri::command]
fn open_welcome_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window("welcome") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[tauri::command]
fn finish_welcome(app: AppHandle, theme: Option<String>, lang: Option<String>) {
    let st = app.state::<AppState>();
    {
        let mut c = st.cfg.lock().unwrap();
        c.first_run_completed = true;
        if let Some(ref th) = theme {
            c.appearance_style = th.clone();
        }
        if let Some(ref l) = lang {
            c.lang = l.clone();
        }
        config::save(&c);
    }
    if let Some(ref th) = theme {
        apply_backdrop(&app, "notch", th);
    }
    if let Some(w) = app.get_webview_window("welcome") {
        let _ = w.hide();
    }
    if let Some(nw) = app.get_webview_window("notch") {
        let _ = nw.show();
    }
    broadcast(&app);
    repaint_tray(&app);
}

#[tauri::command]
fn get_hardware_config(app: AppHandle) -> serde_json::Value {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    serde_json::json!({
        "hardware_enabled": c.hardware_enabled,
        "overlay_enabled": c.overlay_enabled,
        "overlay_opacity": c.overlay_opacity,
        "overlay_clickthrough": c.overlay_clickthrough,
        "tray_display_mode": c.tray_display_mode,
        "hotkey_overlay": c.hotkey_overlay,
        "hotkey_lock": c.hotkey_lock,
        "overlay_font_size": c.overlay_font_size,
        "overlay_theme": c.overlay_theme,
        "overlay_bg_opacity": c.overlay_bg_opacity,
        "overlay_dock": c.overlay_dock,
        "overlay_margin_x": c.overlay_margin_x,
        "overlay_margin_y": c.overlay_margin_y,
        "overlay_show_fps": c.overlay_show_fps,
        "overlay_show_low": c.overlay_show_low,
        "overlay_show_gpu": c.overlay_show_gpu,
        "overlay_show_vram": c.overlay_show_vram,
        "overlay_show_cpu": c.overlay_show_cpu,
        "overlay_show_ram": c.overlay_show_ram,
        "overlay_show_page": c.overlay_show_page,
        "overlay_show_net": c.overlay_show_net,
        "overlay_show_day": c.overlay_show_day,
        "overlay_show_app": c.overlay_show_app,
    })
}

#[tauri::command]
fn set_hardware_config(
    app: AppHandle,
    enabled: bool,
    tray_display_mode: String,
    overlay_opacity: f64,
    hotkey_overlay: Option<String>,
    hotkey_lock: Option<String>,
    overlay_font_size: Option<u32>,
    overlay_theme: Option<String>,
    overlay_bg_opacity: Option<f64>,
    overlay_dock: Option<String>,
    overlay_margin_x: Option<i32>,
    overlay_margin_y: Option<i32>,
    overlay_show_fps: Option<bool>,
    overlay_show_low: Option<bool>,
    overlay_show_gpu: Option<bool>,
    overlay_show_vram: Option<bool>,
    overlay_show_cpu: Option<bool>,
    overlay_show_ram: Option<bool>,
    overlay_show_page: Option<bool>,
    overlay_show_net: Option<bool>,
    overlay_show_day: Option<bool>,
    overlay_show_app: Option<bool>,
) {
    let st = app.state::<AppState>();
    let mut c = st.cfg.lock().unwrap();
    c.hardware_enabled = enabled;
    c.tray_display_mode = tray_display_mode;
    c.overlay_opacity = overlay_opacity.clamp(0.1, 1.0);
    if let Some(hk) = hotkey_overlay {
        if !hk.trim().is_empty() {
            c.hotkey_overlay = hk.trim().to_string();
        }
    }
    if let Some(hl) = hotkey_lock {
        if !hl.trim().is_empty() {
            c.hotkey_lock = hl.trim().to_string();
        }
    }
    if let Some(fs) = overlay_font_size {
        c.overlay_font_size = fs.clamp(9, 36);
    }
    if let Some(th) = overlay_theme {
        c.overlay_theme = th;
    }
    if let Some(bg) = overlay_bg_opacity {
        c.overlay_bg_opacity = bg.clamp(0.0, 0.8);
    }
    if let Some(dk) = overlay_dock {
        c.overlay_dock = dk;
    }
    if let Some(mx) = overlay_margin_x {
        c.overlay_margin_x = mx.clamp(0, 200);
    }
    if let Some(my) = overlay_margin_y {
        c.overlay_margin_y = my.clamp(0, 200);
    }
    if let Some(v) = overlay_show_fps { c.overlay_show_fps = v; }
    if let Some(v) = overlay_show_low { c.overlay_show_low = v; }
    if let Some(v) = overlay_show_gpu { c.overlay_show_gpu = v; }
    if let Some(v) = overlay_show_vram { c.overlay_show_vram = v; }
    if let Some(v) = overlay_show_cpu { c.overlay_show_cpu = v; }
    if let Some(v) = overlay_show_ram { c.overlay_show_ram = v; }
    if let Some(v) = overlay_show_page { c.overlay_show_page = v; }
    if let Some(v) = overlay_show_net { c.overlay_show_net = v; }
    if let Some(v) = overlay_show_day { c.overlay_show_day = v; }
    if let Some(v) = overlay_show_app { c.overlay_show_app = v; }

    config::save(&c);

    let snapshot = serde_json::json!({
        "overlay_font_size": c.overlay_font_size,
        "overlay_theme": c.overlay_theme,
        "overlay_bg_opacity": c.overlay_bg_opacity,
        "overlay_dock": c.overlay_dock,
        "overlay_margin_x": c.overlay_margin_x,
        "overlay_margin_y": c.overlay_margin_y,
        "overlay_show_fps": c.overlay_show_fps,
        "overlay_show_low": c.overlay_show_low,
        "overlay_show_gpu": c.overlay_show_gpu,
        "overlay_show_vram": c.overlay_show_vram,
        "overlay_show_cpu": c.overlay_show_cpu,
        "overlay_show_ram": c.overlay_show_ram,
        "overlay_show_page": c.overlay_show_page,
        "overlay_show_net": c.overlay_show_net,
        "overlay_show_day": c.overlay_show_day,
        "overlay_show_app": c.overlay_show_app,
    });
    drop(c);

    let _ = app.emit("overlay_config_changed", snapshot);
    notify_hotkey_change();
    repaint_tray(&app);
}

#[tauri::command]
fn save_overlay_metrics(app: AppHandle, config: serde_json::Value) {
    let st = app.state::<AppState>();
    let mut c = st.cfg.lock().unwrap();
    if let Some(fs) = config.get("fontSize").and_then(|v| v.as_u64()) {
        c.overlay_font_size = (fs as u32).clamp(9, 36);
    }
    if let Some(th) = config.get("theme").and_then(|v| v.as_str()) {
        c.overlay_theme = th.to_string();
    }
    if let Some(bg) = config.get("bgOpacity").and_then(|v| v.as_f64()) {
        c.overlay_bg_opacity = bg.clamp(0.0, 0.8);
    }
    if let Some(dk) = config.get("dock").and_then(|v| v.as_str()) {
        c.overlay_dock = dk.to_string();
    }
    if let Some(mx) = config.get("margin_x").and_then(|v| v.as_i64()) {
        c.overlay_margin_x = (mx as i32).clamp(0, 200);
    }
    if let Some(my) = config.get("margin_y").and_then(|v| v.as_i64()) {
        c.overlay_margin_y = (my as i32).clamp(0, 200);
    }
    if let Some(v) = config.get("show_fps").and_then(|v| v.as_bool()) { c.overlay_show_fps = v; }
    if let Some(v) = config.get("show_low").and_then(|v| v.as_bool()) { c.overlay_show_low = v; }
    if let Some(v) = config.get("show_gpu").and_then(|v| v.as_bool()) { c.overlay_show_gpu = v; }
    if let Some(v) = config.get("show_vram").and_then(|v| v.as_bool()) { c.overlay_show_vram = v; }
    if let Some(v) = config.get("show_cpu").and_then(|v| v.as_bool()) { c.overlay_show_cpu = v; }
    if let Some(v) = config.get("show_ram").and_then(|v| v.as_bool()) { c.overlay_show_ram = v; }
    if let Some(v) = config.get("show_page").and_then(|v| v.as_bool()) { c.overlay_show_page = v; }
    if let Some(v) = config.get("show_net").and_then(|v| v.as_bool()) { c.overlay_show_net = v; }
    if let Some(v) = config.get("show_day").and_then(|v| v.as_bool()) { c.overlay_show_day = v; }
    if let Some(v) = config.get("show_app").and_then(|v| v.as_bool()) { c.overlay_show_app = v; }

    config::save(&c);

    let snapshot = serde_json::json!({
        "overlay_font_size": c.overlay_font_size,
        "overlay_theme": c.overlay_theme,
        "overlay_bg_opacity": c.overlay_bg_opacity,
        "overlay_dock": c.overlay_dock,
        "overlay_margin_x": c.overlay_margin_x,
        "overlay_margin_y": c.overlay_margin_y,
        "overlay_show_fps": c.overlay_show_fps,
        "overlay_show_low": c.overlay_show_low,
        "overlay_show_gpu": c.overlay_show_gpu,
        "overlay_show_vram": c.overlay_show_vram,
        "overlay_show_cpu": c.overlay_show_cpu,
        "overlay_show_ram": c.overlay_show_ram,
        "overlay_show_page": c.overlay_show_page,
        "overlay_show_net": c.overlay_show_net,
        "overlay_show_day": c.overlay_show_day,
        "overlay_show_app": c.overlay_show_app,
    });
    drop(c);

    let _ = app.emit("overlay_config_changed", snapshot);
}

pub fn provider_label(id: &str) -> &'static str {
    match id {
        "codex" => "Codex",
        "cursor" => "Cursor",
        "gemini" => "Antigravity",
        "gemini_cli" => "Gemini CLI",
        "opencode" => "OpenCode",
        "deepseek" => "DeepSeek",
        "zed" => "Zed AI",
        "ollama" => "Ollama",
        "aider" => "Aider AI",
        _ => "Claude",
    }
}

/// Every provider the tray menu can offer, in the order the notch shows them.
pub const TRAY_PROVIDER_IDS: [&str; 10] = [
    "claude", "codex", "cursor", "gemini", "gemini_cli", "opencode", "deepseek", "zed", "ollama", "aider",
];

/// Draws the icon and writes the tooltip. Shared by the polling thread and by the settings window,
/// so a change made in settings shows up at once rather than on the next poll.
fn paint_tray(app: &AppHandle, mode: &str, slots: &[config::TraySlot], values: &[Option<u32>]) {
    let Some(tray) = app.tray_by_id("main") else {
        applog("tray: no tray with id 'main' — icon not updated");
        return;
    };
    let (tray_disp, hw_enabled) = {
        let st = app.state::<AppState>();
        let c = st.cfg.lock().unwrap();
        (c.tray_display_mode.clone(), c.hardware_enabled)
    };

    if hw_enabled && tray_disp == "hardware" {
        let m = hardware::poll_system_metrics();
        let cpu_p = m.cpu_usage.round().clamp(0.0, 100.0) as u32;
        let ram_p = m.ram_usage_pct.round().clamp(0.0, 100.0) as u32;
        let hw_vals = vec![Some(cpu_p), Some(ram_p)];
        let outcome = match mode {
            "bars" => tray.set_icon(Some(trayicon::bars(&hw_vals))),
            _ => tray.set_icon(Some(trayicon::numbers(&hw_vals))),
        };
        if let Err(e) = outcome {
            applog(&format!("tray: set_icon FAILED (hardware): {e}"));
        }
        let ram_used = m.ram_used_mb as f64 / 1024.0;
        let ram_tot = m.ram_total_mb as f64 / 1024.0;
        let tip = format!(
            "CodeNotch Hardware Vitals\nCPU: {:.1}% ({} Cores)\nRAM: {:.1}/{:.1} GB ({}%)\nNet: ↓ {} · ↑ {}\nToday: {} downloaded",
            m.cpu_usage, m.cpu_cores, ram_used, ram_tot, ram_p, m.net_download_speed, m.net_upload_speed, m.daily_download_str
        );
        let _ = tray.set_tooltip(Some(&tip));
        return;
    }

    if hw_enabled && tray_disp == "network" {
        let m = hardware::poll_system_metrics();
        let down_kbs = (m.net_download_bps / 1024) as u32;
        let down_scaled = (down_kbs / 100).min(100);
        let up_kbs = (m.net_upload_bps / 1024) as u32;
        let up_scaled = (up_kbs / 100).min(100);
        let net_vals = vec![Some(down_scaled), Some(up_scaled)];
        let outcome = match mode {
            "bars" => tray.set_icon(Some(trayicon::bars(&net_vals))),
            _ => tray.set_icon(Some(trayicon::numbers(&net_vals))),
        };
        if let Err(e) = outcome {
            applog(&format!("tray: set_icon FAILED (network): {e}"));
        }
        let tip = format!(
            "CodeNotch Network Speeds\nDownload: {}\nUpload: {}\nToday: {} ↓ / {} ↑",
            m.net_download_speed, m.net_upload_speed, m.daily_download_str, m.daily_upload_str
        );
        let _ = tray.set_tooltip(Some(&tip));
        return;
    }

    let outcome = match mode {
        "numbers" if !values.is_empty() => tray.set_icon(Some(trayicon::numbers(values))),
        "bars" if !values.is_empty() => tray.set_icon(Some(trayicon::bars(values))),
        // "Plain icon": the application's own icon
        _ => match trayicon::app_mark() {
            Some(img) => tray.set_icon(Some(img)),
            None => Ok(()), // leave whatever icon is there rather than clearing it to nothing
        },
    };
    if let Err(e) = outcome {
        applog(&format!("tray: set_icon FAILED mode={mode} values={values:?}: {e}"));
    }
    // The tooltip lists every slot, including any the digit layout could not fit, so nothing is
    // silently dropped.
    let parts: Vec<String> = slots
        .iter()
        .zip(values.iter())
        .map(|(slot, v)| {
            format!(
                "{} {}",
                provider_label(&slot.provider),
                v.map(|p| format!("{p}%")).unwrap_or_else(|| "—".into())
            )
        })
        .collect();
    let tip = if parts.is_empty() {
        concat!("Codenotch v", env!("CARGO_PKG_VERSION")).to_string()
    } else {
        format!("Codenotch — {}", parts.join(" · "))
    };
    let _ = tray.set_tooltip(Some(&tip));
}

/// Reads the current settings and readings, and repaints immediately.
pub fn repaint_tray(app: &AppHandle) {
    let (mode, slots) = {
        let st = app.state::<AppState>();
        let c = st.cfg.lock().unwrap();
        (c.tray_mode.clone(), c.tray_slots.clone())
    };
    let values: Vec<Option<u32>> = slots.iter().map(|s| reading_for_slot(app, s)).collect();
    paint_tray(app, &mode, &slots, &values);
}

pub fn cycle_tray_display_mode_impl(app: AppHandle) -> String {
    let next_mode = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        let next = match c.tray_display_mode.as_str() {
            "ai" => "hardware",
            "hardware" => "network",
            _ => "ai",
        };
        c.tray_display_mode = next.to_string();
        config::save(&c);
        next.to_string()
    };
    repaint_tray(&app);
    let _ = app.emit("tray_display_mode_changed", &next_mode);
    next_mode
}

#[tauri::command]
fn cycle_tray_display_mode(app: AppHandle) -> String {
    cycle_tray_display_mode_impl(app)
}

pub fn toggle_overlay_window(app: &AppHandle) -> bool {
    if let Some(w) = app.get_webview_window("overlay") {
        let is_vis = w.is_visible().unwrap_or(false);
        let target = !is_vis;
        if target {
            let _ = w.show();
            let _ = w.set_focus();
        } else {
            let _ = w.hide();
        }
        {
            let st = app.state::<AppState>();
            let mut c = st.cfg.lock().unwrap();
            c.overlay_enabled = target;
            config::save(&c);
        }
        let _ = app.emit("overlay_visibility", target);
        target
    } else {
        false
    }
}

/// Repaints when a reading changes. Every 2 seconds, but it only touches the icon when something
/// actually moved, so it costs nothing while idle.
fn start_tray_updater(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last: Option<(String, String, Vec<config::TraySlot>, Vec<Option<u32>>)> = None;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let (mode, slots, tray_disp, hw_enabled) = {
                let st = app.state::<AppState>();
                let c = st.cfg.lock().unwrap();
                (c.tray_mode.clone(), c.tray_slots.clone(), c.tray_display_mode.clone(), c.hardware_enabled)
            };
            if hw_enabled && (tray_disp == "hardware" || tray_disp == "network") {
                paint_tray(&app, &mode, &slots, &[]);
                continue;
            }
            let values: Vec<Option<u32>> = slots.iter().map(|s| reading_for_slot(&app, s)).collect();
            let key = (mode.clone(), tray_disp.clone(), slots.clone(), values.clone());
            if last.as_ref() == Some(&key) {
                continue;
            }
            last = Some(key);
            paint_tray(&app, &mode, &slots, &values);
        }
    });
}

/// Seen-clears-it: looking at a session acknowledges it (engine behaviour, unchanged)
#[cfg(windows)]
fn ack_scan(app: &AppHandle) -> bool {
    let need = {
        let st = app.state::<AppState>();
        let store = st.store.lock().unwrap();
        store.has_done()
    };
    if !need {
        return false;
    }
    let fg = focus::fg_pid();
    if fg == 0 {
        return false;
    }
    let maps = focus::proc_maps();
    let fg_name = maps.name.get(&fg).cloned().unwrap_or_default();
    let fg_is_claude_desktop = fg_name.contains("claude") && !fg_name.contains("codenotch");
    let st = app.state::<AppState>();
    let mut store = st.store.lock().unwrap();
    store.ack_done(|s| {
        if s.ppid == 0 {
            fg_is_claude_desktop
        } else {
            focus::pid_hits_chain(fg, &focus::chain_of(s.ppid, &maps.ppid), &maps)
        }
    })
}
#[cfg(not(windows))]
fn ack_scan(_app: &AppHandle) -> bool {
    false
}

// ---------------- main ----------------

#[cfg(windows)]
fn attach_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
#[cfg(not(windows))]
fn attach_console() {}

fn report(r: Result<String, String>) {
    let msg = match r {
        Ok(m) => format!("OK: {m}"),
        Err(e) => format!("FAILED: {e}"),
    };
    println!("{msg}");
    let log = config::config_path().with_file_name("install.log");
    let _ = std::fs::write(log, &msg);
}

fn main() {
    attach_console();
    let args: Vec<String> = std::env::args().collect();
    if let Some(cmd) = args.get(1) {
        match cmd.as_str() {
            "install-hooks" => {
                report(hooks_install::install());
                return;
            }
            "uninstall-hooks" => {
                report(hooks_install::uninstall());
                return;
            }
            "autostart" => {
                let r = match args.get(2).map(|s| s.as_str()) {
                    Some("on") => autostart::enable(),
                    Some("off") => autostart::disable(),
                    _ => Err("usage: codenotch.exe autostart on|off".into()),
                };
                report(r);
                return;
            }
            "doctor" => {
                let out = if args.get(2).map(|s| s.as_str()) == Some("deep") { diag::run() } else { doctor::run() };
                println!("{out}");
                let log = config::config_path().with_file_name("doctor.log");
                let _ = std::fs::write(log, &out);
                return;
            }
            _ => {}
        }
    }

    let cfg = config::load();
    let port = cfg.port;

    #[cfg(windows)]
    {
        // Safe WebView2 arguments: disable bloat features while preserving native GPU rasterization and composition
        // Eliminates black box tile-dropping artifacts and GPU screen flickering/tearing
        std::env::set_var(
            "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
            "--disable-features=Translate,OptimizationHints,MediaRouter,DialMediaRouteProvider,CalculateNativeWinOcclusion \
             --disable-direct-composition-video-overlays \
             --renderer-process-limit=1 \
             --disk-cache-size=4194304",
        );
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Launching a freshly built exe while the old one is still running lands here: the new
            // instance is turned away and what stays on screen is the old process. Say so loudly.
            applog(&format!("single instance: another launch was refused; the running instance is build={BUILD} — quit it from the tray first if you just rebuilt"));
            let _ = app.emit("notice", format!("Codenotch is already running ({BUILD}) — quit it from the tray before starting a new build"));
        }))
        .on_window_event(|window, event| {
            if window.label() == "settings" || window.label() == "welcome" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                    trim_memory();
                }
            }
        })
        .manage(AppState {
            store: Mutex::new(Default::default()),
            cfg: Mutex::new(cfg),
            usage: Mutex::new(usage::load_persisted()),
            codex: Mutex::new(codex::load_persisted()),
            cursor: Mutex::new(cursor::load_persisted()),
            antigravity: Mutex::new(antigravity::load_persisted()),
            glyphs: Mutex::new(Default::default()),
            activity: Mutex::new(Vec::new()),
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            get_usage,
            get_codex,
            get_cursor,
            get_antigravity,
            get_glyphs,
            get_activity,
            open_data_dir,
            drag_begin,
            open_provider_page,
            refresh_usage,
            open_usage_page,
            set_hot,
            report_dpr,
            log_js,
            focus_session,
            dismiss_session,
            set_lang,
            get_scale,
            set_scale,
            get_tray_options,
            get_tray_config,
            set_tray_config,
            get_tray_preview,
            get_notch_slots,
            set_notch_slots,
            get_app_icon,
            get_ui_flags,
            set_ui_flags,
            get_lang,
            get_lang_resolved,
            get_autostart,
            set_autostart,
            get_hooks_installed,
            set_hooks_installed,
            reset_notch_position,
            open_settings,
            get_theme,
            get_appearance,
            set_appearance,
            get_notch_y,
            set_notch_y,
            get_hotkey,
            set_hotkey,
            set_backdrop,
            detect_installed_providers,
            set_settings_dark_mode,
            get_system_metrics,
            toggle_overlay,
            set_overlay_clickthrough,
            set_overlay_opacity,
            save_overlay_position,
            resize_overlay_window,
            move_overlay_by,
            save_overlay_metrics,
            start_overlay_drag,
            snap_overlay_position,
            get_hardware_config,
            set_hardware_config,
            cycle_tray_display_mode,
            open_welcome_window,
            finish_welcome
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            place_notch(&handle);
            noactivate(&handle);
            let current_style = {
                let st = handle.state::<AppState>();
                let c = st.cfg.lock().unwrap();
                c.appearance_style.clone()
            };
            apply_backdrop(&handle, "notch", &current_style);
            apply_backdrop(&handle, "welcome", &current_style);
            let is_first_run = {
                let st = handle.state::<AppState>();
                let c = st.cfg.lock().unwrap();
                !c.first_run_completed
            };
            if is_first_run {
                if let Some(w) = handle.get_webview_window("welcome") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            } else if let Some(w) = handle.get_webview_window("notch") {
                let _ = w.show();
            }
            if let Some(ow) = handle.get_webview_window("overlay") {
                #[cfg(windows)]
                {
                    use windows::Win32::Foundation::HWND;
                    use windows::Win32::UI::WindowsAndMessaging::{
                        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, GWL_STYLE,
                        WS_EX_TOOLWINDOW, WS_EX_NOACTIVATE, WS_MAXIMIZEBOX, WS_THICKFRAME,
                    };
                    if let Ok(h) = ow.hwnd() {
                        let hwnd = HWND(h.0 as isize as *mut core::ffi::c_void);
                        unsafe {
                            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                            let _ = SetWindowLongPtrW(
                                hwnd,
                                GWL_EXSTYLE,
                                ex | WS_EX_TOOLWINDOW.0 as isize | WS_EX_NOACTIVATE.0 as isize,
                            );
                            let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
                            let _ = SetWindowLongPtrW(
                                hwnd,
                                GWL_STYLE,
                                style & !(WS_MAXIMIZEBOX.0 as isize) & !(WS_THICKFRAME.0 as isize),
                            );
                        }
                    }
                }
                let (ov_enabled, ov_ct, ox, oy) = {
                    let st = handle.state::<AppState>();
                    let c = st.cfg.lock().unwrap();
                    (c.overlay_enabled, c.overlay_clickthrough, c.overlay_x, c.overlay_y)
                };
                if let (Some(x), Some(y)) = (ox, oy) {
                    let _ = ow.set_position(tauri::PhysicalPosition::new(x, y));
                }
                if ov_enabled {
                    let _ = ow.show();
                }
                if ov_ct {
                    let _ = set_overlay_clickthrough(handle.clone(), true);
                }
            }
            tray::setup(&handle)?;
            trim_memory();
            start_tray_updater(handle.clone());
            // Honours the saved switches: a notch hidden last time stays hidden.
            apply_visibility(&handle);
            start_hotkey_listener(handle.clone());
            start_theme_watcher(handle.clone());
            server::start(handle.clone(), port);
            watcher::start(handle.clone());
            usage::start(handle.clone());
            codex::start(handle.clone());
            cursor::start(handle.clone());
            antigravity::start(handle.clone());
            activity::start(handle.clone());
            // Collecting glyphs may read icon resources out of a few executables; do it off the main thread and push when done
            let gh = handle.clone();
            spawn_thread("glyphs", move || reload_glyphs(&gh));
            start_pointer_watchdog(handle.clone());
            // Seen-clears-it scan
            let acker = handle.clone();
            spawn_thread("ack_scan", move || {
                activity::lower_thread_priority();
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    if ack_scan(&acker) {
                        broadcast(&acker);
                    }
                }
            });
            // Stale session cleanup + periodic memory trimming
            let sweeper = handle.clone();
            spawn_thread("sweeper", move || {
                let mut tick: u32 = 0;
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(30));
                    let changed = {
                        let st = sweeper.state::<AppState>();
                        let mut s = st.store.lock().unwrap();
                        s.sweep()
                    };
                    if changed {
                        broadcast(&sweeper);
                    }
                    tick = tick.wrapping_add(1);
                    if tick % 2 == 0 {
                        // Trim memory working set every 60s
                        trim_memory();
                    }
                }
            });
            // Persist the config (codenotch-hook reads the port from it)
            {
                let st = handle.state::<AppState>();
                let c = st.cfg.lock().unwrap();
                config::save(&c);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Codenotch failed to start");
}

#[cfg(test)]
mod tests {
    use super::{cursor_in_hot, HOT_PAD};

    /// Real values from the run.log in #106: a 2560×1600 display at 150 %.
    const PILL: [f64; 4] = [405.0, 183.5, 105.0, 323.0];
    const CARD: [f64; 4] = [21.0, 142.5, 369.0, 262.0];
    const WINDOW: Option<(f64, f64)> = Some((510.0, 690.0));

    #[test]
    fn nothing_is_hot_before_the_page_reports() {
        assert!(!cursor_in_hot(&[], 450.0, 300.0, WINDOW));
    }

    #[test]
    fn the_pill_is_hot() {
        assert!(cursor_in_hot(&[PILL], 450.0, 300.0, WINDOW));
    }

    #[test]
    fn the_transparent_area_beside_the_pill_is_not() {
        assert!(!cursor_in_hot(&[PILL], 0.0, 297.0, WINDOW));
        assert!(!cursor_in_hot(&[PILL], 100.0, 400.0, WINDOW));
    }

    #[test]
    fn the_card_is_hot_while_it_is_open() {
        assert!(!cursor_in_hot(&[PILL], 100.0, 250.0, WINDOW));
        assert!(cursor_in_hot(&[PILL, CARD], 100.0, 250.0, WINDOW));
    }

    #[test]
    fn the_shipped_pill_and_card_have_no_cold_strip_between_them() {
        // The 15 px gap is narrower than the 20 px the two pads bring, so the pads already bridge
        // it and the bounding box never fires for the shipped layout. Pinned: if that stops being
        // true the crossing starts depending on the bounding box, and the card blinks out mid-travel.
        let x = (CARD[0] + CARD[2] + PILL[0]) / 2.0;
        assert!(cursor_in_hot(&[PILL, CARD], x, 250.0, WINDOW));
        const { assert!(PILL[0] - (CARD[0] + CARD[2]) < 2.0 * HOT_PAD) };
    }

    /// Far enough apart that the pads do not meet — the case the bounding box exists for.
    const FAR_A: [f64; 4] = [0.0, 0.0, 50.0, 50.0];
    const FAR_B: [f64; 4] = [200.0, 0.0, 50.0, 50.0];

    #[test]
    fn a_wide_gap_is_bridged_by_the_bounding_box() {
        assert!(cursor_in_hot(&[FAR_A, FAR_B], 125.0, 25.0, None));
    }

    #[test]
    fn the_bounding_box_needs_two_rectangles_to_bridge_anything() {
        assert!(!cursor_in_hot(&[FAR_A], 125.0, 25.0, None));
    }

    #[test]
    fn the_pad_reaches_slightly_past_the_pill() {
        assert!(cursor_in_hot(&[PILL], PILL[0] - HOT_PAD + 1.0, 300.0, WINDOW));
        assert!(!cursor_in_hot(&[PILL], PILL[0] - HOT_PAD - 1.0, 300.0, WINDOW));
    }

    #[test]
    fn a_cursor_off_the_window_is_never_hot() {
        assert!(!cursor_in_hot(&[PILL], 515.0, 300.0, WINDOW));
        assert!(!cursor_in_hot(&[PILL], 450.0, -5.0, WINDOW));
    }

    #[test]
    fn an_unreadable_window_size_falls_back_to_the_rectangles() {
        assert!(cursor_in_hot(&[PILL], 450.0, 300.0, None));
        assert!(!cursor_in_hot(&[PILL], 100.0, 300.0, None));
    }
}
