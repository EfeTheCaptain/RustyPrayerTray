//! Taskbar lookup/geometry, fullscreen detection, DPI, z-order — port of Native/Interop.cs.

use super::displays;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, FindWindowW, GA_ROOT, GetAncestor, GetClassNameW, GetWindowRect,
    GetWindowThreadProcessId, IsWindowVisible,
};

pub fn taskbar() -> Option<HWND> {
    unsafe { FindWindowW(w!("Shell_TrayWnd"), None).ok().filter(|h| !h.is_invalid()) }
}

/// Is the taskbar window visible? If hidden (e.g. by a fullscreen app), return false.
pub fn taskbar_visible() -> bool {
    unsafe {
        if let Some(h) = taskbar() {
            IsWindowVisible(h).as_bool()
        } else {
            false
        }
    }
}

/// Is a specific taskbar HWND visible?
pub fn taskbar_visible_for(h: HWND) -> bool {
    unsafe { IsWindowVisible(h).as_bool() }
}

/// Taskbar window for a monitor device: primary's Shell_TrayWnd, or the matching
/// Shell_SecondaryTrayWnd; falls back to primary.
pub fn taskbar_for_device(device: Option<&str>) -> Option<HWND> {
    let primary = taskbar();
    let Some(d) = device.filter(|d| !d.is_empty()) else { return primary };
    if let Some(p) = displays::primary() {
        if p.device.eq_ignore_ascii_case(d) {
            return primary;
        }
    }
    let mut h = HWND::default();
    loop {
        h = match unsafe { FindWindowExW(None, Some(h), w!("Shell_SecondaryTrayWnd"), None) } {
            Ok(w) if !w.is_invalid() => w,
            _ => return primary,
        };
        if let Some(m) = displays::from_window(h) {
            if m.device.eq_ignore_ascii_case(d) {
                return Some(h);
            }
        }
    }
}

pub fn window_rect(h: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(h, &mut r) }.is_ok().then_some(r)
}

/// Screen-x of the taskbar's tray (clock cluster) left edge, or 0 if not found.
pub fn tray_notify_left(taskbar: HWND) -> i32 {
    match unsafe { FindWindowExW(Some(taskbar), None, w!("TrayNotifyWnd"), None) } {
        Ok(h) if !h.is_invalid() => window_rect(h).map(|r| r.left).unwrap_or(0),
        _ => 0,
    }
}

fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Some(class-name) when ANY top-level window (not just the foreground one) fully covers the
/// given monitor rect — i.e. a fullscreen/borderless app is present on that monitor; None when
/// the monitor is clear. This is per-monitor and does not depend on which window has focus,
/// fixing the multi-monitor bleed where interacting with the taskbar on monitor B made a game
/// on monitor A visible. The foreground window plays no role in this predicate.
pub fn is_fullscreen_on_monitor(bounds: &RECT) -> Option<String> {
    struct State {
        bounds: RECT,
        class: String,
        found: bool,
    }
    let mut state = State { bounds: *bounds, class: String::new(), found: false };
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        // SAFETY: EnumWindows passes back the `&mut State` supplied as lparam; it runs
        // synchronously on this thread, so the borrow is exclusive and alive throughout.
        let st = &mut *(lparam.0 as *mut State);
        if st.found {
            return false.into();
        }
        if !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }
        // Skip owned windows (overlays riding an owner); only root windows occupy a monitor.
        if GetAncestor(hwnd, GA_ROOT) != hwnd {
            return true.into();
        }
        let cls = class_name(hwnd);
        match cls.as_str() {
            "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "WorkerW" | "Progman"
            | "RustyPrayerTrayWidget" | "RustyPrayerTrayApp" | "RustyPrayerTrayHidden" | "RustyPrayerTrayPopup"
            | "RustyPrayerTraySettings" => {
                return true.into();
            }
            _ => {}
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return true.into();
        }
        if !(r.left <= st.bounds.left
            && r.top <= st.bounds.top
            && r.right >= st.bounds.right
            && r.bottom >= st.bounds.bottom)
        {
            return true.into();
        }
        // A DWM-cloaked window (suspended UWP etc.) is invisible even though it is
        // "visible" and full-size — it does not occupy the screen.
        let mut cloaked = 0u32;
        let ok = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        )
        .is_ok();
        if ok && cloaked != 0 {
            return true.into();
        }
        st.found = true;
        st.class = cls;
        false.into()
    }
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut state as *mut _ as isize));
    }
    if state.found {
        Some(state.class)
    } else {
        None
    }
}

pub fn scale_of(h: HWND) -> f32 {
    if h.is_invalid() {
        return 1.0;
    }
    let dpi = unsafe { GetDpiForWindow(h) };
    if dpi == 0 {
        1.0
    } else {
        dpi as f32 / 96.0
    }
}

pub fn thread_of(h: HWND) -> u32 {
    unsafe { GetWindowThreadProcessId(h, None) }
}
