# Changelog

All notable changes to PrayerTray are documented here.
Format based on [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Fixed

- **Multi-monitor fullscreen z-order bleed**: with a fullscreen app on Monitor A, clicking the
  taskbar on Monitor B no longer pops the pill over the game. Full write-up:
  [docs/ZORDER_POSTMORTEM.md](docs/ZORDER_POSTMORTEM.md).
  - Z-order is now **stateless**: `WS_EX_TOPMOST` + taskbar ownership are set once at creation
    and never toggled (all `HWND_NOTOPMOST` band switches removed) — this also fixes the pill
    falling *behind* the Win11 XAML taskbar.
  - Fullscreen detection switched to a per-monitor **occupancy predicate**
    (`is_fullscreen_on_monitor`): a visible root window whose rect contains the monitor bounds,
    excluding shell/self/owned/DWM-cloaked windows — completely focus-independent.
  - All pill visibility funnels through one choke point (`Widget::update_visibility`,
    edge-triggered latch) — the only `ShowWindow` path for the pill.
  - Taskbar-click burying fixed by re-asserting the top of the band (`raise()`:
    `HWND_TOPMOST` with `NOMOVE|NOSIZE|NOACTIVATE`) instead of toggling z-order.
  - Removed the dead foreground-based `is_fullscreen_app_on`.

### Added

- **Aladhan API prayer times**: the current month is cached to `%APPDATA%\PrayerTray\cache.json`
  (valid for 7 days; stale cache serves when offline), and the tooltip shows the Hijri date from
  the API response.
- **Calculation-method dropdown** (Settings → Location): Diyanet (default), MWL, ISNA, Umm
  al-Qura, Egyptian General Authority, University of Karachi, Moonsighting Committee. Changing it
  invalidates the cache and re-fetches immediately; the method is now part of the cache key.
- **Hijri rollover at Maghrib**: the displayed Hijri date, moon-phase tray icon, and popup flip at
  Maghrib (the Islamic day start), not midnight. The date is taken from tomorrow's entry in the
  cached month array after Maghrib, with a next-month pre-fetch on the last two days of the
  Gregorian month as a safety net (`App::get_current_hijri_date`).
- **Moon-phase tray icon** derived from the API's Hijri day number via the 8-phase lunar
  mapping (`moon_1`…`moon_8`: day 1-2 new, 3-6 waxing crescent, 7-8 first quarter, 9-12 waxing
  gibbous, 13-15 full, 16-18 waning gibbous, 19-21 last quarter, 22-30 waning crescent).
- **Daily prayer icon** on the pill, following the day's arc (night / sunrise / midday /
  sunset), scaled to text height and swapped at most 4x/day.
- 1 s visibility watchdog timer (`TIMER_WATCH`) that self-heals fullscreen transitions which
  fired no win-event, plus a visibility evaluation at pill creation.
- `PRAYERTRAY_ZDEBUG=1` env var: appends every pill hide/show transition (trigger event +
  covering window class) to `%TEMP%\prayertray_zdebug.log`.

### Changed

- Settings UI overhaul: Location (City, Lat/Long, Detect, Pick Map, **Refresh Data Now**,
  Calculation Method), Appearance (Language, Theme, Font, Font Size, Widget Side/Gap/Monitor,
  24-hour clock, Hide over Fullscreen), Religious (Hijri date, Islamic events, Sunnah fast
  & Friday reminders), Notifications (rich toasts, reminders, sounds, azan, mute-when-busy).
- Pill renders a single hardcoded layout — **Daily icon + Name + Countdown** (the daily icon is
  always shown; the old "Taskbar pill icon" toggle and the layout picker were removed) — with
  offline GDI+ rendering to a buffer and a pure bitblt paint.
- Performance: tooltip and icons update only on actual changes; timer interval 15 s → 20 s;
  countdown redraw guarded by an equality check.
- i18n: removed dead strings, fixed `card.*` ordering for binary-search lookup, added
  `card.system` / `card.trayIcons`.
- Build: serde features trimmed; `make-portable.ps1` builds release and copies to `dist/`.

### Removed

- **Offline calculation engine**: `src/calc/*` (PrayTimes engine, Umm al-Qura Hijri
  conversion, Islamic events, data tables) and its config fields (`method`, `asr`,
  `high_lats`, `hijri_adjust`, `timezone_hours`, imsak adjust) and methods
  (`calc_method()`, `asr_juristic()`, `high_lat()`, `time_adjust()`, `resolve_timezone()`,
  `Offsets`). The six per-prayer ± minute adjustments remain.
- **Network meters** (download/upload speed, ping, per-day data usage, CPU/memory, VPN
  tunnel marker) and their backing services and `native/net.rs` adapter layer — the pill is
  prayer-only now.
- Settings: the Calculation tab, the pill-layout dropdown (single layout by design), the
  daily-icon / moon-icon / tray-icon checkboxes, the Hijri-date adjuster, and the settings
  font-size option.
- Stale test suites (`net_logic`, `calc_reference`, `config_roundtrip`) and their fixtures —
  they targeted the removed offline engine and net-meter services.
- `WHEN-BROKEN.txt` — its content now lives in
  [docs/ZORDER_POSTMORTEM.md](docs/ZORDER_POSTMORTEM.md) and
  [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md).
