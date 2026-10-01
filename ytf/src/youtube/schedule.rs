use anyhow::Result;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};

use super::api;
use super::auth::Session;
use crate::settings::Context;

/// Даты, уже занятые на канале — и отложенные, и опубликованные
pub async fn occupied(session: &Session, uploads_playlist: &str) -> Result<Vec<String>> {
    let vids = api::channel_videos(session, uploads_playlist, 300).await?;
    let mut dates: Vec<String> = vids
        .iter()
        .filter_map(|v| {
            let s = v.publish_at.trim();
            if s.len() >= 10 && s[..4].chars().all(|c| c.is_ascii_digit()) {
                Some(s[..10].to_string())
            } else {
                None
            }
        })
        .collect();
    dates.sort();
    dates.dedup();
    Ok(dates)
}

/// Разобрать "HH:MM" в часы/минуты
pub fn parse_hhmm(s: &str) -> (u32, u32) {
    let mut it = s.split(':');
    let h = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(22);
    let m = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    (h.clamp(0, 23), m.clamp(0, 59))
}

/// Ближайший свободный день начиная с `from`.
///
/// Логика как просил Матвей: «смотрит, есть ли видео на сегодня. Если нет —
/// выкладывает на сегодня, если есть — на завтра». Плюс `horizon` дней вперёд.
pub fn next_free_day(occupied: &[String], from: DateTime<Utc>, horizon_days: i64) -> NaiveDate {
    let today: NaiveDate = from.date_naive();
    let max_look = horizon_days.max(1);

    for i in 0..=(max_look as i64) {
        let d = today + Duration::days(i);
        let key = d.format("%Y-%m-%d").to_string();
        if !occupied.iter().any(|o| o == &key) {
            return d;
        }
    }
    // горизонт кончился — всё занято, берём следующий день после него
    today + Duration::days(max_look)
}

/// Собрать `publishAt` в формате, который ждёт YouTube (UTC, RFC3339)
pub fn publish_at(day: NaiveDate, hour: u32, minute: u32, offset_hours: i64) -> String {
    // Локальное время канала -> UTC
    let local = day
        .and_hms_opt(hour.clamp(0, 23), minute.clamp(0, 59), 0)
        .unwrap_or_default();
    let utc = local - Duration::hours(offset_hours);
    Utc.from_utc_datetime(&utc)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// Смещение канала в часах от UTC. По умолчанию Омск = UTC+6
pub fn tz_offset(ctx: &Context, channel_override: Option<&str>) -> i64 {
    if let Some(s) = channel_override {
        let (h, m) = parse_hhmm(s);
        let _ = (h, m);
    }
    match ctx.settings.publish.timezone.as_deref() {
        Some("Asia/Omsk") | None => 6,
        Some(tz) => offset_from_tz(tz),
    }
}

/// Смещение в часах для поддерживаемых зон. Для остальных — 6 (Омск)
fn offset_from_tz(tz: &str) -> i64 {
    use chrono_tz::Tz;
    let Ok(t) = tz.parse::<Tz>() else { return 6 };
    match t {
        Tz::UTC => 0,
        Tz::Europe__Kaliningrad => 2,
        Tz::Europe__Moscow => 3,
        Tz::Asia__Yekaterinburg => 5,
        Tz::Asia__Omsk => 6,
        Tz::Asia__Krasnoyarsk => 7,
        Tz::Asia__Irkutsk => 8,
        Tz::Asia__Yakutsk => 9,
        Tz::Asia__Vladivostok => 10,
        _ => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn сегодня_свободен() {
        let now = Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap();
        let d = next_free_day(&["2026-09-28".into()], now, 1);
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 9, 26).unwrap());
    }

    #[test]
    fn сегодня_занят_берём_завтра() {
        let now = Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap();
        let d = next_free_day(&["2026-09-26".into()], now, 3);
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 9, 27).unwrap());
    }

    #[test]
    fn три_дня_подряд_заняты() {
        let now = Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap();
        let occ = vec![
            "2026-09-26".to_string(),
            "2026-09-27".to_string(),
            "2026-09-28".to_string(),
        ];
        let d = next_free_day(&occ, now, 7);
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 9, 29).unwrap());
    }

    #[test]
    fn омск_22_00_это_16_00_utc() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 26).unwrap();
        assert_eq!(publish_at(day, 22, 0, 6), "2026-09-26T16:00:00Z");
    }
}
