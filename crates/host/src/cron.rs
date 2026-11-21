//! Cron schedules for automations: five fields (`minute hour day-of-month
//! month day-of-week`) in the host's local time, with `*`, lists, ranges,
//! steps and English names, plus `@hourly`, `@daily`, `@weekly`, `@monthly`.
//! When both day fields are restricted, a day matching either counts (as in
//! Vixie cron).

/// Schedules may not fire more often than this.
pub const MIN_INTERVAL_MINUTES: u32 = 5;

/// A broken-down local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tm {
    pub year: i32,
    /// 1–12.
    pub month: u32,
    /// 1–31.
    pub mday: u32,
    pub hour: u32,
    pub min: u32,
    /// 0 = Sunday.
    pub wday: u32,
}

/// Turns Unix seconds into local time.
pub trait LocalTime {
    fn local(&self, secs: i64) -> Tm;
}

/// The host's time zone (`TZ`, else the system's).
pub struct SystemTz;

impl LocalTime for SystemTz {
    fn local(&self, secs: i64) -> Tm {
        extern "C" {
            fn tzset();
        }
        let t: libc::time_t = secs as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: plain libc calls on values we own; tzset makes
        // localtime_r see a changed TZ.
        let ok = unsafe {
            tzset();
            !libc::localtime_r(&t, &mut tm).is_null()
        };
        if !ok {
            return FixedOffset(0).local(secs);
        }
        Tm {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1) as u32,
            mday: tm.tm_mday as u32,
            hour: tm.tm_hour as u32,
            min: tm.tm_min as u32,
            wday: tm.tm_wday as u32,
        }
    }
}

/// A fixed offset from UTC in seconds (tests, and a fallback).
pub struct FixedOffset(pub i64);

impl LocalTime for FixedOffset {
    fn local(&self, secs: i64) -> Tm {
        let s = secs + self.0;
        let days = s.div_euclid(86_400);
        let rem = s.rem_euclid(86_400);
        // Howard Hinnant's civil_from_days.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
        Tm {
            year: y as i32,
            month: m as u32,
            mday: d as u32,
            hour: (rem / 3600) as u32,
            min: ((rem % 3600) / 60) as u32,
            wday: (days + 4).rem_euclid(7) as u32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minutes: u64,
    hours: u32,
    mdays: u32,
    months: u16,
    wdays: u8,
    mday_any: bool,
    wday_any: bool,
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
const DAY_NAMES: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

fn value(s: &str, names: &[&str], base: u32) -> Option<u32> {
    if let Ok(n) = s.parse::<u32>() {
        return Some(n);
    }
    let l = s.to_ascii_lowercase();
    names.iter().position(|n| *n == l).map(|i| i as u32 + base)
}

/// One field as a bit set over `lo..=hi`; `true` when it was `*`.
fn field(src: &str, lo: u32, hi: u32, names: &[&str], name_base: u32, what: &str) -> Result<(u64, bool), String> {
    let mut bits = 0u64;
    let any = src == "*" || src == "?";
    for part in src.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => {
                let s: u32 = s.parse().map_err(|_| format!("{what}: bad step {s:?}"))?;
                if s == 0 {
                    return Err(format!("{what}: a step can't be 0"));
                }
                (r, s)
            }
            None => (part, 1),
        };
        let (a, b) = if range == "*" || range == "?" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            let a = value(a, names, name_base).ok_or_else(|| format!("{what}: bad value {a:?}"))?;
            let b = value(b, names, name_base).ok_or_else(|| format!("{what}: bad value {b:?}"))?;
            (a, b)
        } else {
            let a = value(range, names, name_base).ok_or_else(|| format!("{what}: bad value {range:?}"))?;
            // `5/15` means from 5 to the end, every 15.
            (a, if part.contains('/') { hi } else { a })
        };
        if a < lo || b > hi || a > b {
            return Err(format!("{what}: {range} is outside {lo}–{hi}"));
        }
        let mut v = a;
        while v <= b {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok((bits, any))
}

impl Cron {
    pub fn parse(src: &str) -> Result<Cron, String> {
        let src = src.trim();
        let expanded = match src.to_ascii_lowercase().as_str() {
            "@hourly" => "0 * * * *".to_string(),
            "@daily" | "@midnight" => "0 0 * * *".to_string(),
            "@weekly" => "0 0 * * 0".to_string(),
            "@monthly" => "0 0 1 * *".to_string(),
            s if s.starts_with('@') => return Err(format!("unknown schedule {src}")),
            _ => src.to_string(),
        };
        let f: Vec<&str> = expanded.split_whitespace().collect();
        if f.len() != 5 {
            return Err("a schedule has five fields: minute hour day-of-month month day-of-week".into());
        }
        let (minutes, _) = field(f[0], 0, 59, &[], 0, "minute")?;
        let (hours, _) = field(f[1], 0, 23, &[], 0, "hour")?;
        let (mdays, mday_any) = field(f[2], 1, 31, &[], 0, "day of month")?;
        let (months, _) = field(f[3], 1, 12, &MONTHS, 1, "month")?;
        let (mut wdays, wday_any) = field(f[4], 0, 7, &DAYS, 0, "day of week")?;
        if wdays & (1 << 7) != 0 {
            wdays = (wdays & !(1 << 7)) | 1;
        }
        let c = Cron {
            minutes,
            hours: hours as u32,
            mdays: mdays as u32,
            months: months as u16,
            wdays: wdays as u8,
            mday_any,
            wday_any,
        };
        if let Some(gap) = c.smallest_gap() {
            if gap < MIN_INTERVAL_MINUTES {
                return Err(format!("runs at most every {MIN_INTERVAL_MINUTES} minutes"));
            }
        }
        Ok(c)
    }

    /// The smallest gap in minutes between two runs within a day.
    fn smallest_gap(&self) -> Option<u32> {
        let mins: Vec<u32> = (0..60).filter(|m| self.minutes & (1 << m) != 0).collect();
        let hours: Vec<u32> = (0..24).filter(|h| self.hours & (1 << h) != 0).collect();
        let mut times: Vec<u32> = hours.iter().flat_map(|h| mins.iter().map(move |m| h * 60 + m)).collect();
        times.sort();
        if times.len() < 2 {
            return None;
        }
        let mut gap = times.windows(2).map(|w| w[1] - w[0]).min().unwrap();
        // Across midnight.
        gap = gap.min(times[0] + 1440 - times[times.len() - 1]);
        Some(gap)
    }

    fn day_matches(&self, tm: &Tm) -> bool {
        let md = self.mdays & (1 << tm.mday) != 0;
        let wd = self.wdays & (1 << tm.wday) != 0;
        match (self.mday_any, self.wday_any) {
            (true, true) => true,
            (false, true) => md,
            (true, false) => wd,
            (false, false) => md || wd,
        }
    }

    /// The first time strictly after `after_ms` (Unix ms) that matches, in
    /// Unix ms; `None` if nothing matches within five years (`0 0 30 2 *`).
    pub fn next_after(&self, after_ms: i64, tz: &dyn LocalTime) -> Option<i64> {
        let start = after_ms.div_euclid(1000);
        let before = tz.local(start);
        let horizon = start + 5 * 366 * 86_400;
        let mut t = start - start.rem_euclid(60) + 60;
        for _ in 0..200_000 {
            if t > horizon {
                return None;
            }
            let tm = tz.local(t);
            if self.months & (1 << tm.month) == 0 || !self.day_matches(&tm) {
                t += ((23 - tm.hour as i64) * 60 + (60 - tm.min as i64)) * 60;
                continue;
            }
            if self.hours & (1 << tm.hour) == 0 {
                t += (60 - tm.min as i64) * 60;
                continue;
            }
            if self.minutes & (1 << tm.min) == 0 {
                match (tm.min + 1..60).find(|m| self.minutes & (1 << m) != 0) {
                    Some(m) => t += (m - tm.min) as i64 * 60,
                    None => t += (60 - tm.min as i64) * 60,
                }
                continue;
            }
            // When clocks go back an hour, the same local minute comes twice:
            // run once.
            let same = |a: &Tm| (a.year, a.month, a.mday, a.hour, a.min);
            if same(&tm) == same(&before) {
                t += 60;
                continue;
            }
            return Some(t * 1000);
        }
        None
    }

    /// The schedule in words, when it is a common one.
    pub fn describe(&self) -> Option<String> {
        let mins: Vec<u32> = (0..60).filter(|m| self.minutes & (1 << m) != 0).collect();
        let hours: Vec<u32> = (0..24).filter(|h| self.hours & (1 << h) != 0).collect();
        let all_days = self.mday_any && self.wday_any && self.months == 0b1_1111_1111_1110;
        let step = |v: &[u32], n: u32| -> Option<u32> {
            if v.len() < 2 || v[0] != 0 {
                return None;
            }
            let s = v[1] - v[0];
            (v.windows(2).all(|w| w[1] - w[0] == s) && v.len() as u32 * s == n).then_some(s)
        };
        if all_days && hours.len() == 24 {
            if let Some(s) = step(&mins, 60) {
                return Some(format!("Every {s} minutes"));
            }
            if mins.len() == 1 {
                return Some(if mins[0] == 0 { "Every hour".into() } else { format!("Every hour at :{:02}", mins[0]) });
            }
            return None;
        }
        if all_days && mins.len() == 1 {
            if let Some(s) = step(&hours, 24) {
                return Some(if mins[0] == 0 { format!("Every {s} hours") } else { format!("Every {s} hours at :{:02}", mins[0]) });
            }
        }
        if mins.len() != 1 || hours.len() != 1 || self.months != 0b1_1111_1111_1110 {
            return None;
        }
        let at = format!("{:02}:{:02}", hours[0], mins[0]);
        match (self.mday_any, self.wday_any) {
            (true, true) => Some(format!("Every day at {at}")),
            (true, false) => {
                let days: Vec<usize> = (0..7).filter(|d| self.wdays & (1 << d) != 0).collect();
                Some(match days.as_slice() {
                    [1, 2, 3, 4, 5] => format!("Weekdays at {at}"),
                    [0, 6] => format!("Weekends at {at}"),
                    [d] => format!("Every {} at {at}", DAY_NAMES[*d]),
                    ds => format!(
                        "{} at {at}",
                        ds.iter().map(|d| &DAY_NAMES[*d][..3]).collect::<Vec<_>>().join(", ")
                    ),
                })
            }
            (false, true) => {
                let days: Vec<u32> = (1..32).filter(|d| self.mdays & (1 << d) != 0).collect();
                match days.as_slice() {
                    [d] => Some(format!("Monthly on the {} at {at}", ordinal(*d))),
                    _ => None,
                }
            }
            (false, false) => None,
        }
    }
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, x) if x != 11 => "st",
        (2, x) if x != 12 => "nd",
        (3, x) if x != 13 => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-20 (a Friday) 12:00 UTC, in ms.
    const FRI_NOON: i64 = 1_795_176_000_000;

    fn next(expr: &str, after: i64) -> i64 {
        Cron::parse(expr).unwrap().next_after(after, &FixedOffset(0)).unwrap()
    }

    #[test]
    fn civil_time() {
        let tm = FixedOffset(0).local(FRI_NOON / 1000);
        assert_eq!(tm, Tm { year: 2026, month: 11, mday: 20, hour: 12, min: 0, wday: 5 });
        assert_eq!(FixedOffset(0).local(0), Tm { year: 1970, month: 1, mday: 1, hour: 0, min: 0, wday: 4 });
        let tm = FixedOffset(2 * 3600).local(FRI_NOON / 1000);
        assert_eq!((tm.hour, tm.mday), (14, 20));
    }

    #[test]
    fn next_times() {
        let h = 3_600_000;
        // Daily at 09:00: tomorrow.
        assert_eq!(next("0 9 * * *", FRI_NOON), FRI_NOON + 21 * h);
        // Later today.
        assert_eq!(next("30 13 * * *", FRI_NOON), FRI_NOON + h + 30 * 60_000);
        // Strictly after: the same minute is not "next".
        assert_eq!(next("0 12 * * *", FRI_NOON), FRI_NOON + 24 * h);
        // Weekdays at 09:00 from Friday noon: Monday.
        assert_eq!(next("0 9 * * 1-5", FRI_NOON), FRI_NOON + (2 * 24 + 21) * h);
        assert_eq!(next("0 9 * * mon-fri", FRI_NOON), FRI_NOON + (2 * 24 + 21) * h);
        // Sunday as 7.
        assert_eq!(next("0 0 * * 7", FRI_NOON), FRI_NOON + (24 + 12) * h);
        // First of the month: 1 December.
        assert_eq!(next("@monthly", FRI_NOON), FRI_NOON + (10 * 24 + 12) * h);
        // Either day field when both are set: the 25th or a Monday.
        assert_eq!(next("0 0 25 * 1", FRI_NOON), FRI_NOON + (2 * 24 + 12) * h);
        assert_eq!(next("*/15 * * * *", FRI_NOON), FRI_NOON + 15 * 60_000);
        assert_eq!(next("0 */6 * * *", FRI_NOON), FRI_NOON + 6 * h);
        // In a zone two hours ahead, 09:00 local is 07:00 UTC.
        let c = Cron::parse("0 9 * * *").unwrap();
        assert_eq!(c.next_after(FRI_NOON, &FixedOffset(7200)).unwrap(), FRI_NOON + 19 * h);
        // Never.
        assert_eq!(Cron::parse("0 0 30 2 *").unwrap().next_after(FRI_NOON, &FixedOffset(0)), None);
    }

    /// A zone that goes back an hour at 02:00 local on day 0 (so 01:00–02:00 repeats).
    struct FallBack {
        at: i64,
    }
    impl LocalTime for FallBack {
        fn local(&self, secs: i64) -> Tm {
            FixedOffset(if secs < self.at { 2 * 3600 } else { 3600 }).local(secs)
        }
    }

    #[test]
    fn repeated_hour_runs_once() {
        // Local 02:00 at +2 is 00:00 UTC; afterwards the zone is +1.
        let day = 1_795_132_800; // 2026-11-20 00:00 UTC
        let tz = FallBack { at: day };
        let c = Cron::parse("30 1 * * *").unwrap();
        let first = c.next_after((day - 3 * 3600) * 1000, &tz).unwrap();
        assert_eq!(first, (day - 30 * 60) * 1000, "01:30 at +2");
        let second = c.next_after(first, &tz).unwrap();
        assert!(second > (day + 20 * 3600) * 1000, "not again at 01:30 at +1: {}", second / 1000 - day);
    }

    #[test]
    fn parse_errors() {
        for bad in ["", "* * * *", "61 * * * *", "* 24 * * *", "* * 0 * *", "* * * 13 *", "* * * * 8", "*/0 * * * *", "x * * * *", "@yearlyish", "* * * * *", "*/2 * * * *"] {
            assert!(Cron::parse(bad).is_err(), "{bad:?}");
        }
        assert!(Cron::parse("*/5 * * * *").is_ok());
        assert!(Cron::parse("0,30 9-17 * jan-jun MON").is_ok());
    }

    #[test]
    fn descriptions() {
        let d = |s: &str| Cron::parse(s).unwrap().describe();
        assert_eq!(d("0 9 * * *").as_deref(), Some("Every day at 09:00"));
        assert_eq!(d("@daily").as_deref(), Some("Every day at 00:00"));
        assert_eq!(d("0 9 * * 1-5").as_deref(), Some("Weekdays at 09:00"));
        assert_eq!(d("30 18 * * 0,6").as_deref(), Some("Weekends at 18:30"));
        assert_eq!(d("0 8 * * 1").as_deref(), Some("Every Monday at 08:00"));
        assert_eq!(d("0 8 * * 1,3,5").as_deref(), Some("Mon, Wed, Fri at 08:00"));
        assert_eq!(d("0 3 1 * *").as_deref(), Some("Monthly on the 1st at 03:00"));
        assert_eq!(d("0 3 22 * *").as_deref(), Some("Monthly on the 22nd at 03:00"));
        assert_eq!(d("@hourly").as_deref(), Some("Every hour"));
        assert_eq!(d("15 * * * *").as_deref(), Some("Every hour at :15"));
        assert_eq!(d("*/10 * * * *").as_deref(), Some("Every 10 minutes"));
        assert_eq!(d("0 */4 * * *").as_deref(), Some("Every 4 hours"));
        assert_eq!(d("0 9 1 1 *"), None);
    }
}
