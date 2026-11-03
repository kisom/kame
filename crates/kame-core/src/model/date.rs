//! A calendar day, which is all kame's dates mean.

use std::fmt;
use std::time::SystemTime;

/// A day on the calendar, ordered by when it falls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    year: i32,
    month: u8,
    day: u8,
}

/// Rust's number parsing also takes a leading sign, which no date has.
fn digits(text: &str) -> bool {
    text.bytes().all(|byte| byte.is_ascii_digit())
}

impl Date {
    /// Reads `YYYY-MM-DD`, optionally followed by `HH:MM` or `HH:MM:SS` as Markdown from
    /// Pelican carries. The time has to make sense, and is then ignored.
    pub fn parse(text: &str) -> Option<Date> {
        let text = text.trim();
        let (day, time) = match text.split_once(' ') {
            Some((day, time)) => (day, Some(time)),
            None => (text, None),
        };
        if let Some(time) = time {
            let parts: Vec<&str> = time.split(':').collect();
            if !matches!(parts.len(), 2 | 3) {
                return None;
            }
            let limits = [24, 60, 60];
            for (part, limit) in parts.iter().zip(limits) {
                if part.len() != 2 || !digits(part) || part.parse::<u8>().ok()? >= limit {
                    return None;
                }
            }
        }
        let parts: Vec<&str> = day.split('-').collect();
        let [year, month, day] = parts.as_slice() else {
            return None;
        };
        if year.len() != 4 || month.len() != 2 || day.len() != 2 {
            return None;
        }
        if !digits(year) || !digits(month) || !digits(day) {
            return None;
        }
        let year: i32 = year.parse().ok()?;
        let month: u8 = month.parse().ok()?;
        let day: u8 = day.parse().ok()?;
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let days = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return None,
        };
        (1..=days)
            .contains(&day)
            .then_some(Date { year, month, day })
    }

    /// The day `when` falls on, in UTC.
    pub fn from_system_time(when: SystemTime) -> Date {
        Date::parse(&crate::session::iso_date(when)).expect("iso_date writes a valid date")
    }

    /// The form RSS requires, at midnight UTC, since kame's dates are days.
    pub fn rfc822(&self) -> String {
        const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        // Sakamoto's method.
        const OFFSETS: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let month = usize::from(self.month) - 1;
        let year = if self.month < 3 {
            self.year - 1
        } else {
            self.year
        };
        let weekday =
            (year + year / 4 - year / 100 + year / 400 + OFFSETS[month] + i32::from(self.day)) % 7;
        format!(
            "{}, {:02} {} {:04} 00:00:00 GMT",
            WEEKDAYS[weekday as usize], self.day, MONTHS[month], self.year
        )
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn shown(text: &str) -> Option<String> {
        Date::parse(text).map(|date| date.to_string())
    }

    #[test]
    fn a_day_parses() {
        assert_eq!(shown("2026-11-02").as_deref(), Some("2026-11-02"));
    }

    #[test]
    fn a_time_from_pelican_is_accepted_and_ignored() {
        assert_eq!(shown("2024-01-02 10:30").as_deref(), Some("2024-01-02"));
        assert_eq!(shown("2024-01-02 10:30:45").as_deref(), Some("2024-01-02"));
    }

    #[test]
    fn surrounding_space_is_ignored() {
        assert_eq!(shown("  2026-11-02\n").as_deref(), Some("2026-11-02"));
    }

    #[test]
    fn impossible_days_are_rejected() {
        for text in [
            "2026-02-30",
            "2026-13-01",
            "2026-00-10",
            "2026-04-31",
            "2025-02-29",
        ] {
            assert_eq!(shown(text), None, "{text}");
        }
    }

    #[test]
    fn a_leap_day_is_accepted() {
        assert_eq!(shown("2024-02-29").as_deref(), Some("2024-02-29"));
        assert_eq!(shown("2000-02-29").as_deref(), Some("2000-02-29"));
        assert_eq!(shown("1900-02-29"), None);
    }

    #[test]
    fn anything_else_is_rejected() {
        for text in [
            "",
            "yesterday",
            "2026-11",
            "2026/11/02",
            "02-11-2026",
            "2026-11-02 25:00",
            "2026-11-02 10:61",
            "2026-1-2",
        ] {
            assert_eq!(shown(text), None, "{text}");
        }
    }

    #[test]
    fn only_digits_count_as_numbers() {
        for text in [
            "-123-01-01",
            "+123-01-01",
            "2026-+1-02",
            "2026-11-+2",
            "2026-11-02 +1:30",
        ] {
            assert_eq!(shown(text), None, "{text}");
        }
    }

    #[test]
    fn dates_order_by_when_they_fall() {
        let a = Date::parse("2024-12-31").unwrap();
        let b = Date::parse("2025-01-01").unwrap();
        assert!(a < b);
    }

    #[test]
    fn rfc822_names_the_right_weekday() {
        assert_eq!(
            Date::parse("2026-11-02").unwrap().rfc822(),
            "Mon, 02 Nov 2026 00:00:00 GMT"
        );
        assert_eq!(
            Date::parse("2024-02-29").unwrap().rfc822(),
            "Thu, 29 Feb 2024 00:00:00 GMT"
        );
        assert_eq!(
            Date::parse("2000-01-01").unwrap().rfc822(),
            "Sat, 01 Jan 2000 00:00:00 GMT"
        );
    }

    #[test]
    fn a_system_time_becomes_its_day() {
        let when = UNIX_EPOCH + Duration::from_secs(1_793_620_800); // 2026-11-02 12:00 UTC
        assert_eq!(Date::from_system_time(when).to_string(), "2026-11-02");
    }
}
