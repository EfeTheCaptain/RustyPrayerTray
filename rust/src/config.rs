//! App config + small runtime state, JSON-compatible with the C# build
//! (%APPDATA%\PrayerTray\config.json, PascalCase, same defaults and sentinels).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn is_true(b: &bool) -> bool {
    *b
}

pub const REMINDER_SOUND_IDS: [&str; 6] = ["chime", "bell", "ding", "beep", "double", "custom"];

/// Aladhan calculation-method ids, display order = dropdown order (first = default).
pub const CALC_METHODS: [u8; 7] = [13, 3, 2, 4, 5, 1, 15];
/// Proper nouns — identical in every language, so they live next to the ids rather
/// than in the translation catalog.
pub const CALC_METHOD_NAMES: [&str; 7] = [
    "Diyanet (Turkey)",
    "Muslim World League (MWL)",
    "ISNA (North America)",
    "Umm al-Qura (Makkah)",
    "Egyptian General Authority",
    "University of Karachi",
    "Moonsighting Committee",
];

/// PopupX/PopupY unset sentinel (C# int.MinValue).
pub const POPUP_UNSET: i32 = i32::MIN;
/// TimezoneHours "use system timezone" sentinel.
pub const TZ_SYSTEM: f64 = 999.0;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct AppConfig {
    pub city: String,
    pub latitude: f64,
    pub longitude: f64,
    pub use24_hour: bool,
    pub widget_anchor: String, // Left | Right
    pub widget_offset: i32,
    pub theme: String, // Auto | Dark | Light | Midnight | Slate | Warm
    pub monitor_device_name: Option<String>,
    pub hide_on_fullscreen: bool,
    pub lock_widget: bool,
    pub language: String, // auto | en | ar | fr | tr | ur | id
    pub show_hijri_date: bool,
    pub show_islamic_events: bool,
    pub sunnah_fast_reminder: bool,
    pub friday_reminder: bool,
    pub font_family: String,
    pub font_scale_pct: i32,
    pub popup_pinned: bool,
    pub popup_x: i32,
    pub popup_y: i32,
    pub rich_toasts: bool,
    pub reminder_enabled: bool,
    pub reminder_minutes: i32,
    pub reminder_sound: bool,
    pub reminder_sound_id: String,
    pub reminder_sound_path: Option<String>,
    pub azan_mode: String, // None | <builtin id> | Custom
    pub azan_custom_path: Option<String>,
    #[serde(skip_serializing_if = "is_true")]
    pub mute_when_busy: bool,
    pub daily_icons: bool,
    /// Aladhan API calculation method (see CALC_METHODS).
    pub calculation_method: u8,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            city: "Makkah".into(),
            latitude: 21.4225,
            longitude: 39.8262,
            use24_hour: false,
            widget_anchor: "Right".into(),
            widget_offset: 12,
            theme: "Auto".into(),
            monitor_device_name: None,
            hide_on_fullscreen: true,
            lock_widget: false,
            language: "auto".into(),
            show_hijri_date: true,
            show_islamic_events: true,
            sunnah_fast_reminder: false,
            friday_reminder: false,
            font_family: "Segoe UI".into(),
            font_scale_pct: 100,
            popup_pinned: false,
            popup_x: POPUP_UNSET,
            popup_y: POPUP_UNSET,
            rich_toasts: true,
            reminder_enabled: false,
            reminder_minutes: 10,
            reminder_sound: true,
            reminder_sound_id: "chime".into(),
            reminder_sound_path: None,
azan_mode: "None".into(),
            azan_custom_path: None,
            mute_when_busy: true,
            daily_icons: false,
            calculation_method: 13,
}
}
}

impl AppConfig {
    pub fn font_scale(&self) -> f32 {
        self.font_scale_pct.clamp(80, 150) as f32 / 100.0
    }

    /// Clamp all ranged fields; run on any config that bypassed the form (file load, import).
    pub fn sanitize(&mut self) {
        self.latitude = self.latitude.clamp(-90.0, 90.0);
        self.longitude = self.longitude.clamp(-180.0, 180.0);
        self.widget_offset = self.widget_offset.clamp(0, 2000);
        self.reminder_minutes = self.reminder_minutes.clamp(1, 60);
        self.font_scale_pct = self.font_scale_pct.clamp(80, 150);
        // Reaches the filesystem via audio::synth_path, so an imported config must not put
        // path separators in it.
        if !REMINDER_SOUND_IDS.contains(&self.reminder_sound_id.as_str()) {
            self.reminder_sound_id = "chime".into();
        }
        if !CALC_METHODS.contains(&self.calculation_method) {
            self.calculation_method = 13;
        }
    }

    /// Portable mode: if a `portable` marker (or an existing `config.json`) sits next to the exe,
    /// keep settings/state beside the app; otherwise use %APPDATA%\PrayerTray like the installed build.
    pub fn dir() -> PathBuf {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                if dir.join("portable").exists() || dir.join("config.json").exists() {
                    return dir.to_path_buf();
                }
            }
        }
        let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default();
        base.join("PrayerTray")
    }

    fn path() -> PathBuf {
        Self::dir().join("config.json")
    }

    pub fn is_first_run() -> bool {
        !Self::path().exists()
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(mut cfg) = serde_json::from_str::<Self>(&text) {
                cfg.sanitize();
                return cfg;
            }
        }
        Self::default() // missing or corrupt -> defaults
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = crate::util::write_atomic(&Self::path(), json.as_bytes());
        }
    }

}

/// Small persisted state (not user settings) — survives restarts so reminders don't double-fire.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct AppState {
    pub sunnah_fast_noticed: String, // yyyy-MM-dd of the last eve-before fast notice
}

impl AppState {
    fn path() -> PathBuf {
        AppConfig::dir().join("state.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string(self) {
            let _ = crate::util::write_atomic(&Self::path(), json.as_bytes());
        }
    }
}
