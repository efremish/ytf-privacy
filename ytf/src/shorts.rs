//! Шортсы: заливка готовых вертикальных видео из `Shorts/<канал>/`.
//!
//! Отличия от основного пайплайна: рендера нет — видео уже сделаны
//! где-то снаружи (генератор, редактор), мы только заливаем.
//!
//! Структура:
//!   Shorts/
//!   └── <канал>/
//!       ├── _config.json   необязательно: описание, теги, категория
//!       ├── video1.mp4     ← сюда складывать готовые шортсы
//!       └── _uploaded/     сюда программа перемещает залитое
//!
//! Токены — общие, из `tokens.json` (ключ = имя папки канала,
//! как и для People/). Заголовок — имя файла без расширения.

use anyhow::Context as _;
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::report::{History, HistoryEntry};
use crate::settings::Context;
use crate::tokens::{ChannelToken, TokenStore};
use crate::youtube::auth::{self, TokenManager};
use crate::youtube::{api, upload};

const VIDEO_EXT: [&str; 6] = ["mp4", "mov", "webm", "mkv", "m4v", "avi"];

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ShortsConfig {
    /// Описание видео (одно на папку). None = пусто
    #[serde(default)]
    pub description: Option<String>,
    /// Теги
    #[serde(default)]
    pub tags: Vec<String>,
    /// Категория YouTube, по умолчанию 22 (People & Blogs)
    #[serde(default)]
    pub category_id: Option<String>,
    /// "public" | "private" | "unlisted", по умолчанию public
    #[serde(default)]
    pub privacy: Option<String>,
    /// made for kids
    #[serde(default)]
    pub made_for_kids: bool,
}

#[derive(Debug)]
pub struct ShortsChannel {
    pub folder: String,
    pub dir: PathBuf,
    pub pending: Vec<PathBuf>,
    pub cfg: ShortsConfig,
    pub has_token: bool,
}

/// Найти все каналы шортсов и что в них лежит
pub fn scan(ctx: &Context) -> Vec<ShortsChannel> {
    let root = ctx.root.join("Shorts");
    let store = TokenStore::load(ctx).unwrap_or_default();
    let Ok(rd) = std::fs::read_dir(&root) else {
        return Vec::new();
    };

    let mut out: Vec<ShortsChannel> = Vec::new();
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    for dir in dirs {
        let folder = dir.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if folder.starts_with('_') || folder.starts_with('.') {
            continue;
        }
        let cfg = load_cfg(&dir);
        let pending = list_videos(&dir);
        out.push(ShortsChannel {
            has_token: store.get(&folder).is_some(),
            folder,
            dir,
            pending,
            cfg,
        });
    }
    out
}

fn load_cfg(dir: &Path) -> ShortsConfig {
    match std::fs::read_to_string(dir.join("_config.json")) {
        Ok(s) => {
            // Блокнот/PowerShell пишут UTF-8 с BOM — serde_json его не ест
            let s = s.trim_start_matches('\u{feff}');
            match serde_json::from_str(s) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        "shorts[{}]: _config.json не распарсился ({e}) — использую значения по умолчанию",
                        dir.display()
                    );
                    ShortsConfig::default()
                }
            }
        }
        Err(_) => ShortsConfig::default(),
    }
}

fn list_videos(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| VIDEO_EXT.contains(&e.to_ascii_lowercase().as_str()))
                    .unwrap_or(false)
        })
        .collect();
    // по имени — стабильный порядок
    v.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    v
}

fn title_of(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Ширина/высота через ffprobe — для диагностики «это вообще шортс?».
/// None = не прочиталось (не критично).
fn probe_wh(ctx: &Context, path: &Path) -> Option<(u64, u64)> {
    let out = std::process::Command::new(&ctx.ffprobe)
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=width,height", "-of", "csv=p=0"])
        .arg(path)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut it = s.trim().split(',');
    let w = it.next()?.trim().parse().ok()?;
    let h = it.next()?.trim().parse().ok()?;
    Some((w, h))
}

/// Залить все видео из папок. `only` — конкретный канал, None = все.
/// `no_upload` — только показать план.
pub async fn publish(
    ctx: &Context,
    only: Option<&str>,
    no_upload: bool,
) -> anyhow::Result<()> {
    let mut chans = scan(ctx);
    if let Some(o) = only {
        chans.retain(|c| c.folder.eq_ignore_ascii_case(o));
    }
    if chans.is_empty() {
        println!("Папок Shorts/ нет или они пусты.");
        println!("Создай Shorts/<канал>/ и положи туда .mp4");
        return Ok(());
    }

    let store = TokenStore::load(ctx)?;
    let mut history = History::load(&ctx.history_file());
    let mut quota = crate::youtube::quota::QuotaState::load(&ctx.state_file());

    let mut uploaded = 0usize;
    let mut failed = 0usize;

    for ch in &chans {
        if ch.pending.is_empty() {
            println!("{} — нет видео", ch.folder);
            continue;
        }
        println!(
            "{} — видео: {}, токен: {}",
            ch.folder,
            ch.pending.len(),
            if ch.has_token { "есть" } else { "НЕТ" }
        );

        let Some(tok) = store.get(&ch.folder) else {
            tracing::error!("shorts[{}]: нет токена в tokens.json", ch.folder);
            println!("  [!] нет токена — запусти: ytf auth \"{}\"", ch.folder);
            failed += ch.pending.len();
            continue;
        };
        if no_upload {
            for f in &ch.pending {
                println!("  [проверка] {} ({} MB)", title_of(f),
                    (std::fs::metadata(f).map(|m| m.len()).unwrap_or(0)) / 1_048_576);
            }
            continue;
        }

        // одна сессия на канал
        let mut tm = TokenManager::new(tok);
        let session = match tm.session().await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("shorts[{}]: сессия не получена: {e:#}", ch.folder);
                println!("  [FAIL] токен: {e:#}");
                failed += ch.pending.len();
                continue;
            }
        };

        for file in &ch.pending {
            match upload_one(ctx, ch, tok, &session, file, &mut quota).await {
                Ok(id) => {
                    uploaded += 1;
                    move_uploaded(file);
                    let url = format!("https://youtube.com/watch?v={id}");
                    println!("  ✓ {} → {url}", title_of(file));
                    tracing::info!("shorts[{}]: {} залит ({id})", ch.folder, title_of(file));
                    history.push(HistoryEntry {
                        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                        person: "shorts".into(),
                        channel: ch.folder.clone(),
                        video_id: id.clone(),
                        url,
                        title: title_of(file),
                        track: file.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                        picture: String::new(),
                        publish_at: "сразу".into(),
                        mode: "Shorts".into(),
                        size_mb: std::fs::metadata(file)
                            .map(|m| m.len() as f64 / 1_048_576.0)
                            .unwrap_or(0.0),
                        render_secs: 0.0,
                        upload_secs: 0.0,
                        thumbnail_fixed: false,
                        error: None,
                    });
                }
                Err(e) => {
                    failed += 1;
                    tracing::error!("shorts[{}]: {} — {e:#}", ch.folder, title_of(file));
                    println!("  ✗ {} — {e:#}", title_of(file));
                    history.push(HistoryEntry {
                        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                        person: "shorts".into(),
                        channel: ch.folder.clone(),
                        video_id: String::new(),
                        url: String::new(),
                        title: title_of(file),
                        track: file.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                        picture: String::new(),
                        publish_at: String::new(),
                        mode: "Shorts".into(),
                        size_mb: 0.0,
                        render_secs: 0.0,
                        upload_secs: 0.0,
                        thumbnail_fixed: false,
                        error: Some(format!("{e:#}")),
                    });
                }
            }
            let _ = history.save(&ctx.history_file());
        }
        let _ = quota.save(&ctx.state_file());
    }

    println!("\nЗалито: {uploaded}, ошибок: {failed}");
    if no_upload {
        println!("(режим проверки — ничего не загружалось)");
    }
    Ok(())
}

async fn upload_one(
    ctx: &Context,
    ch: &ShortsChannel,
    _tok: &ChannelToken,
    session: &auth::Session,
    file: &Path,
    quota: &mut crate::youtube::quota::QuotaState,
) -> anyhow::Result<String> {
    // диагностика: вертикальное ли видео (шортс = 9:16)
    if let Some((w, h)) = probe_wh(ctx, file) {
        if w >= h {
            tracing::warn!(
                "shorts[{}]: {} — горизонтальное {w}x{h}, YouTube может не показать как шортс",
                ch.folder, title_of(file)
            );
        }
    }

    let a = &ctx.settings.api;
    quota
        .charge("videos.insert", a.insert_bucket_per_day, a.general_units_per_day, a.quota_warn_percent)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let title = title_of(file);
    let body = api::InsertBody {
        snippet: api::SnippetBody {
            title,
            description: ch.cfg.description.clone().unwrap_or_default(),
            category_id: ch
                .cfg
                .category_id
                .clone()
                .unwrap_or_else(|| "22".into()),
            tags: ch.cfg.tags.clone(),
        },
        status: api::StatusBody {
            privacy_status: ch.cfg.privacy.clone().unwrap_or_else(|| "public".into()),
            made_for_kids: ch.cfg.made_for_kids,
            publish_at: None,
        },
    };

    let total = std::fs::metadata(file)
        .with_context(|| format!("нет файла {}", file.display()))?
        .len();

    let id = upload::upload_file(
        session,
        file,
        &body,
        ctx.settings.publish.upload_retries,
        ctx.settings.publish.retry_base_delay_secs,
        |pct| {
            if pct % 25 == 0 {
                tracing::debug!("shorts[{}] {} {pct}%", ch.folder, title_of(file));
            }
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    tracing::info!(
        "shorts[{}]: {} залит ({} МБ), жду обработку YouTube…",
        ch.folder, title_of(file),
        total / 1_048_576
    );

    if !id.is_empty() {
        api::wait_processed(session, &id, 20, 15)
            .await
            .with_context(|| "YouTube не обработал файл")?;
    } else {
        // пустой id — нечем проверять, но и не придумываем
        tracing::warn!("shorts[{}]: пустой id после загрузки", ch.folder);
    }
    Ok(id)
}

fn move_uploaded(file: &Path) {
    let Some(dir) = file.parent() else { return };
    let dst = dir.join("_uploaded");
    let name = file.file_name().unwrap_or_default();
    let mut target = dst.join(name);
    if target.exists() {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        target = dst.join(format!("{stamp}-{}", name.to_string_lossy()));
    }
    let _ = std::fs::create_dir_all(&dst);
    let _ = std::fs::rename(file, &target);
}
