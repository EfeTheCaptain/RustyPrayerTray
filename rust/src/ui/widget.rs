//! The next-prayer taskbar pill, port of UI/TaskbarWidget.cs: a top-level always-on-top
//! overlay owned by the taskbar window so it rides the taskbar's z-band.

use crate::i18n;
use crate::native::{displays, taskbar};
use crate::ui::gdip::{self, Bitmap, Font, Graphics, SolidBrush, StringFormat};
use crate::ui::theme;
use crate::ui::window::{self, WindowHandler};
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, EndPaint, InvalidateRect, SetWindowRgn, PAINTSTRUCT,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_LBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, DestroyWindow, GetCursorPos, HICON, IMAGE_ICON, IsWindowVisible, LoadImageW, LR_DEFAULTSIZE,
    LR_LOADFROMFILE, MoveWindow, PostMessageW, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, EVENT_OBJECT_REORDER, EVENT_SYSTEM_FOREGROUND, SW_HIDE, SW_SHOWNA, WINEVENT_OUTOFCONTEXT,
    WM_APP, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT,
    WM_RBUTTONUP, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    GWLP_HWNDPARENT, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SWP_NOACTIVATE,
};

const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_CAPTURECHANGED: u32 = 0x0215;

/// Posted to the app window on pill left-click (toggle popup).
pub const WM_WIDGET_CLICK: u32 = WM_APP + 2;
/// Posted to the app window on pill right-click; cursor pos packed in lparam.
pub const WM_WIDGET_MENU: u32 = WM_APP + 3;
/// Posted to the app window after a drag; the new logical offset rides in wparam.
pub const WM_WIDGET_MOVED: u32 = WM_APP + 9;
/// Posted to the widget window by the win-event hook (raise/visibility check).
const WM_RAISE: u32 = WM_APP + 10;

const MA_NOACTIVATE: u32 = 3;

/// Pixels of travel before a press counts as a drag rather than a click.
const DRAG_SLOP: i32 = 4;

// Hook callbacks carry no context; a single widget exists at a time.
static HOOK_TARGET: AtomicIsize = AtomicIsize::new(0);

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    let target = HOOK_TARGET.load(Ordering::Relaxed);
    if target != 0 {
        // SAFETY: HOOK_TARGET holds the widget HWND stored at creation (0 = none); the
        // callback only forwards a message to it and never dereferences it.
        let _ = unsafe { PostMessageW(Some(HWND(target as *mut _)), WM_RAISE, WPARAM(0), LPARAM(0)) };
    }
}

pub struct Widget {
    pub hwnd: HWND,
    app_hwnd: HWND,
    device: Option<String>,
    pub anchor_right: bool,
    pub offset: i32,
    pub hide_on_fullscreen: bool,
    pub locked: bool,
    name: String,
    count: String,
    urgency: f32,                // 0 = window wide open, 1 = next prayer due
    icon: HICON,
    has_icon: bool,
    last_data_has_icon: bool,
    drag_from: Option<(i32, i32)>, // (cursor x, window x) at mouse-down; Some() = pressed
    drag_moved: bool,            // travelled past the slop, so this is a drag not a click
    ref_digit: Option<char>,     // widest digit for the current basis (measured, not assumed)
    digit_font_scale: f32,      // digit-normalization basis
    digit_font_family: String,
    hover: bool,
    tracking: bool,
    paused: bool,
    suppressed: bool,
    w: i32,
    h: i32,
    scale: f32,
    last_rect: RECT,
    buffer: Option<Bitmap>,
    measure_bmp: Bitmap,
    fg_hook: HWINEVENTHOOK,
    reorder_hook: HWINEVENTHOOK,
    last_raise: u32,
}

fn tick_count() -> u32 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount() }
}

/// Transition log for debugging z-order/visibility bugs, off by default: set the
/// PRAYERTRAY_ZDEBUG environment variable to append every hide/show decision (with its
/// triggering event and the fullscreen culprit) to %TEMP%\prayertray_zdebug.log.
fn zdebug(msg: &str) {
    if std::env::var_os("PRAYERTRAY_ZDEBUG").is_none() {
        return;
    }
    use std::io::Write;
    let line = format!("[{}] {}\r\n", tick_count(), msg);
    let path = std::env::temp_dir().join("prayertray_zdebug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

impl Widget {
    pub fn new(app_hwnd: HWND, device: Option<String>) -> Box<Self> {
        gdip::init();
        let tb = taskbar::taskbar_for_device(device.as_deref());
        let scale = tb.map(taskbar::scale_of).unwrap_or(1.0);

        let mut widget = Box::new(Self {
            hwnd: HWND::default(),
            app_hwnd,
            device,
            anchor_right: true,
            offset: 12,
            hide_on_fullscreen: true,
            locked: false,
            name: "—".into(),
            count: "…".into(),
            urgency: 0.0,
            icon: HICON::default(),
            has_icon: false,
            last_data_has_icon: false,
            drag_from: None,
            drag_moved: false,
            ref_digit: None,
            digit_font_scale: theme::font_scale(),
            digit_font_family: theme::family(),
            hover: false,
            tracking: false,
            paused: false,
            suppressed: false,
            w: 160,
            h: 32,
            scale: if scale > 0.0 { scale } else { 1.0 },
            last_rect: RECT::default(),
            buffer: None,
            measure_bmp: Bitmap::new(1, 1),
            fg_hook: HWINEVENTHOOK::default(),
            reorder_hook: HWINEVENTHOOK::default(),
            last_raise: 0,
        });

        let hwnd = window::create(
            w!("RustyPrayerTrayWidget"),
            w!(""),
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            WS_POPUP | WS_VISIBLE,
            0,
            0,
            widget.w,
            widget.h,
            tb, // owner -> rides in the taskbar's z-band
            widget.as_mut() as *mut Self,
        )
        .expect("widget window");
        widget.hwnd = hwnd;
        HOOK_TARGET.store(hwnd.0 as isize, Ordering::Relaxed);

        // Re-raise the instant the taskbar comes forward. Foreground hook is global (infrequent);
        // the chatty reorder hook is scoped to the taskbar's thread — never system-wide.
        unsafe {
            widget.fg_hook = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(win_event_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            );
            if let Some(tb) = tb {
                let tid = taskbar::thread_of(tb);
                if tid != 0 {
                    widget.reorder_hook = SetWinEventHook(
                        EVENT_OBJECT_REORDER,
                        EVENT_OBJECT_REORDER,
                        None,
                        Some(win_event_proc),
                        0,
                        tid,
                        WINEVENT_OUTOFCONTEXT,
                    );
                }
            }
        }

        widget.render_buffer();
        widget.sync_position();
        // Evaluate immediately at creation: the window is born WS_VISIBLE, so a fullscreen
        // app already running must not see the pill flash until the first watchdog tick.
        widget.update_visibility("init");
        widget
    }

    pub fn device_name(&self) -> Option<&str> {
        self.device.as_deref()
    }

    fn s(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    fn main_font(&self) -> Font {
        Font::new(&theme::family(), 13.0 * self.scale * theme::font_scale(), gdip::STYLE_REGULAR)
    }

    fn count_font(&self) -> Font {
        Font::new(&theme::family(), 13.0 * self.scale * theme::font_scale(), gdip::STYLE_BOLD)
    }

    fn measure(&self, s: &str, font: &Font) -> f32 {
        Graphics::from_bitmap(&self.measure_bmp).measure_string(s, font).0
    }

    /// Widest of '0'..'9' in `font` (measured, cached per basis). Substituting it for every digit
    /// makes equal-length countdowns measure identically (kills ±1px jitter) and upper-bounds width.
    fn ref_digit(&mut self, font: &Font) -> char {
        if let Some(c) = self.ref_digit {
            return c;
        }
        let (mut best, mut best_w) = ('0', 0.0f32);
        for d in '0'..='9' {
            let w = self.measure(&d.to_string(), font);
            if w > best_w {
                best_w = w;
                best = d;
            }
        }
        self.ref_digit = Some(best);
        best
    }

    fn norm_digits(&mut self, s: &str, font: &Font) -> String {
        let r = self.ref_digit(font);
        s.chars().map(|c| if c.is_ascii_digit() { r } else { c }).collect()
    }

    fn reset_slots(&mut self) {
        self.ref_digit = None;
    }

    /// Reset the digit-normalization basis when the font/DPI basis changes. Idempotent within a tick.
    fn refresh_slot_basis(&mut self) {
        if theme::font_scale() != self.digit_font_scale || theme::family() != self.digit_font_family {
            self.digit_font_scale = theme::font_scale();
            self.digit_font_family = theme::family();
            self.reset_slots();
        }
    }

    /// `urgency` is 0.0 with the window wide open and 1.0 as it closes; it tints the countdown.
    pub fn set_data(&mut self, name: &str, countdown: &str, urgency: f32) {
        if self.name == name
            && self.count == countdown
            && self.urgency == urgency
            && self.has_icon == self.last_data_has_icon
        {
            return;
        }
        self.name = name.into();
        self.count = countdown.into();
        self.urgency = urgency;
        self.last_data_has_icon = self.has_icon;
        self.resize_to_content();
        self.render_buffer();
        self.invalidate();
    }

    pub fn set_icon_bytes(&mut self, bytes: &[u8]) {
        let path = std::env::temp_dir().join("prayertray_pill.ico");
        if std::fs::write(&path, bytes).is_ok() {
            if let Some(s) = path.to_str() {
                let mut wide: Vec<u16> = s.encode_utf16().collect();
                wide.push(0);
                let hicon = unsafe {
                    LoadImageW(
                        None,
                        windows::core::PWSTR(wide.as_mut_ptr()),
                        IMAGE_ICON,
                        0,
                        0,
                        LR_DEFAULTSIZE | LR_LOADFROMFILE,
                    )
                    .map(|h| HICON(h.0))
                    .unwrap_or_default()
                };
                if !hicon.0.is_null() {
                    if self.has_icon && !self.icon.0.is_null() {
                        unsafe { let _ = DestroyIcon(self.icon); }
                    }
                    self.icon = hicon;
                    self.has_icon = true;
                    self.rerender();
                }
            }
        }
    }

    pub fn clear_icon(&mut self) {
        if self.has_icon && !self.icon.0.is_null() {
            unsafe { let _ = DestroyIcon(self.icon); }
        }
        self.icon = HICON::default();
        self.has_icon = false;
        self.rerender();
    }

    fn resize_to_content(&mut self) {
        self.refresh_slot_basis(); // reset count slot on font change even with an empty tail
        let f_main = self.main_font();
        let f_count = self.count_font();
        let w_left = self.measure(&self.name, &f_main).ceil() as i32;
        let w_icon = if self.has_icon { self.s(18.0) } else { 0 };
        // Digit-normalized so equal-length countdowns measure identically (no ±1px
        // jitter); width still tracks the current text, so no reserved gap after short values.
        let count_txt = self.count.clone();
        let count_norm = self.norm_digits(&count_txt, &f_count);
        let w_count = self.measure(&count_norm, &f_count).ceil() as i32;
        // Daily icon + name + gap + count: pad + gap + icon + text + gap + count + right pad.
        self.w = (self.s(12.0) + self.s(8.0) + w_icon + w_left + self.s(8.0) + w_count + self.s(12.0))
            .max(self.s(110.0));
    }

    /// 1 s watchdog; hooks handle the instant cases.
    pub fn tick(&mut self) {
        self.update_visibility("timer");
    }

    /// The single choke point that owns the pill's visibility. Z-order is stateless: the
    /// window is created WS_EX_TOPMOST and never leaves that band — fullscreen handling is
    /// hide/show only, and `raise` re-asserts the top of the topmost band when the taskbar
    /// comes forward. `event` tags the trigger in the PRAYERTRAY_ZDEBUG transition log.
    fn update_visibility(&mut self, event: &str) {
        if self.hwnd.is_invalid() || self.paused {
            return;
        }

        let mut want_hidden = false;
        let mut culprit = String::new();
        if self.hide_on_fullscreen || self.suppressed {
            // Per-monitor fullscreen occupancy: any root window fully covering THIS monitor,
            // regardless of focus — so a game on monitor A stays hidden while the user clicks
            // the taskbar on monitor B. The foreground window plays no role here.
            if let Some(screen) = displays::by_device(self.device.as_deref()) {
                if let Some(cls) = taskbar::is_fullscreen_on_monitor(&screen.bounds) {
                    want_hidden = true;
                    culprit = cls;
                }
            }
            // Supplementary hide-only net for a genuinely hidden taskbar (RDP etc.); it can
            // never gate a show. IsWindowVisible is a style bit — a fullscreen game covering
            // the bar does NOT clear it, so this stays false in every normal case.
            if !want_hidden {
                let tb = taskbar::taskbar_for_device(self.device.as_deref());
                if tb.map_or(false, |h| !taskbar::taskbar_visible_for(h)) {
                    want_hidden = true;
                    culprit = "<taskbar hidden>".into();
                }
            }
        }

        if want_hidden {
            if !self.suppressed {
                self.suppressed = true;
                self.hide();
                zdebug(&format!("{event}: hide (covers monitor: {culprit})"));
            }
            return;
        }

        if self.suppressed {
            self.suppressed = false;
            self.show();
            zdebug(&format!("{event}: show (monitor clear)"));
        }
        // The taskbar came forward and buried us: re-assert the top of the topmost band.
        self.raise();
        self.sync_position();
    }

    pub fn screen_rect(&self) -> RECT {
        taskbar::window_rect(self.hwnd).unwrap_or_default()
    }

    pub fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd) }.as_bool()
    }

    pub fn show(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNA);
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    /// Re-assert the top of the topmost band. The pill never changes bands after creation
    /// (stateless z-order); this only reorders it within its own band after a taskbar click
    /// buries it. On a hidden window it is a no-op.
    fn raise(&self) {
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0, 0, 0, 0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    /// Stop fighting modal dialogs: a topmost re-raise dismisses an open combo dropdown.
    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn resume(&mut self) {
        self.paused = false;
        self.update_visibility("resume");
    }

    pub fn sync_position(&mut self) {
        if !self.paused {
            self.position_core();
        }
    }

    /// Settings live preview: bypasses the pause guard and never re-raises.
    pub fn preview_reposition(&mut self) {
        self.position_core();
    }

    /// The band the pill lives in: (strip rect, right edge, left edge). The right edge stops at
    /// the tray cluster so the pill never lands under the clock.
    fn strip(&self) -> Option<(RECT, i32, i32)> {
        let screen = displays::by_device(self.device.as_deref())?;
        let tb = taskbar::taskbar_for_device(self.device.as_deref());
        let on_its_bar = tb
            .and_then(displays::from_window)
            .map(|m| m.device.eq_ignore_ascii_case(&screen.device))
            .unwrap_or(false);
        if on_its_bar {
            let r = taskbar::window_rect(tb?)?;
            if r.right <= r.left {
                return None;
            }
            let tray_left = taskbar::tray_notify_left(tb?);
            Some((r, if tray_left > 0 { tray_left } else { r.right }, r.left))
        } else {
            // No taskbar on this monitor -> float at its bottom.
            let b = screen.bounds;
            let h = self.s(40.0);
            let r = RECT { left: b.left, top: b.bottom - h, right: b.right, bottom: b.bottom };
            Some((r, b.right, b.left))
        }
    }

    fn drag_to(&mut self, cursor_x: i32) {
        let Some((start_cursor, start_window)) = self.drag_from else { return };
        // A lost button-up (capture stolen, released off the pill) would otherwise leave the
        // pill following a button-less cursor on every hover.
        if unsafe { GetKeyState(VK_LBUTTON.0 as i32) } >= 0 {
            self.end_drag(true);
            return;
        }
        if !self.drag_moved && (cursor_x - start_cursor).abs() < DRAG_SLOP {
            return; // still within slop: this may yet be a plain click
        }
        if self.locked {
            return; // the press still counts as a click; only the move is refused
        }
        self.drag_moved = true;
        let Some((strip, right_edge, left_edge)) = self.strip() else { return };
        // widget_offset is clamped to 0..2000 logical units on save, so anything dropped beyond
        // that would spring back on the next reposition. Stop the pill there instead.
        let span = (2000.0 * self.scale) as i32;
        let (lo, hi) = if self.anchor_right {
            ((right_edge - self.w - span).max(left_edge), right_edge - self.w)
        } else {
            (left_edge, (left_edge + span).min(right_edge - self.w))
        };
        let x = (start_window + cursor_x - start_cursor).clamp(lo.min(hi), hi.max(lo));
        let strip_h = strip.bottom - strip.top;
        let y = strip.top + (strip_h - self.h) / 2;
        unsafe {
            let _ = MoveWindow(self.hwnd, x, y, self.w, self.h, true);
        }
        // Reparent to the taskbar of the monitor we are now on, so the pill rides that
        // monitor's taskbar z-band.
        if let Some(m) = displays::all().iter().find(|m| x >= m.bounds.left && x < m.bounds.right) {
            if let Some(new_tb) = taskbar::taskbar_for_device(Some(m.device.as_str())) {
                unsafe {
                    let _ = SetWindowLongPtrW(self.hwnd, GWLP_HWNDPARENT, new_tb.0 as isize);
                }
            }
        }
        // position_core early-returns when its target equals last_rect, so it has to learn
        // where the window actually went or the next tick would skip the correcting move.
        self.last_rect = RECT { left: x, top: y, right: x + self.w, bottom: y + self.h };
    }

    /// Release the drag. `commit` persists where it landed; a cancelled drag leaves the offset
    /// alone and lets the next reposition put the pill back.
    fn end_drag(&mut self, commit: bool) {
        let was_pressed = self.drag_from.take().is_some();
        let moved = self.drag_moved;
        self.drag_moved = false;
        if was_pressed {
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        if commit && moved {
            self.commit_drag();
        }
    }

    /// Turn the dragged position back into the anchored, DPI-independent offset config stores.
    fn commit_drag(&mut self) {
        let Some((_, right_edge, left_edge)) = self.strip() else { return };
        let px = if self.anchor_right {
            right_edge - self.last_rect.left - self.w
        } else {
            self.last_rect.left - left_edge
        };
        self.offset = ((px as f32 / self.scale).round() as i32).clamp(0, 2000);
        unsafe {
            let _ = PostMessageW(
                Some(self.app_hwnd),
                WM_WIDGET_MOVED,
                WPARAM(self.offset as usize),
                LPARAM(0),
            );
        }
    }

    fn position_core(&mut self) {
        if self.drag_from.is_some() {
            return; // the pointer owns the position until the button comes up
        }
        let Some(screen) = displays::by_device(self.device.as_deref()) else { return };
        let tb = taskbar::taskbar_for_device(self.device.as_deref());
        let on_its_bar = tb
            .and_then(displays::from_window)
            .map(|m| m.device.eq_ignore_ascii_case(&screen.device))
            .unwrap_or(false);

        let prev_scale = self.scale;
        let scale_src = if on_its_bar { tb.unwrap() } else { self.hwnd };
        self.scale = taskbar::scale_of(scale_src);
        if self.scale <= 0.0 {
            self.scale = 1.0;
        }
        if self.scale != prev_scale {
            self.reset_slots(); // digit basis is DPI-dependent too
            self.resize_to_content();
            self.render_buffer();
            self.invalidate();
        }

        let Some((strip, right_edge, left_edge)) = self.strip() else { return };

        let strip_h = strip.bottom - strip.top;
        self.h = self.s(32.0).min(strip_h - self.s(4.0));
        let y = strip.top + (strip_h - self.h) / 2;
        let x = if self.anchor_right {
            right_edge - self.w - self.s(self.offset as f32)
        } else {
            left_edge + self.s(self.offset as f32)
        };

        let target = RECT { left: x, top: y, right: x + self.w, bottom: y + self.h };
        if target == self.last_rect {
            return;
        }
        self.last_rect = target;
        unsafe {
            let _ = MoveWindow(self.hwnd, x, y, self.w, self.h, true);
        }
        self.update_region();
    }

    fn update_region(&self) {
        let r = self.s(8.0);
        unsafe {
            let rgn = CreateRoundRectRgn(0, 0, self.w + 1, self.h + 1, r * 2, r * 2);
            let _ = SetWindowRgn(self.hwnd, Some(rgn), true); // window takes ownership
        }
    }

    fn invalidate(&self) {
        if !self.hwnd.is_invalid() {
            unsafe {
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
        }
    }

    pub fn rerender(&mut self) {
        self.resize_to_content();
        self.render_buffer();
        self.invalidate();
    }

    fn render_buffer(&mut self) {
        let pal = theme::current();
        let bmp = Bitmap::new(self.w.max(1), self.h.max(1));
        {
            let g = Graphics::from_bitmap(&bmp);
            g.clear(if self.hover { pal.bg_hover } else { pal.bg });

            let cy = (self.h / 2) as f32;
            let f_main = self.main_font();
            let f_count = self.count_font();
            let wf = self.w as f32;
            let hf = self.h as f32;

            let text = SolidBrush::new(pal.text);
            let good = SolidBrush::new(theme::urgency(&pal, self.urgency));

            let rect = |x: f32, w: f32| gdip::rectf(x, 0.0, w, hf);
            let has_icon = self.has_icon && !self.icon.0.is_null();
            let ih = self.s(18.0) as f32;

            if i18n::is_rtl() {
                let far = StringFormat::new(gdip::ALIGN_FAR, gdip::ALIGN_CENTER);
                let mut xr = if has_icon {
                    if let Some(ibmp) = gdip::bitmap_from_hicon(self.icon) {
                        g.draw_bitmap_rect(&ibmp, wf - self.s(12.0) as f32 - ih, cy - ih / 2.0, ih, ih);
                    }
                    wf - self.s(12.0) as f32 - self.s(18.0) as f32 - self.s(4.0) as f32
                } else {
                    wf - self.s(12.0) as f32 - self.s(8.0) as f32
                };
                if !self.name.is_empty() {
                    g.draw_string(&self.name, &f_main, &text, rect(0.0, xr), &far);
                    xr -= self.measure(&self.name, &f_main) + self.s(8.0) as f32;
                }
                g.draw_string(&self.count, &f_count, &good, rect(0.0, xr), &far);
            } else {
                let sf = StringFormat::new(gdip::ALIGN_NEAR, gdip::ALIGN_CENTER);
                let mut x = if has_icon {
                    if let Some(ibmp) = gdip::bitmap_from_hicon(self.icon) {
                        g.draw_bitmap_rect(&ibmp, self.s(12.0) as f32, cy - ih / 2.0, ih, ih);
                    }
                    self.s(12.0) as f32 + self.s(8.0) as f32 + self.s(18.0) as f32 + self.s(4.0) as f32
                } else {
                    (self.s(12.0) + self.s(8.0)) as f32
                };
                if !self.name.is_empty() {
                    g.draw_string(&self.name, &f_main, &text, rect(x, wf), &sf);
                    x += self.measure(&self.name, &f_main) + self.s(8.0) as f32;
                }
                g.draw_string(&self.count, &f_count, &good, rect(x, wf), &sf);
            }
        }
        self.buffer = Some(bmp);
    }

    fn paint(&mut self) {
        let mut ps = PAINTSTRUCT::default();
        let hdc = unsafe { BeginPaint(self.hwnd, &mut ps) };
        if let Some(buf) = &self.buffer {
            let g = Graphics::from_hdc(hdc);
            g.draw_bitmap(buf, 0, 0);
        }
        unsafe {
            let _ = EndPaint(self.hwnd, &ps);
        }
    }

    fn track_leave(&self) {
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        unsafe {
            let _ = TrackMouseEvent(&mut tme);
        }
    }
}

impl WindowHandler for Widget {
    fn message(&mut self, _hwnd: HWND, msg: u32, _wparam: WPARAM, _lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            WM_PAINT => {
                self.paint();
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                let mut pt = Default::default();
                if unsafe { GetCursorPos(&mut pt) }.is_ok() {
                    unsafe { SetCapture(self.hwnd) };
                    self.drag_from = Some((pt.x, self.last_rect.left));
                    self.drag_moved = false;
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                if self.drag_from.is_some() {
                    let mut pt = Default::default();
                    if unsafe { GetCursorPos(&mut pt) }.is_ok() {
                        self.drag_to(pt.x);
                    }
                }
                if !self.tracking {
                    self.track_leave();
                    self.tracking = true;
                }
                if !self.hover {
                    self.hover = true;
                    self.render_buffer();
                    self.invalidate();
                }
                None
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                if self.hover {
                    self.hover = false;
                    self.render_buffer();
                    self.invalidate();
                }
                None
            }
            WM_LBUTTONUP => {
                let click = self.drag_from.is_some() && !self.drag_moved;
                self.end_drag(true);
                if click {
                    unsafe {
                        let _ = PostMessageW(Some(self.app_hwnd), WM_WIDGET_CLICK, WPARAM(0), LPARAM(0));
                    }
                }
                Some(LRESULT(0))
            }
            // Capture can go without a button-up: hiding on fullscreen mid-drag, a menu taking it,
            // the secure desktop. Without this the drag state sticks and position_core is dead.
            WM_CAPTURECHANGED => {
                self.drag_from = None;
                self.drag_moved = false;
                None
            }
            WM_RBUTTONUP => {
                let mut pt = Default::default();
                unsafe {
                    let _ = GetCursorPos(&mut pt);
                    let packed = ((pt.y as isize) << 32) | (pt.x as isize & 0xFFFF_FFFF);
                    let _ = PostMessageW(Some(self.app_hwnd), WM_WIDGET_MENU, WPARAM(0), LPARAM(packed));
                }
                None
            }
            WM_RAISE => {
                if !self.paused {
                    let now = tick_count();
                    if now.wrapping_sub(self.last_raise) >= 16 {
                        self.last_raise = now;
                        self.update_visibility("hook");
                    }
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

impl Drop for Widget {
    fn drop(&mut self) {
        HOOK_TARGET.store(0, Ordering::Relaxed);
        unsafe {
            if !self.fg_hook.is_invalid() {
                let _ = UnhookWinEvent(self.fg_hook);
            }
            if !self.reorder_hook.is_invalid() {
                let _ = UnhookWinEvent(self.reorder_hook);
            }
            if !self.hwnd.is_invalid() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}
