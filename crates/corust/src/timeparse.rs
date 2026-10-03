//! Parsing of user-supplied times and durations.

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use coroot_rs::TimeRange;

/// Parses a duration like "90s", "15m", "1h30m", "2d", "1w", "500ms" into milliseconds.
pub fn parse_duration_ms(s: &str) -> Result<i64> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty duration");
    }
    let mut total: f64 = 0.0;
    let mut rest = s;
    while !rest.is_empty() {
        let num_end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        if num_end == 0 {
            bail!("invalid duration '{s}' (expected e.g. 30s, 15m, 1h, 2d)");
        }
        let n: f64 = rest[..num_end]
            .parse()
            .map_err(|_| anyhow!("invalid duration '{s}'"))?;
        rest = &rest[num_end..];
        let unit_end = rest
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(rest.len());
        let unit = &rest[..unit_end];
        rest = &rest[unit_end..];
        let mult = match unit {
            "ms" => 1.0,
            "s" | "sec" | "" => 1e3,
            "m" | "min" => 60e3,
            "h" => 3600e3,
            "d" => 86400e3,
            "w" => 7.0 * 86400e3,
            _ => bail!("invalid duration unit '{unit}' in '{s}' (use ms, s, m, h, d, w)"),
        };
        total += n * mult;
    }
    Ok(total as i64)
}

/// Parses a point in time into epoch milliseconds, relative to `now_ms`.
///
/// Accepted forms: "now", "now-1h", "1h" (meaning 1h ago), RFC 3339,
/// "YYYY-MM-DD HH:MM[:SS]" / "YYYY-MM-DDTHH:MM[:SS]" in local time, "YYYY-MM-DD",
/// and epoch seconds or milliseconds.
pub fn parse_time_ms(s: &str, now_ms: i64) -> Result<i64> {
    let s = s.trim();
    if s == "now" {
        return Ok(now_ms);
    }
    if let Some(rest) = s.strip_prefix("now") {
        let (sign, d) = match rest.as_bytes().first() {
            Some(b'-') => (-1, &rest[1..]),
            Some(b'+') => (1, &rest[1..]),
            _ => bail!("invalid time '{s}' (expected e.g. now-1h)"),
        };
        return Ok(now_ms + sign * parse_duration_ms(d)?);
    }
    if s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty() {
        let n: i64 = s.parse()?;
        // Heuristic: values below 10^11 are epoch seconds (until year 5138).
        return Ok(if n < 100_000_000_000 { n * 1000 } else { n });
    }
    if let Ok(d) = parse_duration_ms(s) {
        return Ok(now_ms - d);
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.timestamp_millis());
    }
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(t) = NaiveDateTime::parse_from_str(s, fmt) {
            return local_ms(t);
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return local_ms(d.and_hms_opt(0, 0, 0).unwrap());
    }
    bail!(
        "invalid time '{s}' (examples: now-2h, 30m, 2026-01-02 15:04, 2026-01-02T15:04:05Z, 1767225600)"
    )
}

fn local_ms(t: NaiveDateTime) -> Result<i64> {
    Local
        .from_local_datetime(&t)
        .earliest()
        .map(|t| t.timestamp_millis())
        .ok_or_else(|| anyhow!("invalid local time {t}"))
}

/// Parses the global `--since`/`--from`/`--to` flags into a time window.
pub fn parse_range(since: Option<&str>, from: Option<&str>, to: Option<&str>) -> Result<TimeRange> {
    let now = Utc::now().timestamp_millis();
    let to_ms = to.map(|t| parse_time_ms(t, now)).transpose()?;
    let from_ms = match (since, from) {
        (Some(_), Some(_)) => bail!("--since and --from are mutually exclusive"),
        (Some(d), None) => Some(to_ms.unwrap_or(now) - parse_duration_ms(d)?),
        (None, Some(f)) => Some(parse_time_ms(f, now)?),
        (None, None) => None,
    };
    if let (Some(f), Some(t)) = (from_ms, to_ms)
        && f >= t
    {
        bail!("the start of the time range must be before its end");
    }
    let time = |ms: i64| {
        Utc.timestamp_millis_opt(ms)
            .single()
            .ok_or_else(|| anyhow!("time out of range"))
    };
    Ok(TimeRange::new(
        from_ms.map(time).transpose()?,
        to_ms.map(time).transpose()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    #[test]
    fn durations() {
        assert_eq!(parse_duration_ms("90s").unwrap(), 90_000);
        assert_eq!(parse_duration_ms("1h30m").unwrap(), 5_400_000);
        assert_eq!(parse_duration_ms("500ms").unwrap(), 500);
        assert_eq!(parse_duration_ms("2d").unwrap(), 172_800_000);
        assert_eq!(parse_duration_ms("1.5h").unwrap(), 5_400_000);
        assert!(parse_duration_ms("abc").is_err());
        assert!(parse_duration_ms("5y").is_err());
    }

    #[test]
    fn times() {
        assert_eq!(parse_time_ms("now", NOW).unwrap(), NOW);
        assert_eq!(parse_time_ms("now-1h", NOW).unwrap(), NOW - 3_600_000);
        assert_eq!(parse_time_ms("15m", NOW).unwrap(), NOW - 900_000);
        assert_eq!(parse_time_ms("1700000000", NOW).unwrap(), 1_700_000_000_000);
        assert_eq!(
            parse_time_ms("1700000000123", NOW).unwrap(),
            1_700_000_000_123
        );
        assert_eq!(
            parse_time_ms("2023-11-14T22:13:20Z", NOW).unwrap(),
            1_700_000_000_000
        );
        assert!(parse_time_ms("2023-11-14 22:13", NOW).is_ok());
        assert!(parse_time_ms("yesterday", NOW).is_err());
    }

    #[test]
    fn ranges() {
        let r = parse_range(
            None,
            Some("2023-11-14T22:00:00Z"),
            Some("2023-11-14T23:00:00Z"),
        )
        .unwrap();
        assert_eq!((r.to.unwrap() - r.from.unwrap()).num_seconds(), 3600);
        assert!(parse_range(Some("1h"), Some("now-1h"), None).is_err());
        assert!(parse_range(None, Some("now"), Some("now-1h")).is_err());
        assert!(parse_range(None, None, None).unwrap().is_default());
    }
}
