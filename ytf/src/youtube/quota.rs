use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::settings::Context;

/// Стоимость методов в общем бакете (10 000 ед./сутки).
/// `videos.insert` и `search.list` сюда НЕ входят — у них отдельные бакеты.
pub fn general_cost(method: &str) -> u32 {
    match method {
        "videos.list" | "channels.list" | "playlistItems.list" | "i18nLanguages.list"
        | "i18nRegions.list" | "videoCategories.list" => 1,
        "thumbnails.set" | "videos.update" | "videos.delete" | "captions.insert" => 50,
        "search.list" => 0, // отдельный бакет
        "videos.insert" => 0, // отдельный бакет
        "analytics.query" => 5,
        _ => 1,
    }
}

/// Отдельный бакет: 100 вызовов в сутки, 1 единица за вызов
pub fn is_bucket_method(method: &str) -> bool {
    matches!(method, "videos.insert" | "search.list")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaState {
    /// дата YYYY-MM-DD
    pub date: String,
    /// вызовы videos.insert сегодня
    pub inserts: u32,
    /// вызовы search.list сегодня
    pub searches: u32,
    /// единицы из общего бакета
    pub general: u32,
}

impl Default for QuotaState {
    fn default() -> Self {
        Self { date: String::new(), inserts: 0, searches: 0, general: 0 }
    }
}

impl QuotaState {
    pub fn load(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else { return Self::default() };
        let Ok(mut s) = serde_json::from_str::<QuotaState>(&raw) else { return Self::default() };
        let today = today();
        if s.date != today {
            // сброс в 00:00 по Pacific Time, но локально достаточно по дате
            s = Self { date: today, ..Default::default() };
        }
        s
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Списать вызов. Возвращает ошибку, если бакет исчерпан
    pub fn charge(&mut self, method: &str, limit_insert: u32, limit_general: u32, warn_pct: u32) -> Result<(), String> {
        if self.date != today() {
            *self = Self { date: today(), ..Default::default() };
        }
        if is_bucket_method(method) {
            let (used, limit) = if method == "videos.insert" {
                (&mut self.inserts, limit_insert)
            } else {
                (&mut self.searches, limit_insert)
            };
            if *used >= limit {
                return Err(format!(
                    "бакет {method} исчерпан: {used}/{limit} в сутки (сброс в 00:00 PT)"
                ));
            }
            if *used * 100 / limit.max(1) >= warn_pct {
                tracing::warn!("бакет {method}: {used}/{limit} (>{warn_pct}%)");
            }
            *used += 1;
        } else {
            let cost = general_cost(method);
            let next = self.general + cost;
            if next > limit_general {
                return Err(format!(
                    "общий бакет исчерпан: {}/{} (метод {method}, +{cost})",
                    self.general, limit_general
                ));
            }
            self.general = next;
        }
        Ok(())
    }
}

pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

pub async fn print_quota(ctx: &Context) -> Result<()> {
    let st = QuotaState::load(&ctx.state_file());
    let a = &ctx.settings.api;

    let day = if st.date.is_empty() { today() } else { st.date.clone() };
    println!("Квота на сегодня ({day})\n");
    println!("  videos.insert : {:>5} / {}   (отдельный бакет, 1 ед. за вызов)",
        st.inserts, a.insert_bucket_per_day);
    println!("  search.list   : {:>5} / {}   (отдельный бакет, 1 ед. за вызов)",
        st.searches, a.insert_bucket_per_day);
    println!("  общий бакет   : {:>5} / {}   (channels/videos/thumbnails/analytics)",
        st.general, a.general_units_per_day);
    println!();
    println!("Это локальный учёт. Реальные цифры Google показывает только в консоли:");
    println!("  console.cloud.google.com -> APIs & Services -> YouTube Data API v3 -> Quotas");
    println!();
    println!("Сброс — 00:00 по Pacific Time.");
    Ok(())
}
