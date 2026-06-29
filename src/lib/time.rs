//! Calendar-time helpers for stamping CSI packets.
//!
//! Provides [`DateTime`] — a serializable wall-clock representation
//! attached to every captured CSI sample so host tooling can correlate
//! packets across nodes.

use embassy_time::Instant;
use postcard::experimental::max_size::MaxSize;
use serde::{Deserialize, Serialize};

// Date Time Struct
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct DateTimeCapture {
    captured_at: Instant,
    captured_secs: u64,
    captured_millis: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, MaxSize)]
/// Calendar date/time representation.
pub struct DateTime {
    /// Year (e.g. 2026).
    pub year: u64,
    /// Month in range 1..=12.
    pub month: u64,
    /// Day in month in range 1..=31.
    pub day: u64,
    /// Hour in range 0..=23.
    pub hour: u64,
    /// Minute in range 0..=59.
    pub minute: u64,
    /// Second in range 0..=59.
    pub second: u64,
    /// Millisecond in range 0..=999.
    pub millisecond: u64,
}

/// Convert a UNIX timestamp to calendar components.
///
/// Returns `(year, month, day, hour, minute, second, millisecond)`.
pub fn unix_to_date_time(
    unix_seconds: u64,
    unix_millis: u64,
) -> (u64, u64, u64, u64, u64, u64, u64) {
    const SECONDS_PER_MINUTE: u64 = 60;
    const SECONDS_PER_HOUR: u64 = 3600;
    const SECONDS_PER_DAY: u64 = 86400;

    // Days since epoch
    let mut days_since_epoch = unix_seconds / SECONDS_PER_DAY;
    let seconds_in_day = unix_seconds % SECONDS_PER_DAY;

    // Calculate hour, minute, second
    let hour = seconds_in_day / SECONDS_PER_HOUR;
    let minute = (seconds_in_day % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE;
    let second = seconds_in_day % SECONDS_PER_MINUTE;

    // Calculate year, month, day
    let mut year = 1970;
    while days_since_epoch >= days_in_year(year) {
        days_since_epoch -= days_in_year(year);
        year += 1;
    }

    let mut month = 1;
    while days_since_epoch >= days_in_month(year, month) {
        days_since_epoch -= days_in_month(year, month);
        month += 1;
    }

    let day = days_since_epoch + 1;

    // Return the calculated values
    (year, month, day, hour, minute, second, unix_millis)
}

/// Return `true` when `year` is a leap year in the Gregorian calendar.
pub fn is_leap_year(year: u64) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

/// Return the number of days in `year`.
pub fn days_in_year(year: u64) -> u64 {
    if is_leap_year(year) { 366 } else { 365 }
}

/// Return the number of days in `month` for a given `year`.
///
/// Returns `0` when `month` is outside `1..=12`.
pub fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        1 => 31,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        3 => 31,
        4 => 30,
        5 => 31,
        6 => 30,
        7 => 31,
        8 => 31,
        9 => 30,
        10 => 31,
        11 => 30,
        12 => 31,
        _ => 0,
    }
}
