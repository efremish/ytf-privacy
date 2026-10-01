use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub at: String,
    pub person: String,
    pub channel: String,
    pub video_id: String,
    pub url: String,
    pub title: String,
    pub track: String,
    pub picture: String,
    pub publish_at: String,
    pub mode: String,
    pub size_mb: f64,
    pub render_secs: f64,
    pub upload_secs: f64,
    pub thumbnail_fixed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub entries: Vec<HistoryEntry>,
}

impl History {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn push(&mut self, e: HistoryEntry) {
        self.entries.push(e);
        // не даём журналу расти бесконечно
        if self.entries.len() > 2000 {
            self.entries.drain(..500);
        }
    }

    pub fn ok_count(&self) -> usize {
        self.entries.iter().filter(|e| e.error.is_none()).count()
    }

    pub fn fail_count(&self) -> usize {
        self.entries.iter().filter(|e| e.error.is_some()).count()
    }
}

/// Сводка за сегодня — то, что Матвей просил «контролировать»
pub fn summary(h: &History) -> String {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let t: Vec<_> = h.entries.iter().filter(|e| e.at.starts_with(&today)).collect();

    if t.is_empty() {
        return format!("Сегодня ({today}) ничего не выложено.");
    }
    let ok = t.iter().filter(|e| e.error.is_none()).count();
    let fail = t.len() - ok;
    let thumbs = t.iter().filter(|e| e.thumbnail_fixed).count();
    let mb: f64 = t.iter().map(|e| e.size_mb).sum();
    let secs: f64 = t.iter().map(|e| e.render_secs + e.upload_secs).sum();

    let mut s = format!(
        "Сегодня ({}): выложено {ok}, ошибок {fail}, обложку поправил у {thumbs}\n",
        today
    );
    s.push_str(&format!(
        "Объём: {mb:.0} МБ, общее время {:.1} мин\n",
        secs / 60.0
    ));
    s.push('\n');
    for e in t {
        match &e.error {
            Some(err) => s.push_str(&format!("  ✗ {} / {} — {err}\n", e.person, e.channel)),
            None => s.push_str(&format!(
                "  ✓ {:<18} {:<28} {} ({})\n",
                e.channel,
                truncate(&e.title, 28),
                e.publish_at.get(0..16).unwrap_or(""),
                e.url
            )),
        }
    }
    s
}

pub fn write_html(h: &History, dst: &Path) -> Result<()> {
    let mut rows = String::new();
    for e in h.entries.iter().rev().take(500) {
        let status = if e.error.is_some() {
            format!("<span class=err>ошибка</span>")
        } else {
            "<span class=ok>ок</span>".to_string()
        };
        let thumb = if e.thumbnail_fixed { "🖼" } else { "" };
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
             <td>{:.1} МБ</td><td>{}</td><td>{}</td></tr>",
            esc(&e.at),
            esc(&e.person),
            esc(&e.channel),
            if e.url.is_empty() { format!("{status} {thumb}") } else {
                format!("<a href=\"{}\" target=_blank>открыть</a> {status} {thumb}", esc(&e.url))
            },
            esc(&e.title),
            e.size_mb,
            e.render_secs,
            esc(e.error.as_deref().unwrap_or(""))
        ));
    }
    let html = format!(
        r#"<!doctype html><meta charset=utf-8><title>YTF — журнал</title>
<style>
body{{font:14px/1.5 system-ui,sans-serif;margin:24px;background:#0f1115;color:#e6e6e6}}
h1{{font-size:20px}} table{{border-collapse:collapse;width:100%;margin-top:16px}}
th,td{{padding:7px 10px;border-bottom:1px solid #262b33;text-align:left;font-size:13px}}
th{{color:#8b949e;font-weight:600}} .ok{{color:#3fb950}} .err{{color:#f85149}}
a{{color:#58a6ff}}
</style>
<h1>Журнал публикаций — всего {total}, успешно {okn}, ошибок {failn}</h1>
<table><tr><th>Когда</th><th>Человек</th><th>Канал</th><th>Ссылка</th><th>Заголовок</th><th>Размер</th><th>Рендер, с</th><th>Ошибка</th></tr>
{rows}</table>"#,
        total = h.entries.len(),
        okn = h.ok_count(),
        failn = h.fail_count(),
        rows = rows
    );
    std::fs::write(dst, html)?;
    Ok(())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}
