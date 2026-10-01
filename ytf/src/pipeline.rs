use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::config::{Channel, RenderMode};
use crate::library;
use crate::media::{Renderer, Rendered, Visual};
use crate::metadata;
use crate::plan::UsedRegistry;
use crate::report::{History, HistoryEntry};
use crate::settings::Context;
use crate::tokens::{ChannelToken, TokenStore};
use crate::youtube::auth::{self, TokenManager};
use crate::youtube::{api, schedule, upload};

pub struct Job {
    pub ch: Channel,
    pub token: ChannelToken,
}

/// Собрать задачи: каналы на диске, у которых есть токен
pub fn build_jobs(ctx: &Context, only: Option<&str>, dry_run: bool) -> Result<Vec<Job>> {
    let store = TokenStore::load(ctx)?;
    let channels = library::scan_all(ctx)?;

    let mut jobs = Vec::new();
    for ch in channels {
        if !ch.config.enabled {
            tracing::info!("канал {} выключен в _config.json", ch.folder);
            continue;
        }
        if let Some(o) = only {
            if !ch.folder.eq_ignore_ascii_case(o) {
                continue;
            }
        }
        let Some(token) = store.get(&ch.folder) else {
            if dry_run {
                // в режиме проверки токен не нужен
                jobs.push(Job { ch, token: ChannelToken {
                    title: String::new(), client_id: String::new(),
                    client_secret: String::new(), refresh_token: String::new(),
                    scopes: Vec::new(),
                }});
                continue;
            }
            tracing::warn!("нет токена для канала «{}» — пропускаю", ch.folder);
            continue;
        };
        if !ch.is_ready() {
            tracing::warn!("канал «{}» не готов (нет {} ) — пропускаю",
                ch.folder,
                if ch.music.is_empty() { "треков" } else { "картинок/видео" });
            continue;
        }
        jobs.push(Job { ch, token: token.clone() });
    }
    Ok(jobs)
}

fn worker_count(ctx: &Context, override_: Option<usize>) -> usize {
    override_
        .or(ctx.settings.publish.workers)
        .unwrap_or_else(|| {
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
            (cores / 2).clamp(1, 8)
        })
}

/// Главная точка входа
pub async fn publish(
    ctx: &Context,
    only: Option<&str>,
    randomize: bool,
    horizon: i64,
    workers: Option<usize>,
    no_upload: bool,
) -> Result<()> {
    let jobs = build_jobs(ctx, only, no_upload)?;
    if jobs.is_empty() {
        println!("Нет готовых каналов к публикации.");
        return Ok(());
    }

    let n = worker_count(ctx, workers);
    let horizon = horizon.max(ctx.settings.publish.horizon_days.max(1));

    println!("Каналов: {}, воркеров: {}", jobs.len(), n);
    println!("Публикация в: {:02}:{:02} (Omsk), горизонт {} дн.\n", ctx.settings.publish.hour, ctx.settings.publish.minute, horizon);

    let renderer = Renderer::new(ctx)?;
    let registry = Arc::new(tokio::sync::Mutex::new(UsedRegistry::load(&ctx.used_file())));
    let history = Arc::new(tokio::sync::Mutex::new(History::load(&ctx.history_file())));
    let quota = Arc::new(tokio::sync::Mutex::new(
        crate::youtube::quota::QuotaState::load(&ctx.state_file()),
    ));

    let mut set = tokio::task::JoinSet::new();
    let sem = Arc::new(tokio::sync::Semaphore::new(n));

    for (i, job) in jobs.into_iter().enumerate() {
        let ctx = ctx.clone();
        let renderer = renderer.clone();
        let registry = registry.clone();
        let history = history.clone();
        let quota = quota.clone();
        let sem = sem.clone();
        set.spawn(async move {
            let Ok(_permit) = sem.acquire_owned().await else {
                return (i, Err(anyhow::anyhow!("не дождался воркера")));
            };
            let r = run_one(&ctx, &renderer, &job, i, randomize, horizon, no_upload, registry, history, quota).await;
            (i, r)
        });
    }

    let mut done = 0usize;
    while let Some(res) = set.join_next().await {
        done += 1;
        match res {
            Ok((_, Ok(line))) => println!("  [{done}] {line}"),
            Ok((_, Err(e))) => println!("  [{done}] ошибка: {e:#}"),
            Err(e) => println!("  [{done}] паника в задаче: {e}"),
        }
    }

    // сохранить состояние
    registry.lock().await.save(&ctx.used_file())?;
    let h = history.lock().await;
    h.save(&ctx.history_file())?;
    crate::youtube::quota::QuotaState::load(&ctx.state_file());
    drop(h);
    let _ = quota.lock().await.save(&ctx.state_file());

    println!("\n{}", crate::report::summary(&History::load(&ctx.history_file())));
    Ok(())
}

/// Полный цикл по одному каналу
#[allow(clippy::too_many_arguments)]
async fn run_one(
    ctx: &Context,
    renderer: &Renderer,
    job: &Job,
    idx: usize,
    randomize: bool,
    horizon: i64,
    no_upload: bool,
    registry: Arc<tokio::sync::Mutex<UsedRegistry>>,
    history: Arc<tokio::sync::Mutex<History>>,
    quota: Arc<tokio::sync::Mutex<crate::youtube::quota::QuotaState>>,
) -> Result<String> {
    let t0 = Instant::now();
    let ch = &job.ch;

    // ── 1. канал на YouTube ──
    // в режиме --no-upload токен не нужен: Google не трогаем вообще
    let mut session: Option<auth::Session> = None;
    let mut channel_title = ch.label();
    let mut occupied: Vec<String> = Vec::new();
    if !no_upload {
        let mut tm = TokenManager::new(&job.token);
        let s = tm.session().await?;
        let info = auth::channel_info(&s).await?;
        channel_title = info.title;
        occupied = schedule::occupied(&s, &info.uploads_playlist).await?;
        session = Some(s);
    }

    // ── 2. квота (в проверке не списываем) ──
    if !no_upload {
        let mut q = quota.lock().await;
        let a = &ctx.settings.api;
        if let Err(e) = q.charge("videos.insert", a.insert_bucket_per_day, a.general_units_per_day, a.quota_warn_percent) {
            anyhow::bail!("{e}");
        }
    }

    // ── 3. занятые даты ──
    let now = chrono::Utc::now();
    let day = schedule::next_free_day(&occupied, now, horizon);
    let (hour, minute) = match &ch.config.publish_at {
        Some(s) => schedule::parse_hhmm(s),
        None => (ctx.settings.publish.hour, ctx.settings.publish.minute),
    };
    let offset = schedule::tz_offset(ctx, None);
    let publish_at = schedule::publish_at(day, hour, minute, offset);
    let published_local = schedule::publish_at(day, hour, minute, 0);

    tracing::info!("{}: канал «{}», публикация {}", ch.folder, channel_title, published_local);
    println!("{} → «{}» | слот {}", ch.folder, channel_title, &published_local[..16]);

    // ── 4. трек и визуал ──
    let seed = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        use std::hash::{Hash, Hasher};
        (ch.folder.clone() + &day.to_string()).hash(&mut h);
        h.finish()
    };
    let (track, picture) = {
        let mut r = registry.lock().await;
        let videos = &ch.videos;
        let pics: Vec<PathBuf> = match ch.config.mode {
            RenderMode::Loop => videos.clone(),
            _ => ch.pictures.clone(),
        };
        let got = r.pick(&ch.music, &pics, seed);
        match got {
            Some((t, p)) => {
                r.note(&t, p.as_deref());
                (t, p)
            }
            None => anyhow::bail!("нечего рендерить: нет треков"),
        }
    };

    // ── 5. метаданные ──
    let meta = metadata::build(ch, &track, randomize && !ch.config.no_randomize)?;

    // ── 6. визуал для рендера ──
    let visual = match (&picture, ch.config.mode) {
        (Some(p), RenderMode::Loop) => {
            let pal = if renderer.cfg().seamless_loop {
                renderer.palindrome(p).await?
            } else {
                p.clone()
            };
            Visual::Looped(pal)
        }
        (Some(p), _) => Visual::Still(p.clone()),
        (None, RenderMode::Loop) => anyhow::bail!("нет видео для зацикливания"),
        (None, _) => Visual::Still(PathBuf::new()),
    };

    // ── 7. обложка: готовим заранее, из исходника (не из рендера) ──
    let thumb_jpeg = if ctx.settings.publish.fix_black_thumbnail {
        make_thumbnail(renderer, &visual).await.ok()
    } else {
        None
    };

    // ── 8. рендер ──
    let t_render = Instant::now();
    // в режиме --no-upload всегда пишем в файл, чтобы можно было посмотреть результат
    let out_path = ctx.work_dir().join(format!("{}_{}.mp4", idx, sanitize(&ch.folder)));
    let rendered: Rendered = if ctx.settings.publish.stream_to_upload && !no_upload {
        renderer.render_to_memory(&track, &visual).await?
    } else {
        renderer.render_to_file(&track, &visual, &out_path).await?;
        Rendered::File(out_path.clone())
    };
    let render_secs = t_render.elapsed().as_secs_f64();

    let size_mb = match &rendered {
        Rendered::File(p) => std::fs::metadata(p).map(|m| m.len() as f64 / 1_048_576.0).unwrap_or(0.0),
        Rendered::Memory(d) => d.len() as f64 / 1_048_576.0,
        Rendered::Stream(_) => 0.0,
    };

    // ── 8b. режим проверки: рендер есть, загрузки нет ──
    if no_upload {
        println!(
            "  [проверка] {} — {:.0}с, {:.1} МБ, {}",
            ch.folder, render_secs, size_mb,
            out_path.file_name().unwrap_or_default().to_string_lossy()
        );
        println!("           заголовок: {}", meta.title);
        println!("           трек     : {}", track.file_name().unwrap_or_default().to_string_lossy());
        println!("           тегов    : {}", meta.tags.len());
        if let Some(j) = &thumb_jpeg {
            println!("           обложка   : {:.0} КБ, чёрная={}",
                j.len() as f64 / 1024.0,
                Renderer::frame_is_black(j,
                    ctx.settings.render.black_frame_threshold,
                    ctx.settings.render.black_frame_flatness));
        }
        return Ok(format!("{} — рендер {:.0}с, {:.1} МБ (загрузка пропущена)",
            ch.label(), render_secs, size_mb));
    }

    // ── 9. загрузка ──
    let body = api::InsertBody {
        snippet: api::SnippetBody {
            title: meta.title.clone(),
            description: meta.description.clone(),
            category_id: ch.config.category_id.clone(),
            tags: meta.tags.clone(),
        },
        status: api::StatusBody {
            privacy_status: "private".into(),
            made_for_kids: ch.config.made_for_kids,
            publish_at: Some(publish_at.clone()),
        },
    };

    let p = &ctx.settings.publish;
    let t_upload = Instant::now();
    let session = session.expect("в этом ветке сессия уже есть");
    let video_id = upload::upload_rendered(
        &session,
        rendered,
        &body,
        p.upload_retries,
        p.retry_base_delay_secs,
        |n| {
            let pct = if size_mb > 0.0 { (n as f64 / (size_mb * 1_048_576.0) * 100.0) as u8 } else { 0 };
            if pct % 25 == 0 && pct > 0 {
                tracing::debug!("загрузка {pct}%");
            }
        },
    )
    .await?;
    let upload_secs = t_upload.elapsed().as_secs_f64();

    // ── 9b. самопроверка: YouTube принял и обработал файл? ──
    // Битые/обрезанные загрузки дают «Processing abandoned» — ловим это сами.
    if !video_id.is_empty() {
        api::wait_processed(&session, &video_id, 20, 15).await?;
    }

    // ── 10. чёрная обложка? ──
    let thumb_fixed = match thumb_jpeg {
        Some(jpg) if ctx.settings.publish.fix_black_thumbnail => {
            fix_thumbnail(
                &session,
                &video_id,
                jpg,
                ctx.settings.render.black_frame_threshold,
                ctx.settings.render.black_frame_flatness,
            )
            .await
        }
        _ => false,
    };

    // ── 11. журнал ──
    let url = format!("https://youtube.com/watch?v={video_id}");
    history.lock().await.push(HistoryEntry {
        at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        person: ch.person.clone(),
        channel: ch.label(),
        video_id: video_id.clone(),
        url: url.clone(),
        title: meta.title.clone(),
        track: track.file_name().unwrap_or_default().to_string_lossy().into_owned(),
        picture: picture
            .as_ref()
            .and_then(|p| p.file_name())
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        publish_at: published_local.clone(),
        mode: format!("{:?}", ch.config.mode),
        size_mb,
        render_secs,
        upload_secs,
        thumbnail_fixed: thumb_fixed,
        error: None,
    });

    let tail = if video_id.is_empty() {
        String::new()
    } else {
        format!(" | {url}")
    };

    Ok(format!(
        "{} за {:.0}с (рендер {:.0}с + загрузка {:.0}с) | {}{tail}",
        ch.label(),
        t0.elapsed().as_secs_f64(),
        render_secs,
        upload_secs,
        if thumb_fixed { " | обложка поправлена" } else { "" },
    ))
}

/// Кадр для обложки из исходника, а не из отрендеренного файла —
/// иначе при стриминге файла уже нет
async fn make_thumbnail(r: &Renderer, visual: &Visual) -> Result<Vec<u8>> {
    let c = r.cfg();
    let geom = if c.crop_to_fill {
        format!(
            "scale={w}:{h}:force_original_aspect_ratio=increase:flags=lanczos,crop={w}:{h}",
            w = c.width, h = c.height
        )
    } else {
        format!(
            "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos,\
             pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black",
            w = c.width, h = c.height
        )
    };

    let out = match visual {
        Visual::Looped(clip) => {
            tokio::process::Command::new(r.ffmpeg_path())
                .args(["-y", "-hide_banner", "-loglevel", "error", "-ss", "0.5", "-i"])
                .arg(clip)
                .args(["-vf", &format!("{geom},format=yuv420p")])
                .args(["-frames:v", "1", "-q:v", "2", "-f", "mjpeg", "pipe:1"])
                .output()
                .await?
        }
        Visual::Still(img) => {
            if img.as_os_str().is_empty() {
                anyhow::bail!("нет картинки для обложки");
            }
            tokio::process::Command::new(r.ffmpeg_path())
                .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
                .arg(img)
                .args(["-vf", &format!("{geom},format=yuv420p")])
                .args(["-frames:v", "1", "-q:v", "2", "-f", "mjpeg", "pipe:1"])
                .output()
                .await?
        }
    };

    if !out.status.success() || out.stdout.is_empty() {
        anyhow::bail!("не собралась обложка: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(out.stdout)
}

/// Проверить обложку YouTube; если чёрная — залить свою
async fn fix_thumbnail(
    session: &auth::Session,
    video_id: &str,
    jpeg: Vec<u8>,
    max_luma: u8,
    max_spread: u8,
) -> bool {
    if video_id.is_empty() {
        return false;
    }
    // YouTube может брать обложку не сразу — пару попыток
    for attempt in 0..4u32 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(4 * attempt as u64)).await;
        }
        let Ok(Some(url)) = api::video_thumb_url(session, video_id).await else { continue };
        let Ok(bytes) = api::fetch_thumb_bytes(&url).await else { continue };

        if !Renderer::frame_is_black(&bytes, max_luma, max_spread) {
            tracing::info!("обложка {video_id} нормальная — оставляю YouTube");
            return false;
        }
        tracing::info!("обложка {video_id} чёрная — заливаю свою");
        match api::set_thumbnail(session, video_id, jpeg.clone()).await {
            Ok(()) => return true,
            Err(e) => {
                tracing::warn!("не смог залить обложку: {e}");
                return false;
            }
        }
    }
    false
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect()
}

/// Диагностика: какие кадры видео система считает чёрными.
/// Именно это решает, будем ли мы перебивать обложку.
pub async fn check_thumb(ctx: &Context, file: &str) -> Result<()> {
    let p = std::path::PathBuf::from(file);
    if !p.exists() {
        anyhow::bail!("нет файла {}", p.display());
    }
    let r = Renderer::new(ctx)?;
    let thr = ctx.settings.render.black_frame_threshold;
    let flat = ctx.settings.render.black_frame_flatness;
    let dur = r.duration(&p).await?;

    println!("Файл    : {}", p.display());
    println!("Длина   : {dur:.1} сек");
    println!("Правило : чёрный = p90 <= {thr} И разброс(p90-p10) <= {flat}\n");

    let mut probes: Vec<(String, f64)> = vec![("первый кадр".into(), 0.05)];
    if ctx.settings.render.fade_from_black_secs > 0.0 {
        let f = ctx.settings.render.fade_from_black_secs;
        probes.push((format!("{:.0}% проявления", f / dur * 100.0), f / 2.0));
        probes.push(("конец проявления".into(), f + 0.5));
    }
    probes.push(("середина".into(), dur / 2.0));

    for (label, t) in probes {
        let tmp = ctx.work_dir().join(format!("chk_{}.jpg", t.round() as i64));
        if let Err(e) = r.extract_frame(&p, Some(t), &tmp).await {
            println!("  {label:<20} ошибка: {e}");
            continue;
        }
        let bytes = std::fs::read(&tmp)?;
        let black = Renderer::frame_is_black(&bytes, thr, flat);
        let pr = crate::media::luma_percentiles(&bytes);
        let (p10, p90) = pr.unwrap_or((0, 0));
        println!(
            "  {label:<20} t={t:>6.1}с  p10={p10:>3} p90={p90:>3} разброс={:>3}  {}",
            p90.saturating_sub(p10),
            if black { "ЧЁРНЫЙ -> обложку надо менять" } else { "норма" }
        );
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(())
}

// ── doctor ────────────────────────────────────────────────

pub async fn doctor(ctx: &Context) -> Result<()> {
    println!("=== YTF: проверка окружения ===\n");
    println!("Корень       : {}", ctx.root.display());
    println!("settings.json: {}", if ctx.settings_file().exists() { "есть" } else { "НЕТ (будут дефолты)" });
    println!("ffmpeg       : {}", ctx.ffmpeg.display());
    println!("ffprobe      : {}", ctx.ffprobe.display());
    println!("tokens.json  : {}", if ctx.tokens_file().exists() { "есть" } else { "НЕТ" });
    println!("People/      : {}", if ctx.people_dir().exists() { "есть" } else { "НЕТ" });

    println!("\n--- рендер ---");
    let r = &ctx.settings.render;
    println!("  {}x{} @ {} fps, crf {}, preset {}", r.width, r.height, r.fps, r.crf, r.preset);
    println!("  выход из темноты: {} сек", r.fade_from_black_secs);
    println!("  обрезка по центру: {}", r.crop_to_fill);
    println!("  порог чёрного кадра: {}", r.black_frame_threshold);

    println!("\n--- публикация ---");
    let p = &ctx.settings.publish;
    println!("  время: {:02}:{:02} (зона {:?})", p.hour, p.minute, p.timezone);
    println!("  горизонт: {} дн.", p.horizon_days);
    println!("  воркеров: {}", worker_count(ctx, None));
    println!("  стрим без диска: {}", p.stream_to_upload);
    println!("  авто-обложка: {}", p.fix_black_thumbnail);
    println!("  удалять после загрузки: {}", p.delete_after_upload);
    println!("  попыток загрузки: {}", p.upload_retries);

    println!("\n--- каналы ---");
    match library::scan_all(ctx) {
        Ok(chs) => {
            let store = TokenStore::load(ctx)?;
            for c in &chs {
                let has_tok = store.get(&c.folder).is_some();
                println!(
                    "  [{}] {} / {} — треков {}, картинок {}, видео {}{}",
                    if c.is_ready() && has_tok { "+" } else { "!" },
                    c.person, c.folder,
                    c.music.len(), c.pictures.len(), c.videos.len(),
                    if has_tok { "" } else { "  <- НЕТ ТОКЕНА" }
                );
            }
        }
        Err(e) => println!("  {e}"),
    }
    Ok(())
}
