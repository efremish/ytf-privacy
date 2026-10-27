use axum::{
    extract::{State, Json},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::config::Settings;
use crate::library;
use crate::pipeline;
use crate::plan::UsedRegistry;
use crate::report::{self, History};
use crate::settings::Context;
use crate::tokens::TokenStore;
use crate::youtube::quota::QuotaState;

pub mod ui;

#[derive(Clone)]
pub struct AppState {
    pub ctx: Arc<Context>,
    pub run: Arc<Mutex<RunInfo>>,
    pub port: u16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunInfo {
    pub running: bool,
    pub log: Vec<String>,
    pub started_at: Option<String>,
    pub current: Option<String>,
    pub randomize: bool,
}

impl RunInfo {
    pub fn push(&mut self, line: impl Into<String>) {
        let l = line.into();
        tracing::info!("web: {l}");
        self.current = Some(l.clone());
        self.log.push(l);
        if self.log.len() > 400 {
            self.log.drain(..100);
        }
    }
}

pub async fn serve(ctx: &Context, host: &str, port: u16) -> anyhow::Result<()> {
    let state = AppState {
        ctx: Arc::new(clone_ctx(ctx)),
        run: Arc::new(Mutex::new(RunInfo::default())),
        port,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/favicon.ico", get(favicon))
        .route("/report.html", get(report_page))
        .route("/api/state", get(api_state))
        .route("/api/publish", post(api_publish))
        .route("/api/shorts", get(api_shorts))
        .route("/api/shorts/publish", post(api_shorts_publish))
        .route("/api/settings", get(api_get_settings).put(api_put_settings))
        .route("/api/history", get(api_history))
        .route("/api/tokens", get(api_tokens))
        .route("/api/used", get(api_used))
        .with_state(state);

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("YTF готов: http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

fn clone_ctx(ctx: &Context) -> Context {
    // Context не Clone — пересобираем из корня, путь тот же
    Context::new(Some(ctx.root.to_string_lossy().into_owned())).expect("корень проекта")
}

async fn index() -> Html<&'static str> {
    Html(ui::PAGE)
}

/// Журнал в HTML — генерируется на месте, если ещё нет
async fn report_page(State(s): State<AppState>) -> impl IntoResponse {
    let h = History::load(&s.ctx.history_file());
    let path = s.ctx.root.join("report.html");
    match report::write_html(&h, &path) {
        Ok(()) => match std::fs::read_to_string(&path) {
            Ok(html) => (StatusCode::OK, Html(html)).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
        },
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// Пустая иконка — иначе браузер сыплет 404 в консоль
async fn favicon() -> impl IntoResponse {
    const ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="7" fill="#0d1117"/><path d="M10 9l12 7-12 7z" fill="#58a6ff"/></svg>"##;
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
        ICON,
    )
}

// ── API ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PublishReq {
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    randomize: bool,
    #[serde(default = "one")]
    horizon: i64,
    #[serde(default)]
    workers: Option<usize>,
}

fn one() -> i64 { 1 }

async fn api_state(State(s): State<AppState>) -> impl IntoResponse {
    let channels = match library::scan_all(&s.ctx) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::OK,
                Json(serde_json::json!({ "error": format!("{e:#}") })),
            )
                .into_response()
        }
    };
    let store = TokenStore::load(&s.ctx).unwrap_or_default();
    let q = QuotaState::load(&s.ctx.state_file());
    let used = UsedRegistry::load(&s.ctx.used_file());
    let run = s.run.lock().await.clone();

    let list: Vec<_> = channels
        .iter()
        .map(|c| {
            let tok = store.get(&c.folder);
            serde_json::json!({
                "person": c.person,
                "folder": c.folder,
                "title": c.label(),
                "mode": format!("{:?}", c.config.mode),
                "enabled": c.config.enabled,
                "music": c.music.len(),
                "pictures": c.pictures.len(),
                "videos": c.videos.len(),
                "has_token": tok.is_some(),
                "has_upload_scope": tok.map(|t| t.has_scope(crate::tokens::SCOPE_UPLOAD)).unwrap_or(false),
                "publish_at": c.config.publish_at,
                "release_track": c.config.single_music,
                "auto_shorts": c.config.auto_shorts,
                "shorts_when": c.config.shorts_when.clone().unwrap_or_else(|| "after".into()),
                "shorts_delay_min": c.config.shorts_delay_min.unwrap_or(60),
                "shorts_trim_secs": c.config.shorts_trim_secs,
                "ready": c.is_ready(),
                "used_music": c.music.iter()
                    .filter(|m| used.music.contains_key(
                        &m.file_name().unwrap_or_default().to_string_lossy().to_lowercase()))
                    .count(),
            })
        })
        .collect();

    Json(serde_json::json!({
        "port": s.port,
        "channels": list,
        "quota": { "inserts": q.inserts, "searches": q.searches, "general": q.general,
                   "insert_limit": s.ctx.settings.api.insert_bucket_per_day,
                   "general_limit": s.ctx.settings.api.general_units_per_day },
        "publish": {
            "hour": s.ctx.settings.publish.hour,
            "minute": s.ctx.settings.publish.minute,
            "timezone": s.ctx.settings.publish.timezone,
            "fade": s.ctx.settings.render.fade_from_black_secs,
            "horizon": s.ctx.settings.publish.horizon_days,
            "stream": s.ctx.settings.publish.stream_to_upload,
            "fix_thumb": s.ctx.settings.publish.fix_black_thumbnail,
        },
        "run": run,
    }))
    .into_response()
}

async fn api_publish(
    State(s): State<AppState>,
    Json(req): Json<PublishReq>,
) -> impl IntoResponse {
    {
        let mut run = s.run.lock().await;
        if run.running {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": "уже идёт публикация" })),
            )
                .into_response();
        }
        *run = RunInfo {
            running: true,
            started_at: Some(chrono::Local::now().format("%H:%M:%S").to_string()),
            randomize: req.randomize,
            ..Default::default()
        };
        run.push(format!(
            "старт: канал={:?} рандом={} горизонт={}",
            req.channel, req.randomize, req.horizon
        ));
    }

    let ctx = s.ctx.clone();
    let state2 = s.clone();
    tokio::spawn(async move {
        let r = pipeline::publish(
            &ctx,
            req.channel.as_deref(),
            req.randomize,
            req.horizon,
            req.workers,
            false,
        )
        .await;
        let mut run = state2.run.lock().await;
        run.running = false;
        match r {
            Ok(()) => run.push(String::from("готово")),
            Err(e) => run.push(format!("ошибка: {e:#}")),
        }
        run.current = None;
    });

    (StatusCode::OK, Json(serde_json::json!({ "started": true }))).into_response()
}

#[derive(Debug, Deserialize)]
struct ShortsReq {
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    all: bool,
}

/// Список папок Shorts/ и общий пул
async fn api_shorts(State(s): State<AppState>) -> impl IntoResponse {
    let plan = crate::shorts::scan(&s.ctx);
    let list: Vec<_> = plan
        .channels
        .into_iter()
        .map(|c| {
            serde_json::json!({
                "folder": c.folder,
                "files": c.pending.iter().map(|p|
                    p.file_name().unwrap_or_default().to_string_lossy()).collect::<Vec<_>>(),
                "count": c.pending.len(),
                "has_token": c.has_token,
                "description": c.cfg.description,
                "tags": c.cfg.tags.len(),
                "per_run": c.cfg.per_run.unwrap_or(1),
                "schedule": c.cfg.schedule.unwrap_or(false),
            })
        })
        .collect();
    Json(serde_json::json!({
        "channels": list,
        "pool": {
            "count": plan.pool.len(),
            "files": plan.pool.iter().take(50).map(|p|
                p.file_name().unwrap_or_default().to_string_lossy()).collect::<Vec<_>>(),
            "targets": plan.targets,
            "per_run": plan.pool_cfg.per_run.unwrap_or(1),
            "schedule": plan.pool_cfg.schedule.unwrap_or(false),
        }
    }))
    .into_response()
}

async fn api_shorts_publish(
    State(s): State<AppState>,
    Json(req): Json<ShortsReq>,
) -> impl IntoResponse {
    {
        let mut run = s.run.lock().await;
        if run.running {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": "уже идёт публикация" })),
            )
                .into_response();
        }
        *run = RunInfo {
            running: true,
            started_at: Some(chrono::Local::now().format("%H:%M:%S").to_string()),
            randomize: false,
            ..Default::default()
        };
        run.push(format!("старт шортсы: канал={:?} всё={}", req.channel, req.all));
    }

    let ctx = s.ctx.clone();
    let state2 = s.clone();
    let all = req.all;
    tokio::spawn(async move {
        let r = crate::shorts::publish(&ctx, req.channel.as_deref(), false, all).await;
        let mut run = state2.run.lock().await;
        run.running = false;
        match r {
            Ok(()) => run.push(String::from("шортсы готово")),
            Err(e) => run.push(format!("ошибка: {e:#}")),
        }
        run.current = None;
    });

    (StatusCode::OK, Json(serde_json::json!({ "started": true }))).into_response()
}

async fn api_get_settings(State(s): State<AppState>) -> impl IntoResponse {
    Json(serde_json::to_value(&s.ctx.settings).unwrap_or_default()).into_response()
}

async fn api_put_settings(
    State(s): State<AppState>,
    Json(new): Json<Settings>,
) -> impl IntoResponse {
    if let Err(e) = new.save(&s.ctx.settings_file()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{e:#}") })),
        )
            .into_response();
    }
    (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
}

async fn api_history(State(s): State<AppState>) -> impl IntoResponse {
    let h = History::load(&s.ctx.history_file());
    let _ = report::write_html(&h, &s.ctx.root.join("report.html"));
    Json(serde_json::json!({
        "summary": report::summary(&h),
        "entries": h.entries.iter().rev().take(200).collect::<Vec<_>>(),
        "report_html": "report.html",
    }))
    .into_response()
}

async fn api_tokens(State(s): State<AppState>) -> impl IntoResponse {
    let store = TokenStore::load(&s.ctx).unwrap_or_default();
    let mut out = Vec::new();
    for (folder, t) in &store.channels {
        let probe = crate::youtube::auth::probe_channel(t).await;
        out.push(serde_json::json!({
            "folder": folder,
            "title": t.title,
            "has_upload_scope": t.has_scope(crate::tokens::SCOPE_UPLOAD),
            "ok": probe.is_ok(),
            "info": probe.as_ref().map(|i| format!("{} ({} видео, {} подписчиков)",
                i.title, i.videos, i.subscribers)).ok(),
            "error": probe.as_ref().err().map(|e| format!("{e:#}")),
        }));
    }
    Json(serde_json::json!({ "tokens": out })).into_response()
}

async fn api_used(State(s): State<AppState>) -> impl IntoResponse {
    let u = UsedRegistry::load(&s.ctx.used_file());
    let mut pairs: Vec<_> = u.pairs.iter().cloned().collect();
    pairs.sort();
    let mut m: Vec<_> = u.music.iter().map(|(k, v)| (k.clone(), *v)).collect();
    m.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Json(serde_json::json!({
        "pairs_total": u.pairs.len(),
        "music_total": u.music.len(),
        "music_top": m.iter().take(50).collect::<Vec<_>>(),
    }))
    .into_response()
}

pub fn router_for(ctx: &Context) -> Router {
    let state = AppState {
        ctx: Arc::new(clone_ctx(ctx)),
        run: Arc::new(Mutex::new(RunInfo::default())),
        port: 3000,
    };
    Router::new()
        .route("/", get(index))
        .route("/favicon.ico", get(favicon))
        .route("/report.html", get(report_page))
        .route("/api/state", get(api_state))
        .route("/api/publish", post(api_publish))
        .route("/api/shorts", get(api_shorts))
        .route("/api/shorts/publish", post(api_shorts_publish))
        .route("/api/settings", get(api_get_settings).put(api_put_settings))
        .route("/api/history", get(api_history))
        .route("/api/tokens", get(api_tokens))
        .route("/api/used", get(api_used))
        .with_state(state)
}

