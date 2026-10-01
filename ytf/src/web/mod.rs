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
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/state", get(api_state))
        .route("/api/publish", post(api_publish))
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
                "ready": c.is_ready(),
                "used_music": c.music.iter()
                    .filter(|m| used.music.contains_key(
                        &m.file_name().unwrap_or_default().to_string_lossy().to_lowercase()))
                    .count(),
            })
        })
        .collect();

    Json(serde_json::json!({
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
    };
    Router::new()
        .route("/", get(index))
        .route("/api/state", get(api_state))
        .route("/api/publish", post(api_publish))
        .route("/api/settings", get(api_get_settings).put(api_put_settings))
        .route("/api/history", get(api_history))
        .route("/api/tokens", get(api_tokens))
        .route("/api/used", get(api_used))
        .with_state(state)
}
