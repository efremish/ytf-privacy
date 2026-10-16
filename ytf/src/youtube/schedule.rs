use anyhow::Result;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};

use super::api;
use super::auth::Session;
use crate::settings::Context;

/// Занятые СЛОТЫ: дата+время (UTC, «YYYY-MM-DDTHH:MM»).
///
/// Раньше хранили только дату — из-за этого видео и шортс не могли выйти
/// в один день даже в разное время. Теперь сравниваем дату+время.
pub async fn occupied_slots(session: &Session, uploads_playlist: &str) -> Result<Vec<String>> {
    let vids = api::channel_videos(session, uploads_playlist, 300).await?;
    let mut slots: Vec<String> = vids.iter().filter_map(|v| slot_key(&v.publish_at)).collect();
    slots.sort();
    slots.dedup();
    Ok(slots)
}

/// «2026-10-17T16:00:00Z» -> «2026-10-17T16:00». None, если формат не тот.
pub fn slot_key(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() >= 16 && s.as_bytes().get(10) == Some(&b'T') {
        Some(s[..16].to_string())
    } else {
        None
    }
}

/// «HH:MM» -> минуты от полуночи
fn hm(s: &str) -> Option<i64> {
    let h: i64 = s.get(0..2)?.parse().ok()?;
    let m: i64 = s.get(3..5)?.parse().ok()?;
    Some(h * 60 + m)
}

/// Насколько близко по времени считаем слот «тем же» (минуты). Небольшой
/// запас, чтобы расхождение на пару минут не привело к двойной публикации.
const SLOT_TOLERANCE_MIN: i64 = 30;

/// Свободен ли день `day` для публикации в `hour:minute` (в зоне `offset`).
/// Конфликт — только если в этот же день уже есть публикация рядом по времени.
pub fn slot_free(
    occupied: &[String],
    day: NaiveDate,
    hour: u32,
    minute: u32,
    offset_hours: i64,
) -> bool {
    let target = publish_at(day, hour, minute, offset_hours);
    let Some(tkey) = slot_key(&target) else { return true };
    let tdate = &tkey[..10];
    let tmin = hm(&tkey[11..16]).unwrap_or(0);
    for o in occupied {
        if o.len() < 16 || &o[..10] != tdate {
            continue;
        }
        if let Some(omin) = hm(&o[11..16]) {
            if (omin - tmin).abs() <= SLOT_TOLERANCE_MIN {
                return false;
            }
        }
    }
    true
}

/// Ближайший день, где слот `hour:minute` свободен.
///
/// Логика Матвея: «есть видео на сегодня? если нет — на сегодня, если есть —
/// на завтра». Теперь то же, но с учётом времени: публикация в другое время
/// того же дня не мешает.
pub fn next_free_slot(
    occupied: &[String],
    from: DateTime<Utc>,
    horizon_days: i64,
    hour: u32,
    minute: u32,
    offset_hours: i64,
) -> NaiveDate {
    let today: NaiveDate = from.date_naive();
    let max_look = horizon_days.max(1);

    for i in 0..=max_look {
        let d = today + Duration::days(i);
        if slot_free(occupied, d, hour, minute, offset_hours) {
            return d;
        }
    }
    today + Duration::days(max_look)
}

/// Разобрать "HH:MM" в часы/минуты
pub fn parse_hhmm(s: &str) -> (u32, u32) {
    let mut it = s.split(':');
    let h = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(22);
    let m = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    (h.clamp(0, 23), m.clamp(0, 59))
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

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn слот_ключ_из_rfc3339() {
        assert_eq!(slot_key("2026-10-17T16:00:00Z"), Some("2026-10-17T16:00".into()));
        assert_eq!(slot_key("2026-10-17"), None);
    }

    #[test]
    fn свободный_день_берём_сегодня() {
        // занято 28-е в 16:00 UTC, сегодня 26-е — берём 26-е
        let occ = vec!["2026-09-28T16:00".to_string()];
        let d = next_free_slot(&occ, Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap(), 1, 22, 0, 6);
        assert_eq!(d, day(2026, 9, 26));
    }

    #[test]
    fn то_же_время_занято_берём_завтра() {
        // 26-е 16:00 UTC занято, публикация тоже 22:00 Омск (=16:00 UTC)
        let occ = vec!["2026-09-26T16:00".to_string()];
        let d = next_free_slot(&occ, Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap(), 3, 22, 0, 6);
        assert_eq!(d, day(2026, 9, 27));
    }

    #[test]
    fn разное_время_в_один_день_можно() {
        // 26-е 16:00 UTC (22:00 Омск) занято, шортс хотим в 12:00 Омск (=06:00 UTC)
        let occ = vec!["2026-09-26T16:00".to_string()];
        let d = next_free_slot(&occ, Utc.with_ymd_and_hms(2026, 9, 26, 4, 0, 0).unwrap(), 3, 12, 0, 6);
        assert_eq!(d, day(2026, 9, 26), "разное время — день свободен");
    }

    #[test]
    fn три_дня_подряд_заняты_в_то_же_время() {
        let occ = vec![
            "2026-09-26T16:00".to_string(),
            "2026-09-27T16:00".to_string(),
            "2026-09-28T16:00".to_string(),
        ];
        let d = next_free_slot(&occ, Utc.with_ymd_and_hms(2026, 9, 26, 10, 0, 0).unwrap(), 7, 22, 0, 6);
        assert_eq!(d, day(2026, 9, 29));
    }

    #[test]
    fn омск_22_00_это_16_00_utc() {
        assert_eq!(publish_at(day(2026, 9, 26), 22, 0, 6), "2026-09-26T16:00:00Z");
    }
}
