# PrayerTray — Architecture & Internals (fork)

Design notes for contributors. For user-facing docs see the [README](README.md). This document
replaces the upstream `ARCHITECTURE.md`, which described the pre-fork z-order design — a design
that worked on single-monitor setups but bled over fullscreen apps on multi-monitor setups (see
[docs/ZORDER_POSTMORTEM.md](docs/ZORDER_POSTMORTEM.md) for the full post-mortem).

## 1. Project Shape

Single Rust crate in `rust/` targeting `x86_64-pc-windows-msvc`. Dependencies are deliberately
minimal: the `windows` crate (feature-gated Win32/WinRT bindings) plus `serde`/`serde_json` for
the config contract — nothing else. Rendering is the **GDI+ flat C API** behind RAII wrappers
(`ui/gdip.rs`); text metrics and Arabic/Urdu shaping therefore match the original C# build
(System.Drawing is a thin layer over the same flat API). Release profile: `opt-level="z"`, fat
LTO, `panic="abort"`, stripped → a single ~3.5 MB exe with no runtime to install. Idle footprint
~30 MB RAM.

**Prayer times come from the Aladhan API** (`https://api.aladhan.com/v1/calendar/…`), with a
**user-selectable calculation method** (default `method=13` — Diyanet; also MWL, ISNA, Umm
al-Qura, Egyptian, Karachi, Moonsighting — see `config::CALC_METHODS`). The current month is
cached to `%APPDATA%\PrayerTray\cache.json` and the cache is valid for **7 days**, keyed by
lat/lng + year/month + calculation method (a method change forces exactly one re-fetch). When
offline or expired-stale, the last known times are served from memory/cache. The network is
touched *only* by explicit user actions (location detect, update check) and by the monthly
cache refresh — prayer *countdown* logic itself never blocks on the network.

## 2. Golden Win32 Insights

### 2.1 Why `SetParent` into the taskbar fails (and what works instead)

There is **no supported way to embed a visible widget *into* the Win11 taskbar**, and this was
proven, not assumed:

- **Deskbands** (the old toolbar API) were removed in Windows 11.
- **Reparenting** a window into `Shell_TrayWnd` (`SetParent`) *does* embed it — but it stays
  invisible. Enumerating the taskbar's children shows a
  `Windows.UI.Composition.DesktopWindowContentBridge` surface spanning the whole bar; it is
  composited **over** GDI child windows regardless of z-order, so an embedded pill is buried
  under it no matter what.

The only option — the same one every other Win11 taskbar widget uses — is a **top-level
`WS_POPUP` overlay** (no-activate, `WS_EX_TOOLWINDOW`, **owned by `Shell_TrayWnd`** via
`GWLP_HWNDPARENT` so it rides the taskbar's z-band) with `WS_EX_TOPMOST` set once at creation.

Corollary: popup menus clip to the *monitor*, not the work area, and the taskbar band would hide
any rows underneath it — so `show_menu` passes `TPMPARAMS.rcExclude` = the taskbar strip with
`TPM_VERTICAL`.

### 2.2 Atomic writes for every persisted store

Every store — `config.json`, `state.json`, `usage.json`-class files, and the adhan/tone files
cached under `%TEMP%` — is written through `util::write_atomic`: write to a temp sibling, flush,
then rename over the target. A plain `fs::write` **truncates first**; if the process dies
mid-write (crash, power loss, update swap), each store fails differently and all failures are
bad:

- a truncated `config.json` silently loads as defaults — user settings gone;
- a torn data/usage store loses its accumulated history;
- a torn cached mp3 is still *non-empty*, so a naive `Path::exists` check keeps approving it and
  the azan silently dies until the temp dir is cleared.

Temp-sibling-then-rename makes every store either fully old or fully new, on both NTFS and FAT.

### 2.3 Toast notifications without package identity

A plain Win32 process cannot post Action Center toasts. The recipe:

1. `SetCurrentProcessExplicitAppUserModelID("DynamicEG.PrayerTray")` at startup — the process
   claims an AppUserModelID.
2. A Start Menu shortcut carries `PKEY_AppUserModel_ID` (created via `IShellLinkW` +
   `IPropertyStore`). Windows matches the toast's AUMID against installed shortcuts to decide
   whether the app "exists".

The same AUMID as v1.x is reused, so upgraders don't get duplicate shortcuts and old
notifications group correctly. Rich toasts are the primary path; the tray balloon is the
fallback when the shortcut is missing (e.g. green/portable installs).

### 2.4 Meeting detection (muting the azan over Zoom/Teams)

`MuteWhenBusy` (on by default) silences reminder tones and the azan while the user is busy. Two
probes back it, because either alone has a blind spot:

- **`SHQueryUserNotificationState()`** — covers exclusive D3D fullscreen, presentation mode, and
  Do-not-disturb (`QUNS_QUIET_TIME`). It does **not** see a windowed video call, which is the
  case that actually matters.
- **The microphone `ConsentStore`** (`HKCU\…\CapabilityAccessManager\ConsentStore\microphone`,
  plus its `NonPackaged` subtree): an app currently holding the mic has
  `LastUsedTimeStart != 0` and `LastUsedTimeStop == 0`. This is exactly how Windows itself
  drives the "mic in use" tray glyph, and it catches windowed Teams/Zoom/Meet regardless of
  window geometry. A call outranks every other busy reason.

`QUNS_NOT_PRESENT` (locked, screensaver) is deliberately *not* busy — that's away-from-desk, and
the azan should still play. An unrecognized future state falls through to "free": failing open
is better than a permanently silent app. Only the sound is suppressed; the notification still
posts (Windows queues it under DND on its own), and a swallowed azan surfaces later as a single
silent catch-up line — replaying the adhan an hour late would be worse than staying quiet.

## 3. The Real Z-Order Architecture (the multi-monitor fix)

The upstream design (`WS_EX_TOPMOST` + taskbar ownership + a foreground-window fullscreen hook)
was correct for **single-monitor** setups but bled over on multi-monitor ones: clicking the
taskbar on Monitor B made the *taskbar* the foreground window, the foreground-based fullscreen
check concluded "safe", and the pill was re-shown over a fullscreen game on Monitor A. Toggling
`HWND_NOTOPMOST` at runtime made things worse — it demotes the window out of its working band
and it falls behind the Win11 XAML taskbar.

This fork's z-order is **entirely stateless**:

- **Z-order never changes after creation.** `WS_EX_TOPMOST` and the taskbar owner are set
  exactly once. There are zero `HWND_NOTOPMOST` calls in the codebase.
- **One choke point owns visibility.** `Widget::update_visibility` is the only code path that
  can show the pill, and it is edge-triggered (an internal `suppressed` latch) — repeated
  events are idempotent. Hiding is `SW_HIDE`; showing is `SW_SHOWNA`.
- **Fullscreen is per-monitor occupancy, focus-independent.**
  `taskbar::is_fullscreen_on_monitor(bounds)` enumerates top-level windows (`EnumWindows`) and
  reports the first *root* window (`GetAncestor(GA_ROOT)`) that is not shell/self
  (`Shell_TrayWnd`, `Shell_SecondaryTrayWnd`, `WorkerW`, `Progman`, the app's own classes), is
  not DWM-cloaked (`DWMWA_CLOAKED`), and whose rect **contains** the widget's monitor bounds.
  A game on Monitor A keeps the pill hidden even while the user clicks the taskbar on Monitor B.
- **`GetForegroundWindow` is completely ignored** by the visibility decision. Focus is not a
  signal; occupancy is.
- **The raise trick.** Clicking the taskbar buries the pill, and the pill must come back
  *without* touching its band: `Widget::raise()` calls
  `SetWindowPos(HWND_TOPMOST, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)` — it re-asserts the
  top of the band the pill already lives in, driven by `EVENT_OBJECT_REORDER` scoped to the
  taskbar's thread (16 ms debounce). Because the pill is hidden during fullscreen, the raise
  never runs there and cannot bleed over a game.

A global `EVENT_SYSTEM_FOREGROUND` hook, the taskbar-thread reorder hook, a 1 s watchdog timer,
and a creation-time evaluation all funnel into the single choke point. `IsWindowVisible` on the
taskbar survives only as a supplementary *hide* net (genuine hides: RDP etc.) — it can never
gate a show, because it is a style bit, not an occlusion check.

Full diagnosis, the five failed upstream attempts, and the scenario walkthrough:
[docs/ZORDER_POSTMORTEM.md](docs/ZORDER_POSTMORTEM.md).

## 4. Threading & Resilience

One UI thread owns all windows, timers, hooks, and GDI+. Network work (update check/download,
IP geolocation, the Aladhan fetch) runs on `std::thread::spawn` and posts results back with
`PostMessageW`, passing `Box::into_raw` payloads in `lparam` — the wndproc trampoline
(`ui/window.rs`) routes messages to a boxed handler via `GWLP_USERDATA`, and the app window
takes ownership of posted heap payloads with `Box::from_raw`.

A hidden **top-level** (not message-only) window receives the broadcasts that drive
self-healing:

- `WM_POWERBROADCAST` (resume from sleep → refresh times),
- `WM_TIMECHANGE` (clock/timezone change → re-arm notifications),
- `WM_SETTINGCHANGE` (Windows light/dark flip when the theme is on Auto),
- the `TaskbarCreated` broadcast (Explorer restarted → re-add the tray icon and rebuild the
  pill, because the old owner window is gone).

Timers: 30 s reposition/countdown (`TIMER_POS`), 20 s render + notification checks
(`TIMER_DATA`), and a 1 s pill-visibility watchdog (`TIMER_WATCH`) that self-heals any
fullscreen transition that fired no win-event. Hooks handle the instant cases; the watchdog
handles everything else.

The settings dialog is native Win32 children (combo/edit/checkbox, owner-drawn buttons) themed
via `WM_CTLCOLOR*` + `SetWindowTheme("DarkMode_CFD")`, run in a nested modal loop; full RTL via
`WS_EX_LAYOUTRTL`. A single-instance mutex guards the process (dev builds opt out with
`PRAYERTRAY_DEV_MUTEX=1`), and a panic hook writes `error.log` next to the config before
showing a message box.

## 5. Fork-Specific Features

Differences from upstream, all user-visible:

- **Diyanet prayer times** — the Aladhan API integration defaults to `method=13` (Diyanet İşleri
  Başkanlığı) with a 7-day cache; the method is user-selectable in Settings (Location page).
  The displayed Hijri date rolls over at **Maghrib** (the Islamic day start), not midnight —
  `App::get_current_hijri_date` compares the clock to Maghrib and takes tomorrow's entry from
  the cached month array, with a next-month pre-fetch safety net on the last two days of the
  Gregorian month.
- **Moon-phase tray icon** — the tray icon is selected from the (Maghrib-adjusted) Hijri day
  number via an 8-phase lunar mapping (`moon_1` … `moon_8`: new → waxing crescent → first
  quarter → waxing gibbous → full → waning gibbous → last quarter → waning crescent), so the
  icon tracks the lunar month instead of being static.
- **Daily prayer icons** — the pill shows a small icon that follows the day's arc (night before
  Fajr, sunrise, midday, sunset), swapped at most four times a day.
- **Five pill layouts** — the renderer implements all five upstream heads: *name + time +
  countdown*, *stacked time-over-countdown*, *name + countdown*, *countdown-only*, and
  **Name + Icon + Countdown** (`nameCountAndIcon`), which is this fork's default and the only
  one the settings page currently exposes.
- **The net meters are gone** — upstream's download/upload speed, ping, per-day data usage,
  CPU/memory meters and the VPN tunnel marker were removed completely (services *and* the
  `native/net.rs` adapter-probing layer) to keep the app ultra-lightweight: no `GetIfTable2`
  polling, no ICMP probe thread, no per-day usage store. What remains in the pill is prayer
  information only.
- **Hide-over-fullscreen that actually works on multi-monitor** — see section 3.

## Files (`rust/src/`)

| File | Role |
|------|------|
| `config.rs` | serde config load/save/sanitize, v1-compatible schema |
| `datetime.rs` | Civil-date ↔ Rata Die helpers (no chrono) |
| `i18n/` | Language runtime + generated string catalog (en/ar/fr/tr/ur/id, RTL) |
| `native/taskbar.rs` | Taskbar find/geometry, per-monitor fullscreen occupancy, DPI |
| `native/displays.rs` | Monitor enumeration + CCD friendly names |
| `native/startup.rs` | HKCU Run key + StartupApproved handling |
| `native/reg.rs` | Registry open/query/enumerate wrappers (RAII key handle) |
| `native/quiet.rs` | Busy detection: shell notification state + mic-in-use probe |
| `native/time.rs` | Local time + DST-aware UTC offset + monotonic tick |
| `ui/gdip.rs` | RAII GDI+ wrappers — the only unsafe-heavy drawing zone |
| `ui/window.rs` | Window-class/wndproc trampoline plumbing |
| `ui/widget.rs` | The overlay pill — stateless z-order, per-monitor fullscreen hide, DPI, RTL, drag |
| `ui/popup.rs` | Today's-times popup (pin, drag, saved position) |
| `ui/settings.rs` + `ui/controls.rs` | Settings dialog + themed native controls |
| `ui/icon.rs` | Tray icon, tooltip, balloon fallback |
| `ui/theme.rs` | Palettes (Dark/Light/Midnight/Slate/Warm) + Auto-follow + colour blending |
| `services/aladhan.rs` | Aladhan API client + 7-day cache + Hijri extraction |
| `services/http.rs` | WinHTTP GET/download (no TLS crate needed) |
| `services/audio.rs` | Azan/reminder playback via MCI + synthesized tones |
| `services/toast.rs` | Action Center toasts + AUMID shortcut |
| `services/location.rs` | WinRT geolocation → IP fallback + map-link parsing |
| `services/update.rs` | GitHub release check + in-place exe swap |
| `app.rs` | Orchestrator: pill + tray + popup + timers + notification engine |
| `main.rs` | Entry point: single-instance mutex, panic log, `.old` cleanup |
| `util.rs` | Atomic file replace, shared by every persisted store |
