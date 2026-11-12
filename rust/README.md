# PrayerTray — native rewrite (Rust)

Native Win32 rewrite of PrayerTray: one small exe, no .NET runtime, same features,
same `%APPDATA%\PrayerTray\config.json`. Replaced the C# build at v2.0.0; the C# tree
was removed afterwards and lives on in git history at tag `v1.14.0`.

## Build

```
cargo build --release --target x86_64-pc-windows-msvc --locked   # needs MSVC Build Tools + Windows SDK
cargo test --target x86_64-pc-windows-msvc                       # quiet-hours, atomic writes, i18n integrity
```

Dev builds are version `0.0.0` (never self-update) and can run beside an installed
release: set `PRAYERTRAY_DEV_MUTEX=1`.

## Ported data

`src/i18n/data.rs` was generated from the C# tree at port time. The generator went with it —
edit it by hand now. See the root [ARCHITECTURE.md](../ARCHITECTURE.md) for the design and
[docs/ZORDER_POSTMORTEM.md](../docs/ZORDER_POSTMORTEM.md) for the z-order fix.
