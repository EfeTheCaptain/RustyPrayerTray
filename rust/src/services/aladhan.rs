//! Aladhan API prayer-times client + 7-day cache.
//! Endpoint: https://api.aladhan.com/v1/calendar/{year}/{month}?latitude={lat}&longitude={lon}&method={method}&school=0

use crate::datetime::Date;
use crate::services::http::get_string;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const API_BASE: &str = "https://api.aladhan.com/v1/calendar";
const CACHE_MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60; // 7 days

#[derive(Deserialize, Clone)]
pub struct AladhanResponse {
    pub data: Vec<AladhanDay>,
}

#[derive(Deserialize, Clone, Serialize)]
pub struct AladhanDay {
    pub timings: AladhanTimings,
    pub date: AladhanDate,
}

// Field names mirror the Aladhan API's JSON keys verbatim.
#[allow(non_snake_case)]
#[derive(Deserialize, Clone, Serialize)]
pub struct AladhanTimings {
    pub Fajr: String,
    pub Sunrise: String,
    pub Dhuhr: String,
    pub Asr: String,
    pub Maghrib: String,
    pub Isha: String,
}

#[derive(Deserialize, Clone, Serialize)]
pub struct AladhanDate {
    pub gregorian: AladhanGregorian,
    pub hijri: AladhanHijri,
}

#[derive(Deserialize, Clone, Serialize)]
pub struct AladhanGregorian {
    pub date: String, // "DD-MM-YYYY"
}

#[derive(Deserialize, Clone, Serialize)]
pub struct AladhanHijri {
    pub day: String,
    pub month: AladhanHijriMonth,
}

#[derive(Deserialize, Clone, Serialize)]
pub struct AladhanHijriMonth {
    pub en: String,
}

#[derive(Serialize, Deserialize)]
pub struct PrayerCache {
    pub fetched_at: u64, // Unix seconds
    pub latitude: f64,
    pub longitude: f64,
    pub year: i32,
    pub month: u32,
    /// Calculation method the data was fetched with. Absent in older caches, which
    /// were always method 13 (Diyanet) — hence the default.
    #[serde(default = "default_cache_method")]
    pub method: u8,
    pub data: Vec<AladhanDay>,
}

fn default_cache_method() -> u8 {
    13
}

/// Parse "HH:MM" portion from a timing string like "05:47 (+03)".
fn parse_hhmm(s: &str) -> Option<u16> {
    let hhmm = s.split_whitespace().next()?;
    let parts: Vec<&str> = hhmm.split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let h: u16 = parts[0].trim().parse().ok()?;
    let m: u16 = parts[1].trim().parse().ok()?;
    Some(h * 60 + m)
}

/// Today's date in "DD-MM-YYYY" format to match the API's gregorian.date.
fn today_key(d: &Date) -> String {
    format!("{:02}-{:02}-{:04}", d.day, d.month, d.year)
}

/// Cache file path: %APPDATA%\PrayerTray\cache.json
fn cache_path() -> PathBuf {
    crate::config::AppConfig::dir().join("cache.json")
}

/// Load the cache from disk, or None if missing/invalid.
fn load_cache() -> Option<PrayerCache> {
    let path = cache_path();
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str::<PrayerCache>(&text).ok()
}

/// Save the cache to disk.
fn save_cache(cache: &PrayerCache) {
    let path = cache_path();
    if let Ok(json) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(&path, json);
    }
}

/// Check if the cache is still valid (< 7 days old, same lat/lng, same year/month,
/// same calculation method).
fn cache_is_valid(cache: &PrayerCache, lat: f64, lng: f64, year: i32, month: u32, method: u8) -> bool {
    if cache.latitude != lat || cache.longitude != lng {
        return false;
    }
    if cache.year != year || cache.month != month {
        return false;
    }
    if cache.method != method {
        return false;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now.saturating_sub(cache.fetched_at) < CACHE_MAX_AGE_SECS
}

/// Fetch the current month from the Aladhan API, using cache when valid.
pub fn fetch_month(lat: f64, lng: f64, year: i32, month: u32, method: u8) -> Option<AladhanResponse> {
    // Try cache first
    if let Some(cache) = load_cache() {
        if cache_is_valid(&cache, lat, lng, year, month, method) {
            return Some(AladhanResponse {
                data: cache.data,
            });
        }
    }

    // Fetch from API
    let url = format!(
        "{}/{}/{}?latitude={}&longitude={}&method={}&school=0",
        API_BASE, year, month, lat, lng, method
    );
    let body = get_string(&url, None)?;
    serde_json::from_str::<AladhanResponse>(&body).ok()
}

/// Save fetched data to cache.
pub fn save_to_cache(lat: f64, lng: f64, year: i32, month: u32, method: u8, data: &[AladhanDay]) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cache = PrayerCache {
        fetched_at: now,
        latitude: lat,
        longitude: lng,
        year,
        month,
        method,
        data: data.to_vec(),
    };
    save_cache(&cache);
}

/// Find the entry matching `date` ("DD-MM-YYYY" gregorian key) in the month's data.
pub fn find_by_date<'a>(response: &'a AladhanResponse, date: &Date) -> Option<&'a AladhanDay> {
    let key = today_key(date);
    response.data.iter().find(|d| d.date.gregorian.date == key)
}

/// Find today's entry in the month's data by matching gregorian.date.
pub fn find_today<'a>(response: &'a AladhanResponse, today: &'a Date) -> Option<&'a AladhanDay> {
    find_by_date(response, today)
}

/// Extract prayer times in minutes since midnight, in KEYS order (fajr, sunrise, dhuhr, asr, maghrib, isha).
pub fn times_from_day(day: &AladhanDay) -> [u16; 6] {
    [
        parse_hhmm(&day.timings.Fajr).unwrap_or(0),
        parse_hhmm(&day.timings.Sunrise).unwrap_or(0),
        parse_hhmm(&day.timings.Dhuhr).unwrap_or(0),
        parse_hhmm(&day.timings.Asr).unwrap_or(0),
        parse_hhmm(&day.timings.Maghrib).unwrap_or(0),
        parse_hhmm(&day.timings.Isha).unwrap_or(0),
    ]
}

/// Extract hijri day number from an AladhanDay.
pub fn hijri_from_day(day: &AladhanDay) -> (u32, String) {
    let day_num: u32 = day.date.hijri.day.parse().unwrap_or(1);
    let month_name = day.date.hijri.month.en.clone();
    (day_num, month_name)
}
