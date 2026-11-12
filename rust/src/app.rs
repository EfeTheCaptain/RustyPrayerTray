//! App orchestrator, port of App/AppHost.cs: hidden top-level window owning the tray icon,
//! timers, prayer computation, and the notification engine. The window is NOT message-only —
//! WM_POWERBROADCAST/WM_TIMECHANGE/WM_SETTINGCHANGE and the TaskbarCreated broadcast are
//! only delivered to real top-level windows.

use crate::config::{AppConfig, AppState};
use crate::datetime::{days_in_month, Date};
use crate::i18n;
use crate::native::{displays, quiet, startup, time};
use crate::services::aladhan;
use crate::services::update::UpdateInfo;
use crate::services::{audio, toast, update};
use crate::services::location::DetectedLocation;
use crate::ui::icon::{TrayIcon, WM_TRAY};
use crate::ui::popup::{Popup, Row, WM_POPUP_MOVED, WM_POPUP_PIN};
use crate::ui::settings::{self, SettingsHost, SettingsResult};
use crate::ui::theme;
use crate::ui::window::{self, WindowHandler};
use crate::ui::widget::{Widget, WM_WIDGET_CLICK, WM_WIDGET_MENU, WM_WIDGET_MOVED};
use std::collections::HashSet;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, KillTimer, PostMessageW, PostQuitMessage,
    RegisterWindowMessageW, SetForegroundWindow, SetTimer, TrackPopupMenuEx,
    MF_CHECKED, MF_SEPARATOR, MF_STRING, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, TPMPARAMS,
    TPM_LAYOUTRTL, TPM_RIGHTBUTTON, TPM_VERTICAL, WM_COMMAND, WM_DESTROY, WM_LBUTTONUP, WM_POWERBROADCAST,
    WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMECHANGE, WM_TIMER, WINDOW_EX_STYLE, WS_POPUP,
};

static MOON_ICONS: [&[u8]; 8] = [
    include_bytes!("../assets/moon_1.ico"),
    include_bytes!("../assets/moon_2.ico"),
    include_bytes!("../assets/moon_3.ico"),
    include_bytes!("../assets/moon_4.ico"),
    include_bytes!("../assets/moon_5.ico"),
    include_bytes!("../assets/moon_6.ico"),
    include_bytes!("../assets/moon_7.ico"),
    include_bytes!("../assets/moon_8.ico"),
];

static DAILY_ICONS: [&[u8]; 4] = [
    include_bytes!("../assets/daily_1.ico"), // night
    include_bytes!("../assets/daily_2.ico"), // sunrise
    include_bytes!("../assets/daily_3.ico"), // sun
    include_bytes!("../assets/daily_4.ico"), // sunset
];

const ORDER: [&str; 6] = ["fajr", "sunrise", "dhuhr", "asr", "maghrib", "isha"];

/// Hijri day (1-30) -> MOON_ICONS index, following the 8-phase lunar cycle:
/// new (1-2), waxing crescent (3-6), first quarter (7-8), waxing gibbous (9-12),
/// full (13-15), waning gibbous (16-18), last quarter (19-21), waning crescent (22-30).
fn moon_phase_idx(hijri_day: u32) -> usize {
    match hijri_day {
        1..=2 => 0,
        3..=6 => 1,
        7..=8 => 2,
        9..=12 => 3,
        13..=15 => 4,
        16..=18 => 5,
        19..=21 => 6,
        _ => 7,
    }
}

/// Cap on holding a muted azan when no later prayer bounds it (Isha).
const MUTED_AZAN_MAX_HOLD: i64 = 3 * 3600;

const TIMER_POS: usize = 1; // 30 s: reposition/seconds countdown
const TIMER_DATA: usize = 2; // 15 s: render + notification checks
const TIMER_WATCH: usize = 3; // 1 s: pill visibility watchdog (hooks handle the instant cases)

/// The countdown starts warming this long before the next prayer, and is fully urgent at it.
const URGENT_SECS: i64 = 15 * 60;

const CMD_SHOW_TIMES: usize = 1001;
const CMD_REFRESH: usize = 1002;
const CMD_STARTUP: usize = 1003;
const CMD_SETTINGS: usize = 1004;
const CMD_STOP_SOUND: usize = 1005;
const CMD_CHECK_UPDATES: usize = 1006;
const CMD_LOCK: usize = 1009;
const CMD_EXIT: usize = 1008;

/// Update-check result; lparam = Box<Option<UpdateInfo>> raw.
const WM_UPDATE_RESULT: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 6;
/// Download finished; wparam = ok, lparam = Box<UpdateInfo> raw.
const WM_UPDATE_DOWNLOADED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 7;
/// First-run detection finished; lparam = Box<Option<DetectedLocation>> raw.
const WM_FIRSTRUN_LOC: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 8;

/// Result of the next-month pre-fetch (Hijri rollover safety net).
const WM_NEXT_MONTH: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 11;

fn tick_count() -> u32 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount() }
}

// Single-instance mutex, held by main; released right before spawning an updated exe
// (the child's mutex check must not see our guard).
static INSTANCE_MUTEX: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

pub fn set_instance_mutex(handle: isize) {
    INSTANCE_MUTEX.store(handle, std::sync::atomic::Ordering::Relaxed);
}

fn release_single_instance() {
    let h = INSTANCE_MUTEX.swap(0, std::sync::atomic::Ordering::Relaxed);
    if h != 0 {
        unsafe {
            use windows::Win32::Foundation::{CloseHandle, HANDLE};
            let handle = HANDLE(h as *mut core::ffi::c_void);
            let _ = windows::Win32::System::Threading::ReleaseMutex(handle);
            let _ = CloseHandle(handle);
        }
    }
}

/// Absolute local time in seconds (Rata-Die day * 86400 + seconds of day).
fn abs_now() -> i64 {
    let (d, min, sec) = time::now_local();
    d.to_rd() * 86400 + min as i64 * 60 + sec as i64
}

fn abs_of(date: Date, minutes: u16) -> i64 {
    date.to_rd() * 86400 + minutes as i64 * 60
}

/// An azan that was swallowed because the user was busy; surfaces once they're free.
struct MutedAzan {
    label: String,
    time_s: String,
    expires: i64,
}

pub struct App {
    pub cfg: AppConfig,
    state: AppState,
    hwnd: HWND,
    tray: Option<TrayIcon>,
    widget: Option<Box<Widget>>,
    popup: Option<Box<Popup>>,
    popup_hidden_at: u32,
    times: [u16; 6],
    times_date: Date,
    hijri_day: u32,
    hijri_month: String,
    /// Hijri date of tomorrow (from the cached month array); None at month boundaries.
    hijri_next: Option<(u32, String)>,
    /// Pre-fetched next Gregorian month (last-2-days safety net for the Maghrib rollover).
    next_month: Option<aladhan::AladhanResponse>,
    /// Day the next-month pre-fetch was last attempted (one try per day).
    next_month_tried: Date,
    last_tooltip: String,
    last_moon_idx: usize,
    fired: HashSet<String>,
    fired_date: Date,
    muted_azan: Option<MutedAzan>,
    next_at: Option<i64>,
    taskbar_created_msg: u32,
    update_busy: bool,
    settings_open: bool,
    last_pill_icon_key: String,
}

const KEYS: [&str; 6] = ["fajr", "sunrise", "dhuhr", "asr", "maghrib", "isha"];

impl App {
    pub fn run() {
        unsafe {
            use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let cfg = AppConfig::load();
        i18n::set(&cfg.language);
        startup::heal_path();
        toast::init();
        theme::apply(&cfg.theme, &cfg.font_family, cfg.font_scale());

        let app = Box::new(Self {
            cfg,
            state: AppState::load(),
            hwnd: HWND::default(),
            tray: None,
            widget: None,
            popup: None,
            popup_hidden_at: 0,
            times: [0; 6],
            times_date: Date::new(1, 1, 1),
            hijri_day: 1,
            hijri_month: String::new(),
            hijri_next: None,
            next_month: None,
            next_month_tried: Date::new(1, 1, 1),
            last_tooltip: String::new(),
            last_moon_idx: usize::MAX,
            fired: HashSet::new(),
            fired_date: Date::new(1, 1, 1),
            muted_azan: None,
            next_at: None,
            taskbar_created_msg: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
            update_busy: false,
            settings_open: false,
            last_pill_icon_key: String::new(),
        });
        let app = Box::leak(app); // lives for the process; freed by the OS at exit

        let hwnd = window::create(
            w!("RustyPrayerTrayApp"),
            w!("RustyPrayerTray"),
            WINDOW_EX_STYLE::default(),
            WS_POPUP, // top-level, never shown
            0,
            0,
            0,
            0,
            None,
            app as *mut Self,
        )
        .expect("app window");
        app.hwnd = hwnd;
        app.tray = Some(TrayIcon::new(hwnd));
        app.build_widget();
        let mut popup = Popup::new(hwnd);
        popup.init_pin(app.cfg.popup_pinned, app.cfg.popup_x, app.cfg.popup_y);
        app.popup = Some(popup);

        app.recompute();
        app.data_tick();
        app.update_tray_icon();
        if app.cfg.popup_pinned {
            app.show_popup();
        }
        unsafe {
            SetTimer(Some(hwnd), TIMER_POS, 30_000, None);
            SetTimer(Some(hwnd), TIMER_DATA, 20_000, None);
            SetTimer(Some(hwnd), TIMER_WATCH, 1_000, None);
        }

        // First run: detect off-thread, then open Settings prefilled (port of FirstRunDetect).
        if AppConfig::is_first_run() {
            let target = hwnd.0 as isize;
            std::thread::spawn(move || {
                let loc = Box::new(crate::services::location::detect());
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(target as *mut _)),
                        WM_FIRSTRUN_LOC,
                        WPARAM(0),
                        LPARAM(Box::into_raw(loc) as isize),
                    );
                }
            });
        }

        // Message loop.
        use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, TranslateMessage, MSG};
        let mut msg = MSG::default();
        unsafe {
            while GetMessageW(&mut msg, None, 0, 0).into() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    fn today(&self) -> Date {
        let (date, _, _) = time::now_local();
        date
    }

    fn build_widget(&mut self) {
        self.widget = None; // drop the old window/hooks first
        let mut w = Widget::new(self.hwnd, self.cfg.monitor_device_name.clone());
        w.anchor_right = !self.cfg.widget_anchor.eq_ignore_ascii_case("Left");
        w.offset = self.cfg.widget_offset;
        w.hide_on_fullscreen = self.cfg.hide_on_fullscreen;
        w.locked = self.cfg.lock_widget;
        self.widget = Some(w);
    }

    fn recompute(&mut self) {
        // Drop the pre-fetch: it belongs to the previous month/method/location.
        self.next_month = None;
        self.next_month_tried = Date::new(1, 1, 1);
        let today = self.today();
        let response = aladhan::fetch_month(
            self.cfg.latitude,
            self.cfg.longitude,
            today.year,
            today.month,
            self.cfg.calculation_method,
        );
        if let Some(resp) = response {
            if let Some(day) = aladhan::find_by_date(&resp, &today) {
                self.times = aladhan::times_from_day(day);
                let (hj_day, hj_month) = aladhan::hijri_from_day(day);
                self.hijri_day = hj_day;
                self.hijri_month = hj_month;
                let tomorrow = today.add_days(1);
                self.hijri_next = aladhan::find_by_date(&resp, &tomorrow).map(aladhan::hijri_from_day);
                aladhan::save_to_cache(
                    self.cfg.latitude,
                    self.cfg.longitude,
                    today.year,
                    today.month,
                    self.cfg.calculation_method,
                    &resp.data,
                );
            }
        }
        self.times_date = today;
    }

    fn ensure_today(&mut self) {
        if self.times_date != self.today() {
            self.recompute();
        }
    }

    /// Recompute and re-arm after a clock/timezone change, resume, or manual "Refresh now".
    fn refresh_now(&mut self) {
        self.recompute();
        self.fired.clear();
        self.fired_date = self.today();
        self.data_tick();
    }

    fn data_tick(&mut self) {
        if self.times_date != self.today() {
            self.recompute();
            self.fired.clear();
            self.fired_date = self.today();
            self.update_tray_icon();
        }
        self.maybe_prefetch_next_month();
        self.render_tick();
        self.check_notifications();
    }

    /// The Hijri day used everywhere for display: before Maghrib it is today's date,
    /// after Maghrib the Islamic day has already rolled over to tomorrow.
    fn get_current_hijri_date(&self) -> (u32, String) {
        let (_, now_min, _) = time::now_local();
        let maghrib = self.times[5];
        if maghrib > 0 && now_min >= maghrib as u32 {
            // Tomorrow within the cached Gregorian month.
            if let Some(next) = &self.hijri_next {
                return (next.0, next.1.clone());
            }
            // Boundary evening: tomorrow is in the next Gregorian month.
            let tomorrow = self.today().add_days(1);
            if let Some(day) = self
                .next_month
                .as_ref()
                .and_then(|r| aladhan::find_by_date(r, &tomorrow))
            {
                return aladhan::hijri_from_day(day);
            }
            // No data at all: advance locally (capped, self-corrects at the midnight recompute).
            if self.hijri_day < 30 {
                return (self.hijri_day + 1, self.hijri_month.clone());
            }
        }
        (self.hijri_day, self.hijri_month.clone())
    }

    /// Last 2 days of the Gregorian month: fetch next month off-thread once per day, so the
    /// Maghrib rollover rarely falls back to the local increment.
    fn maybe_prefetch_next_month(&mut self) {
        if self.next_month.is_some() || self.next_month_tried == self.today() {
            return;
        }
        let today = self.today();
        let dim = days_in_month(today.year, today.month);
        if dim.saturating_sub(today.day) > 2 {
            return;
        }
        let (ny, nm) = if today.month == 12 { (today.year + 1, 1) } else { (today.year, today.month + 1) };
        let (lat, lng) = (self.cfg.latitude, self.cfg.longitude);
        let method = self.cfg.calculation_method;
        let target = self.hwnd.0 as isize;
        self.next_month_tried = today;
        std::thread::spawn(move || {
            let resp = Box::new(aladhan::fetch_month(lat, lng, ny, nm, method));
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(target as *mut _)),
                    WM_NEXT_MONTH,
                    WPARAM(0),
                    LPARAM(Box::into_raw(resp) as isize),
                );
            }
        });
    }

    fn pos_tick(&mut self) {
        if let Some(w) = &mut self.widget {
            w.tick();
        }
        if let Some(at) = self.next_at {
            let s = at - abs_now();
            if s > -60 && s <= 60 {
                self.render_tick();
            }
        }
    }

    /// Watchdog: any fullscreen transition that fired no win-event self-heals here.
    fn watch_tick(&mut self) {
        if let Some(w) = &mut self.widget {
            w.tick();
        }
    }

    fn time_of(&self, key: &str) -> Option<u16> {
        KEYS.iter().position(|k| *k == key).map(|i| self.times[i])
    }

    /// Formats minutes-since-midnight per the C# PrayerPopup.Format.
    pub fn format_time(minutes: u16, use24: bool) -> String {
        let (h, m) = (minutes / 60, minutes % 60);
        if use24 {
            format!("{:02}:{:02}", h, m)
        } else {
            let h12 = match h % 12 {
                0 => 12,
                x => x,
            };
            format!("{}:{:02} {}", h12, m, i18n::am_pm(h as u32))
        }
    }

    fn format_countdown(secs: i64) -> String {
        if secs < 60 {
            format!("{}{}", 1, i18n::t("unit.s"))
        } else if secs < 3600 {
            format!("{}{}", secs / 60, i18n::t("unit.m"))
        } else {
            format!("{}h {:02}{}", secs / 3600, (secs % 3600) / 60, i18n::t("unit.m"))
        }
    }

    /// (key, target abs-seconds, countdown-string) — "now" during the prayer's own minute,
    /// else the plain countdown. Port of CurrentOrNext.
    fn current_or_next(&mut self) -> (Option<&'static str>, Option<i64>, String) {
        self.ensure_today();
        let now = abs_now();
        let today = self.today();
        for key in ORDER {
            let Some(ts) = self.time_of(key) else { continue };
            let at = abs_of(today, ts);
            if now < at {
                return (Some(key_static(key)), Some(at), Self::format_countdown(at - now));
            }
            if now < at + 60 {
                return (Some(key_static(key)), Some(at), "now".into());
            }
        }
        let tomorrow = today.add_days(1);
        let response = aladhan::fetch_month(
            self.cfg.latitude,
            self.cfg.longitude,
            tomorrow.year,
            tomorrow.month,
            self.cfg.calculation_method,
        );
        if let Some(resp) = response {
            if let Some(day) = aladhan::find_by_date(&resp, &tomorrow) {
                let times = aladhan::times_from_day(day);
                let fajr_at = abs_of(tomorrow, times[0]);
                (Some("fajr"), Some(fajr_at), Self::format_countdown(fajr_at - now))
            } else {
                (None, None, String::new())
            }
        } else {
            (None, None, String::new())
        }
    }

    /// Map the internal "now" sentinel to localized display text.
    fn shown_countdown(countdown: &str) -> String {
        if countdown == "now" {
            i18n::t("countdown.now").to_string()
        } else {
            countdown.to_string()
        }
    }

    /// Display-only refresh (no notifications).
    fn render_tick(&mut self) {
        let use24 = self.cfg.use24_hour;
        let (key, at, countdown) = self.current_or_next();
        self.next_at = at;
        let label = key.map(i18n::prayer).unwrap_or("?");
        // The countdown runs to the next prayer, which is also when the current window shuts.
        let urgency = at
            .map(|t| 1.0 - ((t - abs_now()).clamp(0, URGENT_SECS) as f32 / URGENT_SECS as f32))
            .unwrap_or(0.0);
        let time_str = key
            .and_then(|k| self.time_of(k))
            .map(|ts| Self::format_time(ts, use24))
            .unwrap_or_default();

        let tip = if countdown == "now" {
            format!("{} {} {}", i18n::t("tray.now"), label, time_str)
        } else {
            format!(
                "{} {} {} ({} {})",
                i18n::t("tray.next"),
                label,
                time_str,
                i18n::t("tray.in"),
                countdown
            )
        };
        let (hj_day, hj_month) = self.get_current_hijri_date();
        let hijri = format!("{} {}", hj_day, hj_month);
        let tip = format!("{}\n{}", tip, hijri);
        if let Some(tray) = &mut self.tray {
            if self.last_tooltip != tip {
                tray.set_tooltip(&tip);
                self.last_tooltip = tip;
            }
        }
        let shown = Self::shown_countdown(&countdown);
        if let Some(w) = &mut self.widget {
            w.set_data(label, &shown, urgency);
        }
        self.update_tray_icon();
        if self.popup.as_ref().map(|p| p.visible()).unwrap_or(false) {
            self.show_popup();
        }
    }

    /// Update tray + pill icons from current time (stateless).
    pub fn update_tray_icon(&mut self) {
        let (today, _, _) = time::now_local();
        let moon_phase_idx = moon_phase_idx(self.get_current_hijri_date().0);
        if let Some(tray) = &mut self.tray {
            if self.last_moon_idx != moon_phase_idx {
                tray.set_icon_bytes(MOON_ICONS[moon_phase_idx]);
                self.last_moon_idx = moon_phase_idx;
            }
        }
        if let Some(w) = &mut self.widget {
            let times = self.times;
            let now = abs_now();
            let base_key = {
                let fajr = abs_of(today, times[0]);
                let dhuhr = abs_of(today, times[2]);
                let asr = abs_of(today, times[3]);
                let maghrib = abs_of(today, times[4]);
                if now < fajr || now >= maghrib {
                    "daily_1"
                } else if now < dhuhr {
                    "daily_2"
                } else if now < asr {
                    "daily_3"
                } else {
                    "daily_4"
                }
            };
            if self.last_pill_icon_key != base_key {
                if let Some(idx) = base_key.strip_prefix("daily_").and_then(|s| s.parse::<usize>().ok()) {
                    let idx = idx.saturating_sub(1).min(3);
                    w.set_icon_bytes(DAILY_ICONS[idx]);
                }
                self.last_pill_icon_key = base_key.to_string();
            }
        }
    }

/// Prefer a rich Action Center toast; fall back to a tray balloon.
    fn notify(&mut self, title: &str, body: &str) {
        self.notify_ex(title, body, false);
    }

    /// `silent` suppresses the balloon's system ding; toasts are already silent by XML.
    fn notify_ex(&mut self, title: &str, body: &str, silent: bool) {
        if self.cfg.rich_toasts && toast::show(title, body) {
            return;
        }
        if let Some(tray) = &mut self.tray {
            tray.balloon(title, body, silent);
        }
    }

    /// The file the configured azan would play, or None when nothing is set up.
    fn azan_path(&self) -> Option<std::path::PathBuf> {
        let mode = if self.cfg.azan_mode == "Builtin" {
            audio::BUILTIN_ADHANS[0].0 // legacy config -> first builtin
        } else {
            self.cfg.azan_mode.as_str()
        };
        match mode {
            "None" | "" => None,
            "Custom" => self
                .cfg
                .azan_custom_path
                .as_deref()
                .filter(|p| !p.trim().is_empty())
                .map(std::path::PathBuf::from),
            id => audio::builtin_adhan_path(id),
        }
    }

    fn play_azan(&self) {
        if let Some(p) = self.azan_path() {
            audio::play(&p);
        }
    }

    /// None = free to make noise.
    fn busy(&self) -> Option<quiet::Busy> {
        self.cfg.mute_when_busy.then(quiet::state).flatten()
    }

    fn next_prayer_after(&self, date: Date, after: i64) -> Option<i64> {
        ORDER
            .iter()
            .filter_map(|k| self.time_of(k))
            .map(|ts| abs_of(date, ts))
            .filter(|t| *t > after)
            .min()
    }

    fn fired_key(&self, date: Date, tail: &str) -> String {
        format!("{:04}{:02}{:02}:{}", date.year, date.month, date.day, tail)
    }

    /// Edge-triggered, day-aware: each reminder/azan fires once, within ~2 min of its minute.
    fn check_notifications(&mut self) {
        let today = self.today();
        let now = abs_now();
        if self.fired_date != today {
            self.fired.clear();
            self.fired_date = today;
        }

        let in_window = |target: i64| {
            let s = now - target;
            (0..120).contains(&s)
        };

        // Determine if any adhan is firing right now – if so, suppress all reminders.
        let adhan_firing = ORDER
            .iter()
            .filter_map(|k| self.time_of(k))
            .map(|ts| abs_of(today, ts))
            .any(|at| in_window(at));

        for key in ORDER {
            if key == "sunrise" {
                continue;
            }
            let Some(ts) = self.time_of(key) else { continue };
            let at = abs_of(today, ts);
            let label = i18n::prayer(key);

            // Adhan notification (always allowed)
            if in_window(at) {
                let id = self.fired_key(today, key);
                if self.fired.insert(id) {
                    let time_s = Self::format_time(ts, self.cfg.use24_hour);
                    let body = i18n::f("balloon.timeBody", &[label, &time_s]);
                    let busy = self.busy();
                    self.notify_ex(i18n::t("balloon.timeTitle"), &body, busy.is_some());
                    self.muted_azan = None; // this prayer supersedes any older muted one
                    self.update_tray_icon();
                    if self.azan_path().is_some() {
                        if busy.is_some() {
                            self.muted_azan = Some(MutedAzan {
                                label: label.to_string(),
                                time_s,
                                expires: self
                                    .next_prayer_after(today, at)
                                    .unwrap_or(at + MUTED_AZAN_MAX_HOLD),
                            });
                        } else {
                            self.play_azan();
                        }
                    }
                }
            }

            // Reminder – only if no adhan is firing right now.
            if !adhan_firing && self.cfg.reminder_enabled && in_window(at - self.cfg.reminder_minutes as i64 * 60) {
                let id = self.fired_key(today, &format!("{key}:rem"));
                if self.fired.insert(id) {
                    let mins = self.cfg.reminder_minutes.to_string();
                    let time_s = Self::format_time(ts, self.cfg.use24_hour);
                    let body = i18n::f("balloon.reminderBody", &[label, &mins, &time_s]);
                    let busy = self.busy();
                    self.notify_ex(i18n::t("balloon.reminderTitle"), &body, busy.is_some());
                    if self.cfg.reminder_sound && busy.is_none() {
                        audio::play_reminder(&self.cfg);
                    }
                }
            }
        }

        // Catch-up line once the user is free; never a late replay of the adhan itself.
        if let Some(m) = self.muted_azan.take() {
            if now < m.expires {
                if self.busy().is_some() {
                    self.muted_azan = Some(m);
                } else {
                    let body = i18n::f("balloon.mutedBody", &[&m.label, &m.time_s]);
                    self.notify(i18n::t("balloon.mutedTitle"), &body);
                }
            }
        }

        // Eve-before nudge for tomorrow's Sunnah fast (persisted latch, fires once from Maghrib on).
        if self.cfg.sunnah_fast_reminder {
            if let Some(mts) = self.time_of("maghrib") {
                let date_s = format!("{:04}-{:02}-{:02}", today.year, today.month, today.day);
                if now >= abs_of(today, mts) && self.state.sunnah_fast_noticed != date_s {
                    if let Some(reason) = self.sunnah_fast_reason(today.add_days(1)) {
                        self.state.sunnah_fast_noticed = date_s;
                        self.state.save();
                        let name = Self::fast_reason_name(reason);
                        let body = i18n::f("balloon.fastBody", &[&name]);
                        self.notify(i18n::t("balloon.fastTitle"), &body);
                    }
                }
            }
        }

        // Friday nudges: Surah Al-Kahf at Fajr, Jumu'ah heads-up before Dhuhr.
        const JUMUAH_LEAD_MIN: i64 = 30;
        if self.cfg.friday_reminder && today.weekday() == 5 {
            if let Some(fts) = self.time_of("fajr") {
                if in_window(abs_of(today, fts)) {
                    let id = self.fired_key(today, "kahf");
                    if self.fired.insert(id) {
                        self.notify(i18n::t("balloon.jumuahTitle"), i18n::t("balloon.kahfBody"));
                    }
                }
            }
            if let Some(dts) = self.time_of("dhuhr") {
                if in_window(abs_of(today, dts) - JUMUAH_LEAD_MIN * 60) {
                    let id = self.fired_key(today, "jumuah");
                    if self.fired.insert(id) {
                        self.notify(i18n::t("balloon.jumuahTitle"), i18n::t("balloon.jumuahBody"));
                    }
                }
            }
        }

        // Icon transitions at sunrise and imsak (not covered by adhan loop).
        if let Some(ts) = self.time_of("sunrise") {
            let at = abs_of(today, ts);
            if in_window(at) {
                self.update_tray_icon();
            }
        }
        let fajr = self.time_of("fajr").unwrap_or(0);
        let imsak_at = abs_of(today, fajr.saturating_sub(10) as u16);
        if in_window(imsak_at) {
            self.update_tray_icon();
        }
    }

    /// Why `day` is a Sunnah fasting day (or None). Monday/Thursday only — hijri events removed.
    fn sunnah_fast_reason(&self, _day: Date) -> Option<&'static str> {
        match _day.weekday() {
            1 => Some("monday"),
            4 => Some("thursday"),
            _ => None,
        }
    }

    fn fast_reason_name(r: &str) -> String {
        match r {
            "monday" => i18n::t("fast.monday").to_string(),
            "thursday" => i18n::t("fast.thursday").to_string(),
            _ => i18n::event(r).to_string(),
        }
    }

    fn show_menu(&mut self) {
        // A modal dialog runs its own message loop that still dispatches tray clicks; suppress the
        // menu so it can't toggle/teardown state the open dialog is displaying.
        if self.settings_open {
            return;
        }
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let add = |flags, id: usize, text: &str| {
                let wide = window::utf16z(text);
                let _ = AppendMenuW(menu, flags, id, windows::core::PCWSTR(wide.as_ptr()));
            };
            add(MF_STRING, CMD_SHOW_TIMES, i18n::t("menu.showTimes"));
            add(MF_STRING, CMD_REFRESH, i18n::t("menu.refresh"));
            add(MF_SEPARATOR, 0, "");
            add(
                if startup::is_enabled() { MF_STRING | MF_CHECKED } else { MF_STRING },
                CMD_STARTUP,
                i18n::t("menu.startup"),
            );
            add(
                if self.cfg.lock_widget { MF_STRING | MF_CHECKED } else { MF_STRING },
                CMD_LOCK,
                i18n::t("menu.lockWidget"),
            );
            add(MF_STRING, CMD_SETTINGS, i18n::t("menu.settings"));
            add(MF_STRING, CMD_STOP_SOUND, i18n::t("menu.stopSound"));
            add(MF_STRING, CMD_CHECK_UPDATES, i18n::t("menu.checkUpdates"));
            add(MF_SEPARATOR, 0, "");
            add(MF_STRING, CMD_EXIT, i18n::t("menu.exit"));

            let mut pt = Default::default();
            let _ = GetCursorPos(&mut pt);
            // Required for the menu to dismiss when clicking elsewhere (classic tray-menu quirk).
            let _ = SetForegroundWindow(self.hwnd);
            // Exclude the taskbar strip: menus clip to the monitor (not work area), and the
            // taskbar z-band would hide any rows placed under it.
            let point_rect = RECT { left: pt.x, top: pt.y, right: pt.x, bottom: pt.y };
            let exclude = displays::at_point(pt)
                .map(|m| {
                    let (b, w) = (m.bounds, m.work);
                    if w.bottom < b.bottom {
                        RECT { left: b.left, top: w.bottom, right: b.right, bottom: b.bottom }
                    } else if w.top > b.top {
                        RECT { left: b.left, top: b.top, right: b.right, bottom: w.top }
                    } else if w.left > b.left {
                        RECT { left: b.left, top: b.top, right: w.left, bottom: b.bottom }
                    } else if w.right < b.right {
                        RECT { left: w.right, top: b.top, right: b.right, bottom: b.bottom }
                    } else {
                        point_rect
                    }
                })
                .unwrap_or(point_rect);
            let params = TPMPARAMS { cbSize: std::mem::size_of::<TPMPARAMS>() as u32, rcExclude: exclude };
            let rtl = if i18n::is_rtl() { TPM_LAYOUTRTL } else { Default::default() };
            let _ = TrackPopupMenuEx(
                menu,
                (TPM_RIGHTBUTTON | TPM_VERTICAL | rtl).0,
                pt.x,
                pt.y,
                self.hwnd,
                Some(&params),
            );
            let _ = DestroyMenu(menu);
        }
    }

    fn toggle_popup(&mut self) {
        let visible = self.popup.as_ref().map(|p| p.visible()).unwrap_or(false);
        if visible {
            if let Some(p) = &self.popup {
                p.hide();
            }
            self.popup_hidden_at = tick_count();
        } else if tick_count().wrapping_sub(self.popup_hidden_at) > 250 {
            self.show_popup();
        }
    }

    fn show_popup(&mut self) {
        self.ensure_today();
        let (next_key, _, countdown) = self.current_or_next();
        let use24 = self.cfg.use24_hour;
        let today = self.today();

        let mut rows = Vec::with_capacity(6);
        for (i, key) in KEYS.iter().enumerate() {
            rows.push(Row {
                label: i18n::prayer(key).to_string(),
                time: Self::format_time(self.times[i], use24),
                is_next: Some(*key) == next_key,
                is_sunrise: *key == "sunrise",
            });
        }

        let hijri = if self.cfg.show_hijri_date {
            let (hj_day, hj_month) = self.get_current_hijri_date();
            format!("{} {}", hj_day, hj_month)
        } else {
            String::new()
        };
        let event = self.todays_event();
        let fast = self.todays_fast();
        let shown = Self::shown_countdown(&countdown);
        let (rect, anchor_right) = self
            .widget
            .as_ref()
            .map(|w| (w.screen_rect(), w.anchor_right))
            .unwrap_or_default();
        let city = self.cfg.city.clone();
        if let Some(p) = &mut self.popup {
            p.show_times(&city, today, rows, &shown, rect, anchor_right, &hijri, &event, &fast);
        }
    }

    /// Popup line: today's special day — removed (hijri events require offline calculation).
    fn todays_event(&self) -> String {
        if !self.cfg.show_islamic_events {
            return String::new();
        }
        String::new()
    }

    /// Popup fast line: fast-day itself all day; else the eve notice from Maghrib onward.
    fn todays_fast(&self) -> String {
        if !self.cfg.sunnah_fast_reminder {
            return String::new();
        }
        let today = self.today();
        if let Some(r) = self.sunnah_fast_reason(today) {
            return i18n::f("fast.today", &[&Self::fast_reason_name(r)]);
        }
        if let Some(mts) = self.time_of("maghrib") {
            if abs_now() >= abs_of(today, mts) {
                if let Some(r) = self.sunnah_fast_reason(today.add_days(1)) {
                    return i18n::f("fast.tomorrow", &[&Self::fast_reason_name(r)]);
                }
            }
        }
        String::new()
    }

    fn command(&mut self, id: usize) {
        match id {
            CMD_SHOW_TIMES => self.toggle_popup(),
            CMD_REFRESH => self.refresh_now(),
            CMD_STARTUP => {
                if !startup::set_enabled(!startup::is_enabled()) {
                    let title = i18n::t("app.name").to_string();
                    self.notify(&title, i18n::t("msg.startupError"));
                }
            }
            CMD_LOCK => {
                self.cfg.lock_widget = !self.cfg.lock_widget;
                self.cfg.save();
                if let Some(w) = &mut self.widget {
                    w.locked = self.cfg.lock_widget;
                }
            }
            CMD_SETTINGS => self.run_settings(None),
            CMD_STOP_SOUND => audio::stop(),
            CMD_CHECK_UPDATES => self.check_updates(),
            CMD_EXIT => self.exit(),
            _ => {}
        }
    }

    /// Modal settings with reopen-loop: a language change closes with Retry so the dialog
    /// rebuilds in the new language/RTL. The opening snapshot is carried across reopens so
    /// Cancel still reverts to the true original. Port of AppHost.RunSettings.
    fn run_settings(&mut self, mut prefill: Option<DetectedLocation>) {
        if self.settings_open {
            return;
        }
        self.settings_open = true;
        if let Some(w) = &mut self.widget {
            w.pause();
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_POS);
            let _ = KillTimer(Some(self.hwnd), TIMER_DATA);
            let _ = KillTimer(Some(self.hwnd), TIMER_WATCH);
        }
        let orig = self.cfg.clone();
        loop {
            let taken = prefill.take();
            let result = settings::run(self, &orig, taken.as_ref());
            match result {
                SettingsResult::Retry => continue,
                SettingsResult::Ok => {
                    // Monitor change is the only thing live preview defers.
                    let current = self.widget.as_ref().and_then(|w| w.device_name()).unwrap_or("").to_string();
                    let wanted = self.cfg.monitor_device_name.clone().unwrap_or_default();
                    if !current.eq_ignore_ascii_case(&wanted) {
                        self.build_widget();
                    }
                    self.update_tray_icon();
                    break;
                }
                SettingsResult::Cancel => break,
            }
        }
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_POS, 30_000, None);
            SetTimer(Some(self.hwnd), TIMER_DATA, 20_000, None);
            SetTimer(Some(self.hwnd), TIMER_WATCH, 1_000, None);
        }
        if let Some(w) = &mut self.widget {
            w.resume();
        }
        self.data_tick();
        self.settings_open = false;
    }

    fn check_updates(&mut self) {
        if self.update_busy {
            return;
        }
        self.update_busy = true;
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let info = Box::new(update::fetch_latest());
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(hwnd as *mut _)),
                    WM_UPDATE_RESULT,
                    WPARAM(0),
                    LPARAM(Box::into_raw(info) as isize),
                );
            }
        });
    }

    fn msg_yes_no(&self, title: &str, body: &str) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{
            MessageBoxW, IDYES, MB_ICONINFORMATION, MB_RIGHT, MB_RTLREADING, MB_YESNO,
        };
        let t = window::utf16z(title);
        let b = window::utf16z(body);
        let style = MB_YESNO
            | MB_ICONINFORMATION
            | if i18n::is_rtl() { MB_RTLREADING | MB_RIGHT } else { Default::default() };
        unsafe {
            MessageBoxW(
                Some(self.hwnd),
                windows::core::PCWSTR(b.as_ptr()),
                windows::core::PCWSTR(t.as_ptr()),
                style,
            ) == IDYES
        }
    }

    fn open_url(url: &str) {
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let u = window::utf16z(url);
        unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                windows::core::PCWSTR(u.as_ptr()),
                None,
                None,
                SW_SHOWNORMAL,
            );
        }
    }

    fn on_update_result(&mut self, info: Option<UpdateInfo>) {
        self.update_busy = false;
        let title = i18n::t("app.name").to_string();
        let Some(info) = info else {
            self.notify(&title, i18n::t("update.error"));
            return;
        };
        if !update::is_newer(&info) {
            let (ma, mi, b) = update::current();
            let body = i18n::f("update.none", &[&format!("v{ma}.{mi}.{b}")]);
            self.notify(&title, &body);
            return;
        }
        let (ma, mi, b) = info.latest;
        let v = format!("v{ma}.{mi}.{b}");
        if !self.msg_yes_no(i18n::t("update.availableTitle"), &i18n::f("update.availableBody", &[&v])) {
            return;
        }
        let Some(asset) = info.asset_url.clone() else {
            Self::open_url(&info.url);
            return;
        };
        self.notify(&title, i18n::t("update.downloading"));
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let dest = std::env::current_exe()
                .ok()
                .and_then(|e| e.parent().map(|d| d.join("RustyPrayerTray-new.exe")));
            let ok = dest.as_deref().map(|d| update::download(&asset, d)).unwrap_or(false);
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(hwnd as *mut _)),
                    WM_UPDATE_DOWNLOADED,
                    WPARAM(ok as usize),
                    LPARAM(Box::into_raw(Box::new(info)) as isize),
                );
            }
        });
    }

    /// Swap the downloaded exe in and restart; any failure restores the original.
    fn on_update_downloaded(&mut self, ok: bool, info: UpdateInfo) {
        let title = i18n::t("app.name").to_string();
        let Some(exe) = std::env::current_exe().ok() else { return };
        let dest = exe.parent().map(|d| d.join("PrayerTray-new.exe")).unwrap_or_default();
        if !ok {
            Self::open_url(&info.url);
            return;
        }
        if !self.msg_yes_no(i18n::t("update.availableTitle"), i18n::t("update.restartAsk")) {
            let _ = std::fs::remove_file(&dest); // postponed -> discard; re-downloaded next check
            return;
        }
        let old = exe.with_extension("exe.old");
        let swap = (|| -> std::io::Result<()> {
            if old.exists() {
                std::fs::remove_file(&old)?;
            }
            std::fs::rename(&exe, &old)?;
            std::fs::rename(&dest, &exe)?;
            Ok(())
        })();
        if swap.is_err() {
            let _ = std::fs::remove_file(&dest);
            self.notify(&title, i18n::t("update.failed"));
            return;
        }
        // Spawn the new exe; undo the swap if that fails so the running process matches disk.
        if std::process::Command::new(&exe).spawn().is_ok() {
            release_single_instance();
            self.exit();
        } else {
            let _ = std::fs::rename(&exe, &dest);
            let _ = std::fs::rename(&old, &exe);
            let _ = std::fs::remove_file(&dest);
            self.notify(&title, i18n::t("update.failed"));
        }
    }

    fn exit(&mut self) {
        audio::stop();
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_POS);
            let _ = KillTimer(Some(self.hwnd), TIMER_DATA);
            let _ = KillTimer(Some(self.hwnd), TIMER_WATCH);
        }
        if let Some(tray) = &mut self.tray {
            tray.remove();
        }
        unsafe { PostQuitMessage(0) };
    }
}

fn key_static(key: &str) -> &'static str {
    ORDER.iter().find(|k| **k == key).copied().unwrap_or("fajr")
}

impl SettingsHost for App {
    fn cfg(&mut self) -> &mut AppConfig {
        &mut self.cfg
    }

    /// Re-apply the (mutated-in-place) config without raising windows. Port of AppHost.LivePreview.
    fn live_preview(&mut self) {
        theme::apply(&self.cfg.theme, &self.cfg.font_family, self.cfg.font_scale());
        if let Some(w) = &mut self.widget {
            w.anchor_right = !self.cfg.widget_anchor.eq_ignore_ascii_case("Left");
            w.offset = self.cfg.widget_offset;
            w.hide_on_fullscreen = self.cfg.hide_on_fullscreen;
            w.locked = self.cfg.lock_widget;
        }
        self.recompute();
        self.render_tick(); // display only — never fire azan/balloon from a settings change
        if let Some(w) = &mut self.widget {
            w.preview_reposition();
        }
    }



    

    fn update_tray_icon(&mut self) {
        self.update_tray_icon();
    }

    fn test_notify(&mut self, use_toast: bool) {
        let title = i18n::t("app.name");
        let body = i18n::t("toast.test");
        if use_toast && toast::show(title, body) {
            return;
        }
        if let Some(tray) = &mut self.tray {
            tray.balloon(title, body, false);
        }
    }
}

impl WindowHandler for App {
    fn message(&mut self, _hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_TIMER => {
                match wparam.0 {
                    TIMER_POS => self.pos_tick(),
                    TIMER_DATA => self.data_tick(),
                    TIMER_WATCH => self.watch_tick(),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_TRAY => {
                match lparam.0 as u32 {
                    WM_LBUTTONUP => self.toggle_popup(),
                    WM_RBUTTONUP => self.show_menu(),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                self.command(wparam.0 & 0xFFFF);
                Some(LRESULT(0))
            }
            WM_TIMECHANGE => {
                self.refresh_now();
                self.update_tray_icon();
                Some(LRESULT(0))
            }
            WM_SETTINGCHANGE => {
                // Follow Windows' light/dark flip only when the user left the theme on "Auto".
                if lparam.0 != 0 {
                    let s = unsafe { windows::core::PCWSTR(lparam.0 as *const u16).to_string() }.unwrap_or_default();
                    if s == "ImmersiveColorSet" && self.cfg.theme.eq_ignore_ascii_case("Auto") {
                        theme::apply(&self.cfg.theme, &self.cfg.font_family, self.cfg.font_scale());
                        if let Some(w) = &mut self.widget {
                            w.rerender();
                        }
                        self.data_tick();
                    }
                }
                None
            }
            WM_POWERBROADCAST => {
                if wparam.0 as u32 == PBT_APMRESUMESUSPEND || wparam.0 as u32 == PBT_APMRESUMEAUTOMATIC {
                    self.refresh_now();
                    self.update_tray_icon();
                }
                None // let DefWindowProc return TRUE for the broadcast
            }
            m if m == self.taskbar_created_msg && m != 0 => {
                if let Some(tray) = &mut self.tray {
                    tray.readd();
                }
                self.build_widget(); // Explorer restarted -> old owner window is gone
                self.data_tick();
                Some(LRESULT(0))
            }
            WM_WIDGET_CLICK => {
                self.toggle_popup();
                Some(LRESULT(0))
            }
            WM_WIDGET_MENU => {
                self.show_menu();
                Some(LRESULT(0))
            }
            WM_POPUP_PIN => {
                self.cfg.popup_pinned = wparam.0 != 0;
                self.cfg.save();
                Some(LRESULT(0))
            }
            WM_FIRSTRUN_LOC => {
                // SAFETY: the payload was `Box::into_raw`d by the worker thread and posted
                // exactly once via PostMessageW; this arm is its sole owner and takes it back.
                let loc = *unsafe { Box::from_raw(lparam.0 as *mut Option<DetectedLocation>) };
                self.run_settings(loc);
                Some(LRESULT(0))
            }
            WM_UPDATE_RESULT => {
                // SAFETY: same one-shot ownership transfer as WM_FIRSTRUN_LOC.
                let info = *unsafe { Box::from_raw(lparam.0 as *mut Option<UpdateInfo>) };
                self.on_update_result(info);
                Some(LRESULT(0))
            }
            WM_UPDATE_DOWNLOADED => {
                // SAFETY: same one-shot ownership transfer as WM_FIRSTRUN_LOC.
                let info = *unsafe { Box::from_raw(lparam.0 as *mut UpdateInfo) };
                self.on_update_downloaded(wparam.0 != 0, info);
                Some(LRESULT(0))
            }
            WM_NEXT_MONTH => {
                // SAFETY: same one-shot ownership transfer as WM_FIRSTRUN_LOC.
                let resp = *unsafe { Box::from_raw(lparam.0 as *mut Option<aladhan::AladhanResponse>) };
                if resp.is_some() {
                    self.next_month = resp;
                    // Paint immediately if we are already past Maghrib on the boundary evening.
                    self.render_tick();
                }
                Some(LRESULT(0))
            }
            WM_WIDGET_MOVED => {
                self.cfg.widget_offset = (wparam.0 as i32).clamp(0, 2000);
                // Settings mutates cfg in place for live preview and only writes on Save, so
                // saving here would make Cancel unable to undo the rest of the dialog.
                if !self.settings_open {
                    self.cfg.save();
                }
                Some(LRESULT(0))
            }
            WM_POPUP_MOVED => {
                let packed = lparam.0 as i64;
                self.cfg.popup_x = (packed & 0xFFFF_FFFF) as i32;
                self.cfg.popup_y = (packed >> 32) as i32;
                self.cfg.save();
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}
