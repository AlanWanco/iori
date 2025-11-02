use std::ops::{Add, AddAssign};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, FixedOffset, NaiveDateTime, TimeZone, Timelike};

/// Japanese Standard Time offset
pub const JST: FixedOffset = FixedOffset::east_opt(9 * 3600).unwrap();

/// Radiko time utilities
#[derive(Debug, Clone, Copy)]
pub struct RadikoTime(DateTime<FixedOffset>);

impl RadikoTime {
    pub fn now() -> Self {
        Self(chrono::Utc::now().with_timezone(&JST))
    }

    pub fn from_timestamp(timestamp: i64) -> Self {
        let dt = DateTime::from_timestamp(timestamp, 0)
            .unwrap_or_else(|| DateTime::from_timestamp(0, 0).unwrap())
            .with_timezone(&JST);
        Self(dt)
    }

    pub fn from_timestring(timestring: &str) -> Result<Self, chrono::ParseError> {
        // Format: YYYYMMDDHHmmss
        let dt = NaiveDateTime::parse_from_str(timestring, "%Y%m%d%H%M%S")?;
        Ok(Self(JST.from_local_datetime(&dt).unwrap()))
    }

    pub fn timestring(&self) -> String {
        self.0.format("%Y%m%d%H%M%S").to_string()
    }

    pub fn isoformat(&self) -> String {
        self.0.to_rfc3339()
    }

    pub fn timestamp(&self) -> i64 {
        self.0.timestamp()
    }

    /// Get the broadcast day start (5:00 AM on the current or previous day)
    pub fn broadcast_day_start(&self) -> Self {
        let mut dt = self.0;
        if dt.hour() < 5 {
            dt -= Duration::days(1);
        }
        let date = dt.date_naive();
        let start = date.and_hms_opt(5, 0, 0).unwrap();
        Self(JST.from_local_datetime(&start).unwrap())
    }

    /// Get the broadcast day string (YYYYMMDD)
    pub fn broadcast_day_string(&self) -> String {
        self.broadcast_day_start().0.format("%Y%m%d").to_string()
    }

    /// Calculate expiry times for timefree content
    /// Returns (expiry_free, expiry_tf30)
    pub fn expiry(&self) -> (DateTime<FixedOffset>, DateTime<FixedOffset>) {
        let expiry_free = self.0 + Duration::days(7);
        let expiry_tf30 = self.0 + Duration::days(30);
        (expiry_free, expiry_tf30)
    }

    pub fn inner(&self) -> DateTime<FixedOffset> {
        self.0
    }
}

impl Add<StdDuration> for RadikoTime {
    type Output = Self;

    fn add(self, duration: StdDuration) -> Self::Output {
        Self(self.0 + duration)
    }
}

impl AddAssign<StdDuration> for RadikoTime {
    fn add_assign(&mut self, duration: StdDuration) {
        self.0 += duration;
    }
}

impl From<DateTime<FixedOffset>> for RadikoTime {
    fn from(dt: DateTime<FixedOffset>) -> Self {
        Self(dt)
    }
}
