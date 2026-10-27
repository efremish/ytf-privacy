//! Шортсы: заливка готовых вертикальных видео.
//!
//! Рендера нет — видео уже сделаны снаружи (генератор, редактор).
//!
//! Два способа складывать:
//!
//! 1. **Общий пул** — `Shorts/_pool/` (или просто файлы в `Shorts/`).
//!    На каждый запуск программа берёт по одному СЛУЧАЙНОМУ видео
//!    на каждый подключённый канал и заливает сразу. Так 100 роликов
//!    в одной папке распределяются по 10 каналам, а не уходят все
//!    на один.
//!
//! 2. **Папка канала** — `Shorts/<канал>/`. Берётся одно видео
//!    (или `per_run`) и уходит на этот канал.
//!
//! По умолчанию шортсы публикуются СРАЗУ (public). Отложка не нужна —
//! это решение заказчика. Если всё же надо по слотам — `schedule: true`
//! и `publish_at` в `_config.json`.
//!
//! Залитое перемещается в `_uploaded/` рядом с исходником.

use anyhow::Context as _;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use rand::seq::SliceRandom;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::report::{History, HistoryEntry};
use crate::settings::Context;
use crate::tokens::TokenStore;
use crate::youtube::auth::{self, TokenManager};
use crate::youtube::{api, schedule, upload};

const VIDEO_EXT: [&str; 6] = ["mp4", "mov", "webm", "mkv", "m4v", "avi"];

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ShortsConfig {
    /// Описание видео (одно на папку/пул). None = пусто
    #[serde(default)]
    pub description: Option<String>,
    /// Теги
    #[serde(default)]
    pub tags: Vec<String>,
    /// Категория YouTube, по умолчанию 22 (People & Blogs)
    #[serde(default)]
    pub category_id: Option<String>,
    /// "public" | "private" | "unlisted". По умолчанию public
    #[serde(default)]
    pub privacy: Option<String>,
    #[serde(default)]
    pub made_for_kids: bool,
    /// Сколько видео брать на канал за один запуск. None = 1
    #[serde(default)]
    pub per_run: Option<usize>,
    /// Ставить отложенную публикацию. None = false (сразу)
    #[serde(default)]
    pub schedule: Option<bool>,
    /// Время публикации "HH:MM" (только если schedule = true)
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

/// Что нашлось на диске
#[derive(Debug)]
pub struct Plan {
    /// Папки каналов `Shorts/<канал>/`
    pub channels: Vec<ShortsChannel>,
    /// Общий пул: `Shorts/_pool/*.mp4` + файлы прямо в `Shorts/`
    pub pool: Vec<PathBuf>,
    pub pool_dir: PathBuf,
    pub pool_cfg: ShortsConfig,
    /// Каналы, между которыми распределяется пул
    pub targets: Vec<String>,
}

/// Найти всё, что лежит в Shorts/
pub fn scan(ctx: &Context) -> Plan {
    let root = ctx.root.join("Shorts");
    let store = TokenStore::load(ctx).unwrap_or_default();
    let pool_dir = root.join("_pool");
    let pool_cfg = load_cfg(&pool_dir);

    let mut channels: Vec<ShortsChannel> = Vec::new();
    let mut pool: Vec<PathBuf> = Vec::new();

    if let Ok(rd) = std::fs::read_dir(&root) {
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
            channels.push(ShortsChannel {
                has_token: store.get(&folder).is_some(),
                folder,
                dir,
                pending,
                cfg,
            });
        }
    }

    // общий пул: файлы прямо в Shorts/ + содержимое _pool/
    pool.extend(list_videos(&root));
    pool.extend(list_videos(&pool_dir));

    Plan {
        channels,
        pool,
        pool_dir,
        pool_cfg,
        targets: load_targets(ctx, &store),
    }
}

/// Кому распределять общий пул.
///
/// 1) `Shorts/_targets.json` — явный список каналов (если файл есть);
/// 2) иначе — ВСЕ подключённые каналы из tokens.json.
///
/// Специально не фильтруем по папкам в People/: пул — это маркетинговые
/// ролики, для них папка канала в People/ не нужна.
fn load_targets(ctx: &Context, store: &TokenStore) -> Vec<String> {
    let all: Vec<String> = store.channels.keys().cloned().collect();

    #[derive(Deserialize)]
    struct Targets {
        channels: Vec<String>,
    }
    let tf = ctx.root.join("Shorts").join("_targets.json");
    if let Ok(s) = std::fs::read_to_string(&tf) {
        let s = s.trim_start_matches('\u{feff}');
        match serde_json::from_str::<Targets>(s) {
            Ok(t) => {
                let v: Vec<String> = t
                    .channels
                    .into_iter()
                    .filter(|c| store.channels.contains_key(c))
                    .collect();
                if !v.is_empty() {
                    return v;
                }
                tracing::warn!("shorts: _targets.json не дал ни одного известного канала — беру все");
            }
            Err(_) => tracing::warn!("shorts: _targets.json не распарсился — беру все каналы"),
        }
    }

    all
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
                        "shorts[{}]: _config.json не распарсился ({e}) — значения по умолчанию",
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
    v.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    v
}

fn title_of(path: &Path) -> String {
    path.file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Ширина/высота через ffprobe — для диагностики «это вообще шортс?»
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

/// Следующие `count` свободных дат. Учитываем дату+время: публикация
/// в другой час того же дня не мешает.
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

/// Главная точка: обработать папки каналов и общий пул.
pub async fn publish(
    ctx: &Context,
    only: Option<&str>,
    no_upload: bool,
    all: bool,
) -> anyhow::Result<()> {
    let plan = scan(ctx);
    if plan.channels.is_empty() && plan.pool.is_empty() {
        println!("Папок Shorts/ нет или они пусты.");
        println!("Положи .mp4 в Shorts/_pool/ (общий пул) или в Shorts/<канал>/");
        return Ok(());
    }

    let store = TokenStore::load(ctx)?;
    let now = Utc::now();
    let offset = schedule::tz_offset(ctx, None);
    let mut sessions: HashMap<String, auth::Session> = HashMap::new();

    let mut uploaded = 0usize;
    let mut failed = 0usize;

    // ── 1. папки конкретных каналов ──
    for ch in &plan.channels {
        if let Some(o) = only {
            if !ch.folder.eq_ignore_ascii_case(o) {
                continue;
            }
        }
        if ch.pending.is_empty() {
            continue;
        }
        // канал занимает только одно окно программы
        let _channel_lock = if no_upload {
            None
        } else {
            match crate::lock::channel_lock(&ctx.root, &ch.folder) {
                Ok(g) => Some(g),
                Err(_) => {
                    println!("  [!] {} — уже публикуется в другом окне, пропускаю", ch.folder);
                    continue;
                }
            }
        };
        let take = if all {
            ch.pending.len()
        } else {
            ch.cfg.per_run.unwrap_or(1).max(1).min(ch.pending.len())
        };
        let schedule_on = ch.cfg.schedule.unwrap_or(false);
        println!(
            "{} — в папке {}, беру {}, {}",
            ch.folder,
            ch.pending.len(),
            take,
            if schedule_on { "с отложкой" } else { "сразу" }
        );

        if no_upload {
            print_plan(&ch.folder, &ch.pending, take, &ch.cfg, now, offset);
            continue;
        }
        let Some(_) = store.get(&ch.folder) else {
            tracing::error!("shorts[{}]: нет токена в tokens.json", ch.folder);
            println!("  [!] нет токена — запусти: ytf auth \"{}\"", ch.folder);
            failed += take;
            continue;
        };

        let session = match session_for(&ch.folder, &store, &mut sessions).await {
            Ok(s) => s,
            Err(e) => {
                println!("  [FAIL] токен: {e:#}");
                failed += take;
                continue;
            }
        };

        let (hour, minute) = hm_of(&ch.cfg, ctx);
        let mut occupied = if schedule_on {
            occupied_slots(&session, &ch.folder).await
        } else {
            Vec::new()
        };
        let days = if schedule_on {
            free_days(&mut occupied, now, take, ctx.settings.publish.horizon_days, hour, minute, offset)
        } else {
            Vec::new()
        };

        for (idx, file) in ch.pending.iter().take(take).enumerate() {
            let (publish_at, when) = slot_for(schedule_on, &days, idx, hour, minute, offset);
            match upload_one(ctx, &ch.folder, &ch.cfg, &session, file, publish_at, schedule_on).await {
                Ok(id) => {
                    uploaded += 1;
                    move_uploaded(file);
                    record_ok(ctx, &ch.folder, file, &id, &when);
                    println!("  ✓ {} → https://youtube.com/watch?v={id}  ({when})", title_of(file));
                }
                Err(e) => {
                    failed += 1;
                    record_err(ctx, &ch.folder, file, &when, &e);
                    println!("  ✗ {} — {e:#}", title_of(file));
                }
            }
        }
    }

    // ── 2. общий пул → распределить по каналам ──
    if !plan.pool.is_empty() {
        let targets: Vec<String> = match only {
            Some(o) => vec![o.to_string()],
            None => plan.targets.clone(),
        };
        let targets: Vec<String> = targets
            .into_iter()
            .filter(|t| store.channels.contains_key(t))
            .collect();

        if targets.is_empty() {
            println!("\nПул: {} видео, но нет каналов с токенами для распределения.", plan.pool.len());
        } else {
            let per = plan.pool_cfg.per_run.unwrap_or(1).max(1);
            let need = if all { plan.pool.len() } else { targets.len() * per };
            let mut queue = plan.pool.clone();
            queue.shuffle(&mut rand::thread_rng());
            queue.truncate(need.min(queue.len()));

            println!(
                "\nПул: {} видео, каналов {}, беру {} (по {} на канал), {}",
                plan.pool.len(),
                targets.len(),
                queue.len(),
                per,
                if plan.pool_cfg.schedule.unwrap_or(false) { "с отложкой" } else { "сразу" }
            );

            if no_upload {
                print_plan("_pool", &queue, queue.len(), &plan.pool_cfg, now, offset);
                println!("  каналы: {}", targets.join(", "));
            } else {
                for (i, file) in queue.iter().enumerate() {
                    let target = &targets[i % targets.len()];
                    let session = match session_for(target, &store, &mut sessions).await {
                        Ok(s) => s,
                        Err(e) => {
                            println!("  ✗ {target}: токен — {e:#}");
                            failed += 1;
                            continue;
                        }
                    };
                    let schedule_on = plan.pool_cfg.schedule.unwrap_or(false);
                    let (hour, minute) = hm_of(&plan.pool_cfg, ctx);
                    // при отложке берём реально свободный день на этом канале
                    let (publish_at, when) = if schedule_on {
                        let mut occ = occupied_slots(&session, target).await;
                        let days = free_days(
                            &mut occ, now, 1, ctx.settings.publish.horizon_days, hour, minute, offset,
                        );
                        match days.first() {
                            Some(d) => (
                                Some(schedule::publish_at(*d, hour, minute, offset)),
                                schedule::publish_at(*d, hour, minute, 0),
                            ),
                            None => (None, "сразу".into()),
                        }
                    } else {
                        (None, "сразу".into())
                    };

                    // канал занимает только одно окно программы
                    let _lock = match crate::lock::channel_lock(&ctx.root, target) {
                        Ok(g) => Some(g),
                        Err(_) => {
                            println!("  [!] {target} — уже публикуется в другом окне, пропускаю");
                            continue;
                        }
                    };
                    match upload_one(ctx, target, &plan.pool_cfg, &session, file, publish_at, schedule_on).await {
                        Ok(id) => {
                            uploaded += 1;
                            move_uploaded(file);
                            record_ok(ctx, target, file, &id, &when);
                            println!("  ✓ {} → {target} → https://youtube.com/watch?v={id}  ({when})", title_of(file));
                        }
                        Err(e) => {
                            failed += 1;
                            record_err(ctx, target, file, &when, &e);
                            println!("  ✗ {} → {target} — {e:#}", title_of(file));
                        }
                    }
                }
            }
        }
    }

    println!("\nЗалито: {uploaded}, ошибок: {failed}");
    if no_upload {
        println!("(режим проверки — ничего не загружалось)");
    }
    Ok(())
}

fn hm_of(cfg: &ShortsConfig, ctx: &Context) -> (u32, u32) {
    match &cfg.publish_at {
        Some(s) => schedule::parse_hhmm(s),
        None => (ctx.settings.publish.hour, ctx.settings.publish.minute),
    }
}

fn slot_for(
    schedule_on: bool,
    days: &[NaiveDate],
    idx: usize,
    hour: u32,
    minute: u32,
    offset: i64,
) -> (Option<String>, String) {
    if !schedule_on {
        return (None, "сразу".into());
    }
    match days.get(idx) {
        Some(d) => (
            Some(schedule::publish_at(*d, hour, minute, offset)),
            schedule::publish_at(*d, hour, minute, 0),
        ),
        None => (None, "сразу".into()),
    }
}

fn print_plan(
    label: &str,
    files: &[PathBuf],
    take: usize,
    cfg: &ShortsConfig,
    now: DateTime<Utc>,
    _offset: i64,
) {
    let schedule_on = cfg.schedule.unwrap_or(false);
    for (k, f) in files.iter().take(take).enumerate() {
        let mb = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0) / 1_048_576;
        if schedule_on {
            let d = now.date_naive() + Duration::days(k as i64);
            println!("  [проверка] {label}: {} ({} МБ) → слот {d}", title_of(f), mb);
        } else {
            println!("  [проверка] {label}: {} ({} МБ) → сразу", title_of(f), mb);
        }
    }
}

async fn session_for(
    folder: &str,
    store: &TokenStore,
    cache: &mut HashMap<String, auth::Session>,
) -> anyhow::Result<auth::Session> {
    if let Some(s) = cache.get(folder) {
        return Ok(s.clone());
    }
    let tok = store
        .get(folder)
        .ok_or_else(|| anyhow::anyhow!("нет токена для «{folder}»"))?;
    let mut tm = TokenManager::new(tok);
    let s = tm.session().await?;
    cache.insert(folder.to_string(), s.clone());
    Ok(s)
}

async fn occupied_slots(session: &auth::Session, folder: &str) -> Vec<String> {
    match auth::channel_info(session).await {
        Ok(info) => match schedule::occupied_slots(session, &info.uploads_playlist).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("shorts[{folder}]: не прочитал занятые слоты: {e}");
                Vec::new()
            }
        },
        Err(e) => {
            tracing::warn!("shorts[{folder}]: не получил info канала: {e}");
            Vec::new()
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn upload_one(
    ctx: &Context,
    folder: &str,
    cfg: &ShortsConfig,
    session: &auth::Session,
    file: &Path,
    publish_at: Option<String>,
    schedule_on: bool,
) -> anyhow::Result<String> {
    if let Some((w, h)) = probe_wh(ctx, file) {
        if w >= h {
            tracing::warn!(
                "shorts[{folder}]: {} — горизонтальное {w}x{h}, YouTube может не показать как шортс",
                title_of(file)
            );
        }
    }

    let a = &ctx.settings.api;
    crate::youtube::quota::QuotaState::charge_shared(
        &ctx.state_file(),
        "videos.insert",
        a.insert_bucket_per_day,
        a.general_units_per_day,
        a.quota_warn_percent,
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    let privacy = if schedule_on {
        "private".to_string()
    } else {
        cfg.privacy.clone().unwrap_or_else(|| "public".into())
    };

    let body = api::InsertBody {
        snippet: api::SnippetBody {
            title: title_of(file),
            description: cfg.description.clone().unwrap_or_default(),
            category_id: cfg.category_id.clone().unwrap_or_else(|| "22".into()),
            tags: cfg.tags.clone(),
        },
        status: api::StatusBody {
            privacy_status: privacy,
            made_for_kids: cfg.made_for_kids,
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
                tracing::debug!("shorts[{folder}] {} {pct}%", title_of(file));
            }
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("{e}"))?;

    tracing::info!(
        "shorts[{folder}]: {} залит ({} МБ), жду обработку YouTube…",
        title_of(file),
        total / 1_048_576
    );

    if !id.is_empty() {
        api::wait_processed(session, &id, 20, 15)
            .await
            .with_context(|| "YouTube не обработал файл")?;
    } else {
        tracing::warn!("shorts[{folder}]: пустой id после загрузки");
    }
    Ok(id)
}

fn record_ok(ctx: &Context, folder: &str, file: &Path, id: &str, when: &str) {
    let entry = HistoryEntry {
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        person: "shorts".into(),
        channel: folder.to_string(),
        video_id: id.to_string(),
        url: format!("https://youtube.com/watch?v={id}"),
        title: title_of(file),
        track: file.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        picture: String::new(),
        publish_at: when.to_string(),
        mode: "Shorts".into(),
        size_mb: std::fs::metadata(file)
            .map(|m| m.len() as f64 / 1_048_576.0)
            .unwrap_or(0.0),
        render_secs: 0.0,
        upload_secs: 0.0,
        thumbnail_fixed: false,
        error: None,
    };
    if let Err(e) = History::append_entry(&ctx.history_file(), entry) {
        tracing::warn!("shorts[{folder}]: журнал не записан — {e:#}");
    }
    tracing::info!("shorts[{folder}]: {} залит ({id}), {when}", title_of(file));
}

fn record_err(ctx: &Context, folder: &str, file: &Path, when: &str, e: &anyhow::Error) {
    let entry = HistoryEntry {
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        person: "shorts".into(),
        channel: folder.to_string(),
        video_id: String::new(),
        url: String::new(),
        title: title_of(file),
        track: file.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        picture: String::new(),
        publish_at: when.to_string(),
        mode: "Shorts".into(),
        size_mb: 0.0,
        render_secs: 0.0,
        upload_secs: 0.0,
        thumbnail_fixed: false,
        error: Some(format!("{e:#}")),
    };
    if let Err(err) = History::append_entry(&ctx.history_file(), entry) {
        tracing::warn!("shorts[{folder}]: журнал не записан — {err:#}");
    }
    tracing::error!("shorts[{folder}]: {} — {e:#}", title_of(file));
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
