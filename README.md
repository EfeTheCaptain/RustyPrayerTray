# RustyPrayerTray 🕌🦀

A native Windows taskbar prayer widget written in Rust, based on the foundation of the original [PrayerTray](https://github.com/Dev-Mohamed-Ali/PrayerTray) by [dev ghoniem](https://github.com/Dev-Mohamed-Ali).

Because the calculation engine, UI rendering, and window management have been heavily modified, this project is published as a standalone repository rather than a standard GitHub fork. This avoids merge conflicts with upstream changes that no longer apply to this heavily diverged codebase.

<div align="center">
  <img width="174" height="86" alt="RustyPrayerTray Icon" src="https://github.com/user-attachments/assets/639dab80-b457-47fd-bfaa-69f7825079c8" />
</div>

---

## ⚡ True Ultra-Lightweight

No background polling, no Electron, no hidden bloat. Because it uses native Win32 GDI+ rendering and the Aladhan API's 7-day caching, the app idles at **0% CPU** and uses just **~5.3 MB of RAM**.

<div align="center">
  <img src="https://github.com/user-attachments/assets/b6ed4996-fe06-4ec8-a463-e4609c816cc9" alt="Task Manager showing 5.3MB RAM usage" width="800" />
</div>

---

## 🏗️ Original Foundation vs. This Fork

The original application provided an excellent Win32/GDI+ shell, a robust settings framework, and a complete notification/Azan system. This fork retains that foundation but completely replaces the core data engine, UI layout, and performance architecture.

<div align="center">
  <img src="https://github.com/user-attachments/assets/f9dbdb39-db79-43da-95ea-e9f877f2556c" alt="Original vs Fork" width="600" />
</div>

### What We Changed
*   **Aladhan API Integration:** The offline PrayTimes.org calculation was replaced with the Aladhan API. It fetches a full month of data in one request, caches it for 7 days, and handles offline fallbacks gracefully.
*   **Calculation Method Dropdown:** The original settings menu for choosing calculation methods was removed during the API rewrite, then intentionally restored. The dropdown now maps directly to the Aladhan API's method IDs, defaulting to Diyanet (Method 13).
*   **Removed Net Meters:** The network speed, ping, CPU/RAM, VPN, and data usage meters were completely stripped out to keep the app ultra-lightweight and focused purely on prayer times.
*   **Sunrise Countdown Fix:** The countdown logic now targets Sunrise as the end of the Fajr prayer window. The original app skipped Sunrise entirely and counted down to Dhuhr, which is functionally useless once the Fajr window has closed.
*   **Hardcoded Streamlined Layout:** All multiple taskbar layout options, fine-tuning menus, and the redundant "Taskbar pill icon" toggle were removed. The pill now uses a single, hardcoded layout: Daily Icon + Prayer Name + Countdown.

---

## ✨ Features Added in This Fork

### 🌇 Maghrib Hijri Rollover
In Islamic tradition, the day changes at sunset. The app calculates the exact Maghrib time for your location and flips the Hijri date, the tray tooltip, and the moon phase icon precisely at that moment, rather than at midnight. 

### 🌙 8-Phase Moon Icons
The system tray icon displays one of 8 embedded moon phases mapped mathematically to the current Hijri day of the month.

### ☀️ Context-Aware Daily Icons
The taskbar pill displays a specific icon based on the current prayer window:
*   **Night:** Maghrib to Fajr
*   **Dawn/Sunrise:** Fajr to Dhuhr
*   **Midday:** Dhuhr to Asr
*   **Sunset:** Asr to Maghrib

---

## ⚙️ Settings

Right-click on either the taskbar widget or the system tray icon to open the settings menu.

<div align="center">
   <img width="600" alt="Settings Menu" src="https://github.com/user-attachments/assets/1007d5ab-3685-4bfc-a165-1f3dc193a85e" />
</div>

---

## ⚡ Performance & Architecture Optimizations

*   **Smart Rendering:** The pill renders to an offscreen GDI+ ARGB bitmap and uses a single 1:1 blit to the screen in `WM_PAINT`. A redraw guard (`set_data`) compares incoming data against the last rendered state and immediately returns if nothing changed, making idle ticks zero-cost. Background erasing is disabled (`WM_ERASEBKGND`), and the window never steals focus (`MA_NOACTIVATE`).
*   **Optimized Timers:** Instead of a single aggressive 1-second timer, the app uses three distinct timers:
    *   `TIMER_DATA` (20s): Main data/render tick.
    *   `TIMER_POS` (30s): Repositioning and final-minute countdown checks.
    *   `TIMER_WATCH` (1s): A lightweight watchdog for fullscreen Z-order visibility.
*   **Urgency Tinting:** The countdown color linearly blends from normal to urgent over the final 15 minutes, switching to "seconds" format in the final minute.
*   **Intelligent Caching:** The Aladhan cache is a single JSON file valid for 7 days. It is method-aware (changing the calculation method invalidates it) and keeps the last known data in memory during offline states so the UI never blanks out. It also pre-fetches the next month in the background on the final 2 days of the Gregorian month.

---

## 🛠️ Engineering Deep Dives

The app uses a **stateless Z-order architecture** to ensure the taskbar widget stays above the taskbar but yields to fullscreen apps across multi-monitor setups (fixing a bug that existed in the original architecture).

For a full explanation of this Win32 logic, the atomic file writes, and the threading model, see the technical documentation:

*   [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — Application architecture and internals.
*   [`docs/ZORDER_POSTMORTEM.md`](docs/ZORDER_POSTMORTEM.md) — Deep dive into the multi-monitor fullscreen bug and its resolution.

---

## 🚀 Download & Run

1. Go to the [Releases](../../releases) page.
2. Download the latest `RustyPrayerTray-win-x64.exe`.
3. Run it. It is completely portable (statically linked CRT, no Visual C++ Redistributable required). It supports a portable config file if placed next to the executable.

---

## ⚖️ Credits & License
This project is licensed under the MIT License. See the LICENSE file for details.

Original Work: A huge thank you to dev ghoniem for creating the original PrayerTray and for providing the MIT license to allow this derivative work to exist.

---

## 🛠️ Building from Source

Requires Rust and MSVC Build Tools.

```powershell
cd rust
cargo build --release
