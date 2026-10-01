use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use super::auth::{get_json, post_json, Session};
use super::{ApiError, VideoEntry, API_BASE, UPLOAD_BASE};

#[derive(Debug, Deserialize)]
struct PlaylistResp {
    #[serde(default)]
    items: Vec<PlaylistItem>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PlaylistItem {
    #[serde(rename = "contentDetails")]
    content_details: ContentDetails,
}

#[derive(Debug, Deserialize)]
struct ContentDetails {
    #[serde(rename = "videoId")]
    video_id: String,
}

#[derive(Debug, Deserialize)]
struct VideosResp {
    #[serde(default)]
    items: Vec<VideoItem>,
}

#[derive(Debug, Deserialize)]
struct VideoItem {
    id: String,
    #[serde(default)]
    snippet: VSnippet,
    #[serde(default)]
    status: VStatus,
}

#[derive(Debug, Default, Deserialize)]
struct VSnippet {
    #[serde(default)]
    title: String,
    #[serde(rename = "publishedAt", default)]
    published_at: String,
    #[serde(default)]
    thumbnails: VThumbs,
}

#[derive(Debug, Default, Deserialize)]
struct VThumbs {
    #[serde(default)]
    default: Option<TUrl>,
    #[serde(default)]
    medium: Option<TUrl>,
    #[serde(default)]
    high: Option<TUrl>,
    #[serde(default)]
    maxres: Option<TUrl>,
}

#[derive(Debug, Deserialize)]
struct TUrl {
    url: String,
}

#[derive(Debug, Default, Deserialize)]
struct VStatus {
    #[serde(default)]
    privacy_status: String,
    #[serde(rename = "publishAt", default)]
    publish_at: String,
    #[serde(rename = "uploadStatus", default)]
    upload_status: String,
}

impl VThumbs {
    /// Самая большая доступная — по ней судим, какую обложку выбрал YouTube
    fn best(&self) -> Option<String> {
        self.maxres
            .as_ref()
            .or(self.high.as_ref())
            .or(self.medium.as_ref())
            .or(self.default.as_ref())
            .map(|t| t.url.clone())
    }
    fn any(&self) -> Option<String> {
        self.best()
    }
}

/// Что уже есть в uploads-плейлисте канала
pub async fn channel_videos(session: &Session, uploads_playlist: &str, max: usize) -> Result<Vec<VideoEntry>> {
    let mut ids: Vec<String> = Vec::new();
    let mut page: Option<String> = None;

    loop {
        let mut url = format!(
            "{API_BASE}/playlistItems?part=contentDetails&maxResults=50&playlistId={}",
            urlenc(uploads_playlist)
        );
        if let Some(p) = &page {
            url.push_str(&format!("&pageToken={}", urlenc(p)));
        }
        let r: PlaylistResp = get_json(session, &url).await?;
        for it in r.items {
            ids.push(it.content_details.video_id);
        }
        if ids.len() >= max {
            break;
        }
        match r.next_page_token {
            Some(t) if !t.is_empty() => page = Some(t),
            _ => break,
        }
    }

    ids.truncate(max);
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for chunk in ids.chunks(50) {
        let url = format!(
            "{API_BASE}/videos?part=snippet,status&id={}",
            chunk.join(",")
        );
        let r: VideosResp = get_json(session, &url).await?;
        for v in r.items {
            let scheduled = !v.status.publish_at.is_empty();
            let when = if scheduled { v.status.publish_at } else { v.snippet.published_at };
            out.push(VideoEntry {
                id: v.id,
                title: v.snippet.title,
                // отложенные — по publishAt, уже вышедшие — по publishedAt
                publish_at: when,
                scheduled,
                privacy: v.status.privacy_status,
            });
        }
    }
    Ok(out)
}

/// Какие даты (YYYY-MM-DD) уже заняты — учитываем и отложенные, и опубликованные
pub async fn occupied_dates(session: &Session, uploads_playlist: &str) -> Result<Vec<String>> {
    let vids = channel_videos(session, uploads_playlist, 200).await?;
    let mut dates: Vec<String> = vids
        .iter()
        .filter_map(|v| {
            let s = &v.publish_at;
            if s.len() >= 10 { Some(s[..10].to_string()) } else { None }
        })
        .collect();
    dates.sort();
    dates.dedup();
    Ok(dates)
}

/// id + текущая обложка (по ней решаем, чёрная ли она)
pub async fn video_thumb_url(session: &Session, video_id: &str) -> Result<Option<String>> {
    let url = format!("{API_BASE}/videos?part=snippet&id={}", urlenc(video_id));
    let r: VideosResp = get_json(session, &url).await?;
    Ok(r.items
        .into_iter()
        .next()
        .and_then(|v| v.snippet.thumbnails.any()))
}

/// Полная самопроверка после загрузки: ждём «processed».
///
/// Доказано на живом канале: у обрезанного файла videos.list вечно пуст
/// («Processing abandoned»), у нормального uploadStatus доходит до
/// processed. Поэтому: видео не появилось за отведённое время или
/// uploadStatus failed/rejected — это ошибка загрузки.
pub async fn wait_processed(
    session: &Session,
    video_id: &str,
    attempts: u32,
    delay_secs: u64,
) -> Result<()> {
    let url = format!("{API_BASE}/videos?part=status&id={}", urlenc(video_id));
    for i in 0..attempts {
        let r: VideosResp = get_json(session, &url).await?;
        if let Some(v) = r.items.into_iter().next() {
            let st = v.status.upload_status.trim().to_ascii_lowercase();
            match st.as_str() {
                "processed" | "uploaded" => {
                    tracing::info!("видео {video_id}: принято YouTube (uploadStatus={st})");
                    return Ok(());
                }
                "failed" | "rejected" | "deleted" => {
                    anyhow::bail!("YouTube отклонил файл (uploadStatus={st}) — видео {video_id} битое")
                }
                _ => {
                    // processing — норма, продолжаем ждать
                }
            }
        }
        if i + 1 < attempts {
            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
        }
    }
    anyhow::bail!(
        "видео {video_id} не обработано YouTube за {} мин — файл не принят \
         (в Studio это выглядит как «Processing abandoned»)",
        (attempts as u64 * delay_secs) / 60
    )
}

/// Скачать обложку YouTube (небольшую) для проверки на чёрноту
pub async fn fetch_thumb_bytes(url: &str) -> Result<Vec<u8>> {
    let client = super::auth::http_client()?;
    let r = client
        .get(url)
        .send()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))?;
    Ok(r.bytes().await?.to_vec())
}

/// Залить свою обложку: POST /upload/youtube/v3/thumbnails/set
pub async fn set_thumbnail(session: &Session, video_id: &str, jpeg: Vec<u8>) -> Result<()> {
    let part = reqwest::multipart::Part::bytes(jpeg)
        .file_name("thumb.jpg")
        .mime_str("image/jpeg")
        .context("mime")?;
    let form = reqwest::multipart::Form::new()
        .text("videoId", video_id.to_string())
        .part("media", part);

    let client = super::auth::http_client()?;
    let url = format!("{UPLOAD_BASE}/thumbnails/set");
    let resp = client
        .post(&url)
        .bearer_auth(&session.token)
        .multipart(form)
        .send()
        .await
        .map_err(|e| ApiError::Network(e.to_string()))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(ApiError::classify(status.as_u16(), &text).into());
    }
    Ok(())
}

pub fn urlenc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ── тело запроса вставки ──

#[derive(Debug, Clone, Serialize)]
pub struct InsertBody {
    pub snippet: SnippetBody,
    pub status: StatusBody,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnippetBody {
    pub title: String,
    pub description: String,
    #[serde(rename = "categoryId")]
    pub category_id: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusBody {
    #[serde(rename = "privacyStatus")]
    pub privacy_status: String,
    #[serde(rename = "selfDeclaredMadeForKids")]
    pub made_for_kids: bool,
    #[serde(rename = "publishAt", skip_serializing_if = "Option::is_none")]
    pub publish_at: Option<String>,
}

#[allow(dead_code)]
async fn _unused(s: &Session) -> Result<()> {
    post_json::<_, serde_json::Value>(s, "", &serde_json::json!({})).await?;
    Ok(())
}
