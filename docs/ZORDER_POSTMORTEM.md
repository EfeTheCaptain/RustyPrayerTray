# Z-Order Post-Mortem: The Multi-Monitor Fullscreen Bleed

The upstream developer's architecture claimed to solve Z-order, and it did for single monitors,
but it failed on multi-monitor setups. This post-mortem documents the exact root cause and the
stateless fix implemented in this fork.

---

## The Multi-Monitor Fullscreen Z-Order Bleed (SOLVED)

> The hardest bug in this project's history. Five fix attempts made things progressively
> worse before the correct mental model was established. Documented here as the reference
> for anyone touching `rust/src/ui/widget.rs` or `rust/src/native/taskbar.rs` again.

---

### 1. The Symptom

With a fullscreen game or video running on **Monitor A**, the taskbar pill on A correctly
hid itself. But if the user then clicked the taskbar on **Monitor B**, the pill on Monitor A
suddenly reappeared — painted directly over the fullscreen game.

Worse, the intermediate fix attempts regressed the baseline in new ways: the pill either
fell *behind* the Windows 11 XAML taskbar (invisible entirely), or toggled Z-bands at
runtime and behaved erratically across monitors. The bug was not merely unfixed; each
attempt traded it for a different, sometimes worse, failure.

---

### 2. Why Each Attempted Signal Lies

| Attempt | Signal used | Why it fails |
|---------|-------------|--------------|
| 1 | Foreground window fullscreen check | Clicking Monitor B's taskbar makes the taskbar the foreground window; the app concludes "no fullscreen" and re-shows the pill over the game on Monitor A. |
| 2 | No `WS_EX_TOPMOST`, taskbar ownership only | Desktop-band windows lose to the Win11 XAML taskbar surface; the pill falls behind the bar and disappears. |
| 3 | `SHQueryUserNotificationState` | A **global** state — it cannot say *which* monitor is fullscreen, and it misses borderless-windowed games entirely. |
| 4 | `IsWindowVisible` on the monitor's taskbar HWND | A style-bit trap: a fullscreen game covering the bar never clears the bar's `WS_VISIBLE` bit, so the check reads "visible" the entire time and gates the show back open. |
| 5 | `EnumWindows` rect matching | The predicate was correct in principle, but it still shared code paths with the foreground/visibility checks above, so the poisoned branches re-showed the pill anyway. |

---

### 3. The Root Cause — The Corrected Mental Model

Four facts about Windows that reframe the entire problem:

1. **Z-bands are global, not per-monitor.** There is no "band on monitor A" vs "band on
   monitor B." Every window in every band lives in one global ordering; monitors are
   viewports onto it. Per-monitor behavior can only come from the application's own logic,
   never from the OS's z-ordering.

2. **`WS_EX_TOPMOST` never leaves the desktop band** (`ZBID_DESKTOP`). It only reorders the
   window *within* that band. The Win11 taskbar and Start Menu live in privileged shell
   bands (`ZBID_SYSTEM_TOOLS` and friends) above it — which is why dropping the topmost
   style loses to the XAML taskbar (Attempt 2), and why toggling `HWND_NOTOPMOST` at
   runtime (Attempts 1/3) permanently demotes the window out of its working position and
   breaks the baseline.

3. **`IsWindowVisible` is a style bit, not an occlusion check.** It reads `WS_VISIBLE`.
   A fullscreen game covering Monitor A does *not* hide Monitor A's `Shell_TrayWnd` — the
   bar is *occluded*, not *hidden*. Any logic that treats "taskbar visible" as "no
   fullscreen" will re-show the pill while the game is running (Attempts 1/4).

4. **The system never shows a hidden window.** No click, no band re-sort, no foreground
   change will ever `SW_SHOW` your window. Z-order operations on a hidden window are
   invisible. Therefore: *if the pill appeared over the game, our own process executed a
   show.* The bug was never "Windows dragged our band forward and bypassed the checks" —
   it was an `EVENT_SYSTEM_FOREGROUND` / `EVENT_OBJECT_REORDER` hook firing on the
   Monitor B taskbar click, feeding a mis-evaluated per-monitor verdict into a code path
   that called `ShowWindow`.

   The mechanism that makes this moment catastrophic: Windows temporarily promotes a
   fullscreen *foreground* window above the topmost region so it can cover the shell.
   When the user clicks the taskbar on Monitor B, the game on A loses foreground and is
   demoted back into the desktop band — below the topmost pill. If the pill is correctly
   hidden, the demotion is harmless. If a handler mis-evaluates and shows the pill, the
   demotion guarantees it paints over the game. The taskbar click does not cause the bug;
   it merely triggers the handlers, and the demotion makes any wrong "show" decision
   visible.

---

### 4. The Solution — Stateless Z-Order Architecture

The fix removes z-order from the equation entirely and centralizes every visibility
decision in one place.

**Stateless Z-Order.** The pill is created once with `WS_EX_TOPMOST` + taskbar ownership
(`GWLP_HWNDPARENT`) and never changes bands afterwards. There are exactly zero
`SetWindowPos(..., HWND_NOTOPMOST)` calls in the codebase. Fullscreen handling is pure
`SW_HIDE` / `SW_SHOWNA`; on a hidden window, z-order is irrelevant, so the topmost state
can never go stale.

**Single Choke Point.** Exactly one function — `Widget::update_visibility` — owns the
hidden<->shown transition, guarded by an edge-triggered latch (`suppressed`):
repeated events are idempotent; hide only fires on clear->occupied, show only on
occupied->clear, and a show can never execute while the monitor is occupied. An audit
confirms it is the only call site in the codebase that can make the pill visible.

**Authoritative Predicate.** `taskbar::is_fullscreen_on_monitor(bounds)` is the sole
decision signal, and the foreground window plays no role in it. It enumerates top-level
windows (`EnumWindows`) and reports the first one that:

- is visible (`IsWindowVisible`),
- is a root window (`GetAncestor(GA_ROOT)` — owned overlays riding an owner are skipped),
- is not shell or self (`Shell_TrayWnd`, `Shell_SecondaryTrayWnd`, `WorkerW`, `Progman`,
  and the app's own window classes),
- has a rect that **contains** the monitor's full bounds (containment, not equality —
  borderless windows a pixel off still match),
- is not DWM-cloaked (`DwmGetWindowAttribute(DWMWA_CLOAKED)` — suspended UWP windows are
  "visible" and full-size but do not occupy the screen).

It is recomputed fresh from OS state on every evaluation, never cached. The taskbar's
`IsWindowVisible` survives only as a supplementary *hide* net (genuine hides: RDP, etc.)
and can never gate a show.

**The Raise Trick.** Without any band toggling, the original "clicking the taskbar buries
the pill" weakness is handled by `Widget::raise()`:
`SetWindowPos(HWND_TOPMOST, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)`. This does not
change the window's band — it re-asserts the top of the band the pill already lives in,
the instant the taskbar comes forward (`EVENT_OBJECT_REORDER` scoped to the taskbar
thread, 16 ms debounce). Because the pill is hidden during fullscreen, the raise never
executes there and cannot bleed over a game.

**Event wiring.** Global `EVENT_SYSTEM_FOREGROUND` + taskbar-thread
`EVENT_OBJECT_REORDER` cover the instant cases; a 1 s watchdog timer (`TIMER_WATCH`)
self-heals any transition that fired no event; the pill evaluates its state once at
creation so an already-running fullscreen app never sees a flash.

**Scenario walkthrough** — game on A, click taskbar on B:

1. Foreground changes to B's taskbar -> global hook fires -> choke point evaluates.
2. Predicate enumerates and finds the game still fully covering Monitor A
   (focus-independent) -> verdict: occupied.
3. Latch already set -> no transition, no show, no z-order call. The pill stays hidden.
4. The 1 s watchdog re-affirms the same verdict every second until the game exits,
   at which point a foreground change (or the watchdog) flips the latch and shows the pill.

---

### 5. Debugging Instrument

`PRAYERTRAY_ZDEBUG=1` (environment variable) enables a transition log at
`%TEMP%\prayertray_zdebug.log`: every hide/show decision is appended with the triggering
event (`init` / `hook` / `timer` / `resume`) and the class of the window covering the
monitor. Because a hidden window cannot appear unless our code shows it, this log names
the exact trigger of any future regression within a single failing scenario. Off by
default; zero cost when unset.
