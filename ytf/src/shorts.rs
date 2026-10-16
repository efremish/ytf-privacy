//! Шортсы: заливка готовых вертикальных видео из `Shorts/<канал>/`.
//!
//! Рендера нет — видео уже сделаны снаружи (генератор, редактор).
//!
//! Структура:
//!   Shorts/
//!   └── <канал>/
//!       ├── _config.json   необязательно: описание, теги, режим
//!       ├── video1.mp4     ← сюда складывать готовые шортсы
//!       └── _uploaded/     сюда программа перемещает залитое
//!
//! Токены — общие, из `tokens.json` (ключ = имя папки канала).
//! Заголовок — имя файла без расширения.
//!
//! По умолчанию за один запуск берётся ОДНО видео на канал и ставится
//! отложенная публикация на ближайший свободный день (как основной
//! пайплайн). Так 100 видео не вываливаются разом на один канал, а
//! раскладываются по дням. Ключ `per_run` в `_config.json` меняет
//! количество, `schedule: false` отключает отложку (заливка сразу).

use anyhow::Context as _;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::report::{History, HistoryEntry};
use crate::settings::Context;
use crate::tokens::{ChannelToken, TokenStore};
use crate::youtube::auth::{self, TokenManager};
use crate::youtube::{api, schedule, upload};

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
    /// "public" | "private" | "unlisted" (когда отложка выключена)
    #[serde(default)]
    pub privacy: Option<String>,
    #[serde(default)]
    pub made_for_kids: bool,
    /// Сколько видео брать из очереди канала за один запуск. None = 1
    #[serde(default)]
    pub per_run: Option<usize>,
    /// Ставить отложенную публикацию (private + publishAt). None = true
    #[serde(default)]
    pub schedule: Option<bool>,
    /// Время публикации "HH:MM". None = из settings.json (22:00)
    #[serde(default)]
    pub publish_at: Option<String>,
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

/// Следующие `count` свободных дат начиная с сегодня. Занятые слоты
/// учитываем по дате+времени: если в этот день уже есть публикация в
/// другое время — день свободен. Выбранные слоты добавляем в `occupied`,
/// чтобы в одном запуске не выдать две публикации на одно время.
fn free_days(
    occupied: &mut Vec<String>,
    from: DateTime<Utc>,
    count: usize,
    horizon_days: i64,
    hour: u32,
    minute: u32,
    offset: i64,
) -> Vec<NaiveDate> {
    let today = from.date_naive();
    let cap = (horizon_days.max(1) + count as i64 + 60).min(400);
    let mut out = Vec::new();
    let mut i = 0i64;
    while out.len() < count && i <= cap {
        let d = today + Duration::days(i);
        if schedule::slot_free(occupied, d, hour, minute, offset) {
            if let Some(k) = schedule::slot_key(&schedule::publish_at(d, hour, minute, offset)) {
                occupied.push(k);
            }
            out.push(d);
        }
        i += 1;
    }
    out
}

/// Залить шортсы из папок. `only` — конкретный канал, None = все.
/// `no_upload` — только показать план. `all` — брать всю очередь
/// (иначе `per_run`, по умолчанию 1 видео на канал за запуск).
pub async fn publish(
    ctx: &Context,
    only: Option<&str>,
    no_upload: bool,
    all: bool,
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
    let now = Utc::now();
    let offset = schedule::tz_offset(ctx, None);

    let mut uploaded = 0usize;
    let mut failed = 0usize;

    for ch in &chans {
        if ch.pending.is_empty() {
            println!("{} — очередь пуста", ch.folder);
            continue;
        }
        let take = if all {
            ch.pending.len()
        } else {
            ch.cfg.per_run.unwrap_or(1).max(1).min(ch.pending.len())
        };
        let schedule_on = ch.cfg.schedule.unwrap_or(true);
        println!(
            "{} — в очереди {}, беру {}, отложка: {}, токен: {}",
            ch.folder,
            ch.pending.len(),
            take,
            if schedule_on { "да" } else { "нет (сразу)" },
            if ch.has_token { "есть" } else { "НЕТ" }
        );

        let (hour, minute) = match &ch.cfg.publish_at {
            Some(s) => schedule::parse_hhmm(s),
            None => (ctx.settings.publish.hour, ctx.settings.publish.minute),
        };

        // в режиме проверки показываем план и без токена
        if no_upload {
            for (k, f) in ch.pending.iter().take(take).enumerate() {
                let mb = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0) / 1_048_576;
                if schedule_on {
                    let d = now.date_naive() + Duration::days(k as i64);
                    println!(
                        "  [проверка] {} ({} МБ) → слот {d} {:02}:{:02}",
                        title_of(f), mb, hour, minute
                    );
                } else {
                    println!("  [проверка] {} ({} МБ) → сразу", title_of(f), mb);
                }
            }
            continue;
        }

        let Some(tok) = store.get(&ch.folder) else {
            tracing::error!("shorts[{}]: нет токена в tokens.json", ch.folder);
            println!("  [!] нет токена — запусти: ytf auth \"{}\"", ch.folder);
            failed += take;
            continue;
        };

        // одна сессия на канал
        let mut tm = TokenManager::new(tok);
        let session = match tm.session().await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("shorts[{}]: сессия не получена: {e:#}", ch.folder);
                println!("  [FAIL] токен: {e:#}");
                failed += take;
                continue;
            }
        };

        // занятые слоты на канале (и обычные видео, и уже отложенные шортсы)
        let mut occupied: Vec<String> = Vec::new();
        if schedule_on {
            match auth::channel_info(&session).await {
                Ok(info) => match schedule::occupied_slots(&session, &info.uploads_playlist).await {
                    Ok(occ) => occupied = occ,
                    Err(e) => tracing::warn!("shorts[{}]: не прочитал занятые слоты: {e}", ch.folder),
                },
                Err(e) => tracing::warn!("shorts[{}]: не получил info канала: {e}", ch.folder),
            }
        }
        let days = free_days(&mut occupied, now, take, ctx.settings.publish.horizon_days, hour, minute, offset);

        for (idx, file) in ch.pending.iter().take(take).enumerate() {
            let (publish_at, when) = if schedule_on {
                match days.get(idx) {
                    Some(d) => (
                        Some(schedule::publish_at(*d, hour, minute, offset)),
                        schedule::publish_at(*d, hour, minute, 0),
                    ),
                    None => (None, "сразу".to_string()),
                }
            } else {
                (None, "сразу".to_string())
            };

            match upload_one(ctx, ch, &session, file, publish_at, schedule_on, &mut quota).await {
                Ok(id) => {
                    uploaded += 1;
                    move_uploaded(file);
                    let url = format!("https://youtube.com/watch?v={id}");
                    println!("  ✓ {} → {url}  ({when})", title_of(file));
                    tracing::info!("shorts[{}]: {} залит ({id}), слот {when}", ch.folder, title_of(file));
                    history.push(HistoryEntry {
                        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                        person: "shorts".into(),
                        channel: ch.folder.clone(),
                        video_id: id.clone(),
                        url,
                        title: title_of(file),
                        track: file.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                        picture: String::new(),
                        publish_at: when,
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
                        publish_at: when,
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

#[allow(clippy::too_many_arguments)]
async fn upload_one(
    ctx: &Context,
    ch: &ShortsChannel,
    session: &auth::Session,
    file: &Path,
    publish_at: Option<String>,
    schedule_on: bool,
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

    // отложка требует private + publishAt
    let privacy = if schedule_on {
        "private".to_string()
    } else {
        ch.cfg.privacy.clone().unwrap_or_else(|| "public".into())
    };

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
            privacy_status: privacy,
            made_for_kids: ch.cfg.made_for_kids,
            publish_at,
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
