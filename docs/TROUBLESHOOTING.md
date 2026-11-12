# Troubleshooting & Known Limitations

Behaviors that are *not* bugs but may look like them. For the deep-dive on the hardest bug
in the project's history, see [ZORDER_POSTMORTEM.md](ZORDER_POSTMORTEM.md).

---

## 1. Moon-phase icon on first run
The moon-phase tray icon is derived from the Hijri day; the icon-state cache starts
uninitialized, so the very first tick may pick an edge-case index. **Impact:** none of
consequence — the state self-corrects on the first refresh cycle.

## 2. Pill icon state after rapid toggling
Rapidly toggling the daily-icon setting off and on can desynchronize the pill's cached
icon key from the config. **Workaround:** restart the app to reset icon state.

## 3. GDI+ bitmap churn
Pill icons are rebuilt from `HICON`s only at prayer boundaries or Hijri day changes — never
per tick — so GDI+ handle churn is bounded. Repeated create/dispose in a tight loop could
exhaust handles temporarily, but no code path does this. **Impact:** none in practice.

## 4. Stale times after a clock rollback
If the system clock is set backward past midnight, the change is picked up on the next
20-second tick, so prayer times from the previous day may linger briefly.
**Impact:** up to ~20 s of stale display.

## 5. Offline mode
When offline and the cache is expired (older than 7 days), no new data can be fetched; the
app keeps showing the last known times from memory and the tooltip's Hijri date goes stale.
**Impact:** times are accurate only as long as the cache.

## 6. Settings combo text on first open
Owner-drawn combo boxes may not paint their text until hovered. A forced-repaint pass
(`InvalidateRect` over all owner-drawn combos of the current page) was added to `load_values()`;
if you still see blank dropdowns on a *first* open, hover over them once — subsequent opens
are fine.

## 7. Countdown precision
The countdown displays HH:mm only; the worst-case staleness is ~20 s (timer interval).
Minute-level precision is a deliberate design choice.

## 8. Single pill layout
Only one layout is selectable in settings — **Name + Countdown + Daily Icon**. The renderer
implements the full five-layout set, but the picker exposes one for now. No workaround;
by design.

## 9. Custom sound formats
Custom reminder sounds must be `.mp3` or `.wav` (MCI auto-detects: `.wav` → `waveaudio`,
everything else → `mpegvideo`). Unsupported formats fail silently.

## 10. Mixed-DPI monitor moves
The widget scales per-window DPI (`GetDpiForWindow`). Moving it between monitors with
different scaling factors causes a brief layout recalculation.

## 11. OS compatibility
Win32 APIs (`Shell_NotifyIconW`, GDI+, WinHTTP) require **Windows 7 SP1 or later**.
Windows XP and earlier are not supported.

## 12. Old config files
Config files from the offline-calculation era (`method`, `asr`, `high_lats`,
`fajr_adjust`, `dhuhr_adjust`, …) load successfully: missing fields fall back to defaults
via `#[serde(default)]`. The removed settings are simply ignored — times now come from the
Aladhan API (Diyanet, method 13).

## 13. Embedded assets
`app.ico` and the two built-in azan files (`azan-makkah.mp3`, `azan-madinah.mp3`) are
embedded in the executable. If the exe is rebuilt without them, built-in azan playback
fails — custom azan files configured in settings still work.

## 14. Corrupted config
A malformed `config.json` falls back to defaults; user settings in that file are lost.
**Workaround:** back up `config.json` before manual edits.

## 15. Data locations
Installed mode stores everything under `%APPDATA%\PrayerTray\` (config, state, cache).
Portable mode (a `portable` marker or `config.json` next to the exe) keeps all data
self-contained beside the executable.

---

## Testing notes

- Build: `cargo build --release --target x86_64-pc-windows-msvc --locked`
- Tests: `cargo test --target x86_64-pc-windows-msvc --test quiet_logic --test atomic_write --test i18n_integrity`
- The build should emit only benign dead-code warnings.
- Runtime smoke test: tray icon appears, pill shows times + countdown, settings opens with
  all four pages rendering correctly.
- Z-order regression test: start a fullscreen app, click another monitor's taskbar, verify
  the pill stays hidden (set `PRAYERTRAY_ZDEBUG=1` and check
  `%TEMP%\prayertray_zdebug.log` — see the [post-mortem](ZORDER_POSTMORTEM.md)).
