//! FILETIME とローカル日時の相互変換、および Everything 互換の日付式の解釈。

use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::Storage::FileSystem::{FileTimeToLocalFileTime, LocalFileTimeToFileTime};
use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToFileTime};

pub const TICKS_PER_SEC: i64 = 10_000_000;
pub const TICKS_PER_DAY: i64 = 86_400 * TICKS_PER_SEC;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalDateTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub weekday: u16,
}

pub fn now_local() -> LocalDateTime {
    // SAFETY: 出力専用構造体。
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    LocalDateTime {
        year: st.wYear,
        month: st.wMonth,
        day: st.wDay,
        hour: st.wHour,
        minute: st.wMinute,
        second: st.wSecond,
        weekday: st.wDayOfWeek,
    }
}

/// ローカル日時（UTC 補正なし）→ UTC FILETIME（ticks）。無効な日付は None。
pub fn local_to_filetime(y: u16, m: u16, d: u16, hh: u16, mm: u16, ss: u16) -> Option<i64> {
    let st = SYSTEMTIME {
        wYear: y,
        wMonth: m,
        wDayOfWeek: 0,
        wDay: d,
        wHour: hh,
        wMinute: mm,
        wSecond: ss,
        wMilliseconds: 0,
    };
    let mut local = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let mut utc = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    // SAFETY: すべて有効なスタック上の構造体。
    unsafe {
        if SystemTimeToFileTime(&st, &mut local) == 0 {
            return None;
        }
        if LocalFileTimeToFileTime(&local, &mut utc) == 0 {
            return None;
        }
    }
    Some(((utc.dwHighDateTime as i64) << 32) | utc.dwLowDateTime as i64)
}

/// UTC FILETIME → ローカル日時。
pub fn filetime_to_local(ft: i64) -> Option<LocalDateTime> {
    if ft <= 0 {
        return None;
    }
    let utc = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
    let mut local = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
    // SAFETY: 有効な構造体。
    unsafe {
        if FileTimeToLocalFileTime(&utc, &mut local) == 0 {
            return None;
        }
        if FileTimeToSystemTime(&local, &mut st) == 0 {
            return None;
        }
    }
    Some(LocalDateTime {
        year: st.wYear,
        month: st.wMonth,
        day: st.wDay,
        hour: st.wHour,
        minute: st.wMinute,
        second: st.wSecond,
        weekday: st.wDayOfWeek,
    })
}

pub fn format_filetime(ft: i64) -> String {
    match filetime_to_local(ft) {
        Some(t) => format!("{:04}/{:02}/{:02} {:02}:{:02}", t.year, t.month, t.day, t.hour, t.minute),
        None => String::new(),
    }
}

fn days_in_month(y: u16, m: u16) -> u16 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400) {
                29
            } else {
                28
            }
        }
    }
}

fn day_start(y: u16, m: u16, d: u16) -> Option<i64> {
    local_to_filetime(y, m, d, 0, 0, 0)
}

fn add_days(y: u16, m: u16, d: u16, delta: i64) -> (u16, u16, u16) {
    let base = day_start(y, m, d).unwrap_or(0) + delta * TICKS_PER_DAY + TICKS_PER_DAY / 2;
    let t = filetime_to_local(base).unwrap_or(LocalDateTime { year: y, month: m, day: d, hour: 0, minute: 0, second: 0, weekday: 0 });
    (t.year, t.month, t.day)
}

/// 日付式 → [start, end) の FILETIME 範囲。`end` は排他的。
/// 対応: today, yesterday, thisweek, lastweek, thismonth, lastmonth, thisyear, lastyear,
/// lastNdays/pastNdays/Ndays, YYYY, YYYY-MM, YYYY-MM-DD, YYYY/MM/DD, 月名。
pub fn parse_date_range(s: &str) -> Option<(i64, i64)> {
    let s = s.trim().to_ascii_lowercase();
    let now = now_local();
    let today = day_start(now.year, now.month, now.day)?;
    let month_start = |y: u16, m: u16| day_start(y, m, 1);
    let next_month = |y: u16, m: u16| if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    let prev_month = |y: u16, m: u16| if m == 1 { (y - 1, 12) } else { (y, m - 1) };
    match s.as_str() {
        "today" => return Some((today, today + TICKS_PER_DAY)),
        "yesterday" => return Some((today - TICKS_PER_DAY, today)),
        "tomorrow" => return Some((today + TICKS_PER_DAY, today + 2 * TICKS_PER_DAY)),
        "thisweek" | "currentweek" => {
            let start = today - now.weekday as i64 * TICKS_PER_DAY;
            return Some((start, start + 7 * TICKS_PER_DAY));
        }
        "lastweek" | "pastweek" | "prevweek" => {
            let start = today - now.weekday as i64 * TICKS_PER_DAY - 7 * TICKS_PER_DAY;
            return Some((start, start + 7 * TICKS_PER_DAY));
        }
        "thismonth" | "currentmonth" => {
            let (ny, nm) = next_month(now.year, now.month);
            return Some((month_start(now.year, now.month)?, month_start(ny, nm)?));
        }
        "lastmonth" | "pastmonth" | "prevmonth" => {
            let (py, pm) = prev_month(now.year, now.month);
            return Some((month_start(py, pm)?, month_start(now.year, now.month)?));
        }
        "thisyear" | "currentyear" => return Some((day_start(now.year, 1, 1)?, day_start(now.year + 1, 1, 1)?)),
        "lastyear" | "pastyear" | "prevyear" => return Some((day_start(now.year - 1, 1, 1)?, day_start(now.year, 1, 1)?)),
        "nextweek" | "comingweek" => {
            let start = today - now.weekday as i64 * TICKS_PER_DAY + 7 * TICKS_PER_DAY;
            return Some((start, start + 7 * TICKS_PER_DAY));
        }
        "nextmonth" | "comingmonth" => {
            let (ny, nm) = next_month(now.year, now.month);
            let (ny2, nm2) = next_month(ny, nm);
            return Some((month_start(ny, nm)?, month_start(ny2, nm2)?));
        }
        "nextyear" | "comingyear" => return Some((day_start(now.year + 1, 1, 1)?, day_start(now.year + 2, 1, 1)?)),
        "thisday" | "currentday" => return Some((today, today + TICKS_PER_DAY)),
        "lastday" | "pastday" | "prevday" => return Some((today - TICKS_PER_DAY, today)),
        "nextday" | "comingday" => return Some((today + TICKS_PER_DAY, today + 2 * TICKS_PER_DAY)),
        "unknown" => return Some((0, 1)),
        _ => {}
    }
    // 曜日名: 今日以前で最も近いその曜日（今日がその曜日なら今日）
    let weekdays = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];
    for (i, w) in weekdays.iter().enumerate() {
        if s == *w || (s.len() == 3 && w.starts_with(&s)) {
            let back = (now.weekday as i64 + 7 - i as i64) % 7;
            let start = today - back * TICKS_PER_DAY;
            return Some((start, start + TICKS_PER_DAY));
        }
    }
    // nextNdays / comingNdays: 今から N 単位先まで
    if let Some(rel) = s.strip_prefix("next").or_else(|| s.strip_prefix("coming")) {
        let digits: String = rel.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() && rel.len() > digits.len() {
            let n: i64 = digits.parse().ok()?;
            let span = unit_span(rel[digits.len()..].trim(), n)?;
            let now_ft = local_to_filetime(now.year, now.month, now.day, now.hour, now.minute, now.second)?;
            return Some((now_ft, now_ft + span));
        }
    }
    // lastNdays / pastNdays / Ndays / lastNhours / lastNweeks / lastNmonths / lastNyears
    let rel = s.trim_start_matches("last").trim_start_matches("past").trim_start_matches("prev");
    let digits: String = rel.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && rel.len() > digits.len() && rel.as_bytes()[digits.len()].is_ascii_alphabetic() {
        let n: i64 = digits.parse().ok()?;
        let span = unit_span(rel[digits.len()..].trim(), n)?;
        let now_ft = local_to_filetime(now.year, now.month, now.day, now.hour, now.minute, now.second)?;
        return Some((now_ft - span, now_ft + 1));
    }
    // 月名
    let months = [
        "january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november",
        "december",
    ];
    for (i, m) in months.iter().enumerate() {
        if s == *m || (s.len() == 3 && m.starts_with(&s)) {
            let mo = i as u16 + 1;
            let (ny, nm) = next_month(now.year, mo);
            return Some((month_start(now.year, mo)?, month_start(ny, nm)?));
        }
    }
    // 数値形式。`YYYY-MM-DDTHH:MM[:SS]`（T または空白区切り）は分または秒単位の範囲
    let (date_part, time_part) = match s.split_once(['t', ' ']) {
        Some((d, t)) if !t.is_empty() && t.contains(':') => (d, Some(t)),
        _ => (s.as_str(), None),
    };
    let parts: Vec<&str> = date_part.split(['-', '/', '.']).collect();
    let nums: Option<Vec<u16>> = parts.iter().map(|p| p.parse::<u16>().ok()).collect();
    let nums = nums?;
    if let Some(t) = time_part {
        let (y, m, d) = match nums.as_slice() {
            [y, m, d] if *y >= 1601 => (*y, *m, *d),
            [d, m, y] | [m, d, y] if *y >= 1601 && (1..=12).contains(m) => (*y, *m, *d),
            _ => return None,
        };
        let tn: Vec<u16> = t.split(':').filter_map(|x| x.parse().ok()).collect();
        return match tn.as_slice() {
            [hh, mm] if *hh < 24 && *mm < 60 => {
                let st = local_to_filetime(y, m, d, *hh, *mm, 0)?;
                Some((st, st + 60 * TICKS_PER_SEC))
            }
            [hh, mm, ss] if *hh < 24 && *mm < 60 && *ss < 60 => {
                let st = local_to_filetime(y, m, d, *hh, *mm, *ss)?;
                Some((st, st + TICKS_PER_SEC))
            }
            _ => None,
        };
    }
    match nums.as_slice() {
        [y] if *y >= 1601 => Some((day_start(*y, 1, 1)?, day_start(*y + 1, 1, 1)?)),
        [y, m] if *y >= 1601 && (1..=12).contains(m) => {
            let (ny, nm) = next_month(*y, *m);
            Some((month_start(*y, *m)?, month_start(ny, nm)?))
        }
        [y, m, d] if *y >= 1601 && (1..=12).contains(m) && *d >= 1 && *d <= days_in_month(*y, *m) => {
            let st = day_start(*y, *m, *d)?;
            Some((st, st + TICKS_PER_DAY))
        }
        [d, m, y] | [m, d, y] if *y >= 1601 && (1..=12).contains(m) && *d >= 1 && *d <= 31 => {
            let st = day_start(*y, *m, *d)?;
            Some((st, st + TICKS_PER_DAY))
        }
        _ => None,
    }
}

/// 相対日付の単位 → ticks。
fn unit_span(unit: &str, n: i64) -> Option<i64> {
    Some(match unit {
        "day" | "days" | "d" => n * TICKS_PER_DAY,
        "hour" | "hours" | "h" => n * 3600 * TICKS_PER_SEC,
        "minute" | "minutes" | "min" | "mins" => n * 60 * TICKS_PER_SEC,
        "second" | "seconds" | "sec" | "secs" => n * TICKS_PER_SEC,
        "week" | "weeks" | "w" => n * 7 * TICKS_PER_DAY,
        "month" | "months" => n * 30 * TICKS_PER_DAY,
        "year" | "years" | "y" => n * 365 * TICKS_PER_DAY,
        _ => return None,
    })
}

pub fn add_days_pub(y: u16, m: u16, d: u16, delta: i64) -> (u16, u16, u16) {
    add_days(y, m, d, delta)
}
