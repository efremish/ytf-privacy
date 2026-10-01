use anyhow::{Context as _, Result};
use serde::Deserialize;
use std::time::Duration;

use super::{ApiError, ChannelInfo, API_BASE, TOKEN_URL};
use crate::tokens::ChannelToken;

#[derive(Debug, Deserialize)]
struct TokenResp {
    access_token: String,
    expires_in: i64,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Кэш access_token на время жизни
#[derive(Clone)]
pub struct Session {
    pub token: String,
    expires_at: std::time::Instant,
    /// scopes, которые реально выдал Google (могут быть шире запрошенных)
    pub granted_scopes: String,
}

impl Session {
    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.token)
    }
}

pub fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .connect_timeout(Duration::from_secs(20))
        .pool_max_idle_per_host(8)
        .build()
        .context("не создать HTTP-клиент")
}

/// Обновить access_token по refresh_token
pub async fn refresh(tok: &ChannelToken) -> Result<Session> {
    let client = http_client()?;
    let scopes: Vec<String> = if tok.scopes.is_empty() {
        vec![crate::tokens::SCOPE_UPLOAD.into(), crate::tokens::SCOPE_READONLY.into()]
    } else {
        tok.scopes.clone()
    };
    let _ = &scopes;

    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("client_id", tok.client_id.as_str()),
            ("client_secret", tok.client_secret.as_str()),
            ("refresh_token", tok.refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();

    if !status.is_success() {
        return Err(ApiError::classify(status.as_u16(), &text).into());
    }

    let tr: TokenResp = serde_json::from_str(&text).context("не распарсил ответ token")?;
    if let Some(e) = tr.error {
        let d = tr.error_description.unwrap_or_default();
        return Err(ApiError::InvalidToken(format!("{e}: {d}")).into());
    }

    Ok(Session {
        token: tr.access_token,
        expires_at: std::time::Instant::now()
            + Duration::from_secs(tr.expires_in.max(60) as u64 - 30),
        granted_scopes: tr.scope,
    })
}

/// Держать сессию и обновлять по мере надобности
pub struct TokenManager {
    tok: ChannelToken,
    session: Option<Session>,
}

impl TokenManager {
    pub fn new(tok: &ChannelToken) -> Self {
        Self { tok: tok.clone(), session: None }
    }

    pub async fn session(&mut self) -> Result<Session> {
        let need = match &self.session {
            Some(s) => std::time::Instant::now() + Duration::from_secs(30) >= s.expires_at,
            None => true,
        };
        if need {
            self.session = Some(refresh(&self.tok).await?);
        }
        Ok(self.session.as_ref().expect("только что заполнено").clone())
    }
}

#[derive(Debug, Deserialize)]
struct ChannelsResp {
    #[serde(default)]
    items: Vec<ChannelItem>,
}

#[derive(Debug, Deserialize)]
struct ChannelItem {
    id: String,
    #[serde(default)]
    snippet: Snippet,
    #[serde(default)]
    statistics: Stats,
    #[serde(rename = "contentDetails", default)]
    content_details: ContentDetails,
}

#[derive(Debug, Default, Deserialize)]
struct Snippet {
    #[serde(default)]
    title: String,
    #[serde(default)]
    thumbnails: Thumb,
}

#[derive(Debug, Default, Deserialize)]
struct Thumb {
    #[serde(default)]
    default: Option<ThumbUrl>,
    #[serde(default)]
    medium: Option<ThumbUrl>,
}

#[derive(Debug, Deserialize)]
struct ThumbUrl {
    url: String,
}

#[derive(Debug, Default, Deserialize)]
struct Stats {
    #[serde(default)]
    subscriber_count: Option<String>,
    #[serde(default)]
    view_count: Option<String>,
    #[serde(default)]
    video_count: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ContentDetails {
    #[serde(rename = "relatedPlaylists", default)]
    related_playlists: RelatedPlaylists,
}

#[derive(Debug, Default, Deserialize)]
struct RelatedPlaylists {
    #[serde(rename = "uploads", default)]
    uploads: String,
}

/// Query-string для channels.list: mine=true&part=...
fn channels_query(part: &str) -> String {
    format!("mine=true&part={part}")
}

/// Узнать, какой канал за токеном. Используется в `ytf tokens`
pub async fn probe_channel(tok: &ChannelToken) -> Result<ChannelInfo> {
    let s = refresh(tok).await?;
    let client = http_client()?;
    let url = format!("{API_BASE}/channels?{}", channels_query("snippet,statistics,contentDetails"));
    let resp = client
        .get(&url)
        .bearer_auth(&s.token)
        .send()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(ApiError::classify(status.as_u16(), &text).into());
    }
    let cr: ChannelsResp = serde_json::from_str(&text)?;
    let item = cr.items.into_iter().next().context("канал не найден для этого токена")?;
    Ok(ChannelInfo {
        channel_id: item.id,
        title: item.snippet.title,
        uploads_playlist: item.content_details.related_playlists.uploads,
        subscribers: item.statistics.subscriber_count.unwrap_or_default(),
        views: item.statistics.view_count.unwrap_or_default(),
        videos: item.statistics.video_count.unwrap_or_default(),
        thumbnail: item
            .snippet
            .thumbnails
            .medium
            .or(item.snippet.thumbnails.default)
            .map(|t| t.url)
            .unwrap_or_default(),
    })
}

pub async fn channel_info(session: &Session) -> Result<ChannelInfo> {
    let client = http_client()?;
    let url = format!("{API_BASE}/channels?{}", channels_query("snippet,statistics,contentDetails"));
    let resp = client
        .get(&url)
        .bearer_auth(&session.token)
        .send()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(ApiError::classify(status.as_u16(), &text).into());
    }
    let cr: ChannelsResp = serde_json::from_str(&text)?;
    let item = cr.items.into_iter().next().context("канал не найден")?;
    Ok(ChannelInfo {
        channel_id: item.id,
        title: item.snippet.title,
        uploads_playlist: item.content_details.related_playlists.uploads,
        subscribers: item.statistics.subscriber_count.unwrap_or_default(),
        views: item.statistics.view_count.unwrap_or_default(),
        videos: item.statistics.video_count.unwrap_or_default(),
        thumbnail: item
            .snippet
            .thumbnails
            .medium
            .or(item.snippet.thumbnails.default)
            .map(|t| t.url)
            .unwrap_or_default(),
    })
}

pub async fn get_json<T: serde::de::DeserializeOwned>(
    session: &Session,
    url: &str,
) -> Result<T> {
    let client = http_client()?;
    let resp = client
        .get(url)
        .bearer_auth(&session.token)
        .send()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(ApiError::classify(status.as_u16(), &text).into());
    }
    Ok(serde_json::from_str(&text)?)
}

pub async fn post_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
    session: &Session,
    url: &str,
    body: &B,
) -> Result<T> {
    let client = http_client()?;
    let resp = client
        .post(url)
        .bearer_auth(&session.token)
        .json(body)
        .send()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(ApiError::classify(status.as_u16(), &text).into());
    }
    Ok(serde_json::from_str(&text)?)
}

// silence unused warning for channels_query variants
#[allow(dead_code)]
fn _unused() -> String { channels_query("snippet") }
