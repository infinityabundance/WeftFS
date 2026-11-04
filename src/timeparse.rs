//! RFC 3339 / unix-nanosecond parsing for `mount --at`.
//!
//! The accepted forms are deliberately small and explicit:
//!
//! * a plain decimal integer interpreted as nanoseconds since the Unix epoch
//!   (an optional leading `-` is accepted);
//! * `YYYY-MM-DDTHH:MM:SS[.frac](Z|+HH:MM|-HH:MM)` (RFC 3339). A lowercase
//!   `t`/`z` is accepted; the fractional part is base-10 and is truncated to
//!   nanosecond precision.
//!
//! No locale, timezone database, or ambient clock participates. An invalid
//! input is an `InvalidArgument` error; there is no "nearest guess".

use crate::error::{Error, Result};

/// Parse `--at` into UTC nanoseconds.
pub fn parse_at_ns(text: &str) -> Result<i64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(Error::invalid_argument("--at must not be empty"));
    }
    if let Some(ns) = parse_unix_ns(trimmed)? {
        return Ok(ns);
    }
    parse_rfc3339(trimmed)
}

fn parse_unix_ns(text: &str) -> Result<Option<i64>> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(None);
    }
    let magnitude: i128 = digits
        .parse()
        .map_err(|_| Error::invalid_argument("--at unix nanoseconds are out of range"))?;
    let value = if negative { -magnitude } else { magnitude };
    let value = i64::try_from(value)
        .map_err(|_| Error::invalid_argument("--at unix nanoseconds are out of range"))?;
    Ok(Some(value))
}

fn parse_rfc3339(text: &str) -> Result<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return Err(Error::invalid_argument(
            "--at must be unix nanoseconds or an RFC 3339 timestamp",
        ));
    }
    let year = parse_fixed(bytes, 0, 4, "year")?;
    expect(bytes, 4, b'-', "year-month separator")?;
    let month = parse_fixed(bytes, 5, 2, "month")?;
    expect(bytes, 7, b'-', "month-day separator")?;
    let day = parse_fixed(bytes, 8, 2, "day")?;
    match bytes[10] {
        b'T' | b't' => {}
        _ => return Err(Error::invalid_argument("--at RFC 3339 needs `T`")),
    }
    let hour = parse_fixed(bytes, 11, 2, "hour")?;
    expect(bytes, 13, b':', "hour-minute separator")?;
    let minute = parse_fixed(bytes, 14, 2, "minute")?;
    expect(bytes, 16, b':', "minute-second separator")?;
    let second = parse_fixed(bytes, 17, 2, "second")?;

    if !(1..=12).contains(&month) {
        return Err(Error::invalid_argument("--at month is out of range"));
    }
    let max_day = days_in_month(year, month);
    if day == 0 || day > max_day {
        return Err(Error::invalid_argument("--at day is out of range"));
    }
    if hour > 23 || minute > 59 || second > 59 {
        return Err(Error::invalid_argument("--at time is out of range"));
    }

    let mut index = 19;
    let mut nanos: i64 = 0;
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == start {
            return Err(Error::invalid_argument(
                "--at fractional seconds need at least one digit",
            ));
        }
        let digits = &bytes[start..index];
        let mut value: i64 = 0;
        for position in 0..9 {
            value *= 10;
            if position < digits.len() {
                value += i64::from(digits[position] - b'0');
            }
        }
        nanos = value;
    }

    let offset_seconds = if index < bytes.len() {
        match bytes[index] {
            b'Z' | b'z' => {
                index += 1;
                0i64
            }
            b'+' | b'-' => {
                let sign = if bytes[index] == b'+' { 1 } else { -1 };
                index += 1;
                if index + 5 > bytes.len() || bytes[index + 2] != b':' {
                    return Err(Error::invalid_argument(
                        "--at timezone offset must be ±HH:MM",
                    ));
                }
                let off_hour = parse_fixed(bytes, index, 2, "offset hour")?;
                let off_minute = parse_fixed(bytes, index + 3, 2, "offset minute")?;
                if off_hour > 23 || off_minute > 59 {
                    return Err(Error::invalid_argument(
                        "--at timezone offset is out of range",
                    ));
                }
                index += 5;
                sign * (off_hour * 3600 + off_minute * 60)
            }
            _ => {
                return Err(Error::invalid_argument(
                    "--at needs a `Z` or numeric offset",
                ))
            }
        }
    } else {
        return Err(Error::invalid_argument(
            "--at needs a `Z` or numeric offset",
        ));
    };
    if index != bytes.len() {
        return Err(Error::invalid_argument("--at has trailing characters"));
    }

    let days = days_from_civil(year, month, day);
    let seconds = days
        .checked_mul(86_400)
        .and_then(|value| value.checked_add(hour * 3600 + minute * 60 + second))
        .and_then(|value| value.checked_sub(offset_seconds))
        .ok_or_else(|| Error::invalid_argument("--at timestamp is out of range"))?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|value| value.checked_add(nanos))
        .ok_or_else(|| Error::invalid_argument("--at timestamp is out of range"))
}

fn parse_fixed(bytes: &[u8], start: usize, len: usize, what: &str) -> Result<i64> {
    if start + len > bytes.len() {
        return Err(Error::invalid_argument(format!(
            "--at is missing its {what}"
        )));
    }
    let mut value: i64 = 0;
    for &byte in &bytes[start..start + len] {
        if !byte.is_ascii_digit() {
            return Err(Error::invalid_argument(format!(
                "--at {what} must be decimal digits"
            )));
        }
        value = value * 10 + i64::from(byte - b'0');
    }
    Ok(value)
}

fn expect(bytes: &[u8], index: usize, wanted: u8, what: &str) -> Result<()> {
    match bytes.get(index) {
        Some(byte) if *byte == wanted => Ok(()),
        _ => Err(Error::invalid_argument(format!(
            "--at is missing its {what}"
        ))),
    }
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian civil date (Howard
/// Hinnant's `days_from_civil`; avoids a calendar dependency).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_nanoseconds_round_trip() {
        assert_eq!(parse_at_ns("0").unwrap(), 0);
        assert_eq!(
            parse_at_ns("1700000000000000000").unwrap(),
            1_700_000_000_000_000_000
        );
        assert_eq!(parse_at_ns("-1").unwrap(), -1);
    }

    #[test]
    fn rfc3339_epoch_and_offsets() {
        assert_eq!(parse_at_ns("1970-01-01T00:00:00Z").unwrap(), 0);
        assert_eq!(
            parse_at_ns("2023-11-14T22:13:20Z").unwrap(),
            1_700_000_000_000_000_000
        );
        assert_eq!(
            parse_at_ns("2023-11-14T22:13:20.123456789Z").unwrap(),
            1_700_000_000_123_456_789
        );
        assert_eq!(
            parse_at_ns("2023-11-14T23:13:20+01:00").unwrap(),
            1_700_000_000_000_000_000
        );
        assert_eq!(
            parse_at_ns("2023-11-14t22:13:20z").unwrap(),
            1_700_000_000_000_000_000
        );
    }

    #[test]
    fn invalid_forms_are_rejected() {
        for text in [
            "",
            "2023-13-01T00:00:00Z",
            "2023-02-30T00:00:00Z",
            "2023-11-14T24:00:00Z",
            "2023-11-14T22:13:20",
            "2023-11-14T22:13:20.Z",
            "2023-11-14T22:13:20Zjunk",
            "not-a-time",
        ] {
            assert!(parse_at_ns(text).is_err(), "{text} should be rejected");
        }
    }
}
