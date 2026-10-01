pub mod api;
pub mod auth;
pub mod oauth;
pub mod quota;
pub mod schedule;
pub mod upload;

use serde::{Deserialize, Serialize};

pub const API_BASE: &str = "https://www.googleapis.com/youtube/v3";
pub const UPLOAD_BASE: &str = "https://www.googleapis.com/upload/youtube/v3";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Ошибка API с разбором, чтобы понимать что именно сломалось
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("квота API исчерпана (quotaExceeded)")]
    QuotaExceeded,
    #[error("суточный лимит загрузок на канал (uploadLimitExceeded)")]
    UploadLimit,
    #[error("слишком много обложек подряд (uploadRateLimitExceeded)")]
    ThumbnailRate,
    #[error("не хватает прав (403 {0})")]
    Forbidden(String),
    #[error("токен недействителен: {0}")]
    InvalidToken(String),
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("сеть: {0}")]
    Network(String),
    #[error("{0}")]
    Other(String),
}

impl ApiError {
    /// Разобрать тело ошибки Google в осмысленный тип
    pub fn classify(status: u16, body: &str) -> Self {
        classify(status, body)
    }
    pub fn is_retriable(&self) -> bool {
        match self {
            ApiError::Network(_) | ApiError::QuotaExceeded | ApiError::ThumbnailRate => true,
            ApiError::Http { status, .. } => matches!(status, 500 | 502 | 503 | 504),
            _ => false,
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Other(format!("{e:#}"))
    }
}
impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        ApiError::Other(e.to_string())
    }
}

pub fn classify(status: u16, body: &str) -> ApiError {
    let b = body.to_string();
    if b.contains("quotaExceeded") || b.contains("rateLimitExceeded") && b.contains("quota") {
        return ApiError::QuotaExceeded;
    }
    if b.contains("uploadLimitExceeded") {
        return ApiError::UploadLimit;
    }
    if b.contains("uploadRateLimitExceeded") {
        return ApiError::ThumbnailRate;
    }
    if b.contains("insufficientPermissions") || b.contains("forbidden") {
        return ApiError::Forbidden(truncate(&b, 200));
    }
    if b.contains("invalid_grant") || b.contains("invalid_token") {
        return ApiError::InvalidToken(truncate(&b, 200));
    }
    ApiError::Http { status, body: truncate(&b, 400) }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() } else { format!("{}…", &s[..n]) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub channel_id: String,
    pub title: String,
    pub uploads_playlist: String,
    pub subscribers: String,
    pub views: String,
    pub videos: String,
    pub thumbnail: String,
}

/// Один видео в uploads-плейлисте
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoEntry {
    pub id: String,
    pub title: String,
    /// отложенные — publishAt, уже опубликованные — publishedAt
    pub publish_at: String,
    /// true = ещё в отложке, false = уже вышло
    pub scheduled: bool,
    pub privacy: String,
}
