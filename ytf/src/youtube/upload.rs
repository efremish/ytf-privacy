use anyhow::Context as _;
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;

use super::api::InsertBody;
use super::auth::{http_client, Session};
use super::{ApiError, UPLOAD_BASE};
use crate::media::Rendered;

const CHUNK: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("ffmpeg упал: {0}")]
    Render(String),
    #[error("загрузка не удалась: {0}")]
    Api(#[from] ApiError),
    #[error("{0}")]
    Other(String),
}

fn videos_url() -> String {
    format!("{UPLOAD_BASE}/videos?uploadType=resumable&part=snippet,status")
}

/// Начать resumable-сессию. Возвращает Location для последующих PUT
async fn start_resumable(
    session: &Session,
    body: &InsertBody,
    total: Option<u64>,
    mime: &str,
) -> Result<String, ApiError> {
    let client = http_client()?;
    let json = serde_json::to_vec(body).map_err(|e| ApiError::Other(e.to_string()))?;

    let mut req = client
        .post(videos_url())
        .bearer_auth(&session.token)
        .header("Content-Type", "application/json; charset=UTF-8")
        .header("X-Upload-Content-Type", mime)
        .body(json);

    if let Some(t) = total {
        req = req.header("X-Upload-Content-Length", t.to_string());
    }

    let resp = req.send().await.map_err(|e| ApiError::Network(e.to_string()))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(ApiError::classify(status.as_u16(), &text));
    }
    resp.headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| ApiError::Other("Google не вернул Location".into()))
}

fn backoff(attempt: u32, base: u64) -> Duration {
    let secs = base.saturating_mul(1u64 << attempt.min(6));
    Duration::from_millis(rand::random::<u64>() % (secs * 1000).max(1))
}

/// Загрузить готовый файл с диска
pub async fn upload_file(
    session: &Session,
    path: &Path,
    body: &InsertBody,
    retries: u32,
    base_delay: u64,
    mut on_progress: impl FnMut(u8),
) -> Result<String, UploadError> {
    let total = std::fs::metadata(path)
        .map_err(|e| UploadError::Other(format!("нет файла {}: {e}", path.display())))?
        .len();
    let location = start_resumable(session, body, Some(total), "video/mp4")
        .await
        .map_err(UploadError::Api)?;

    let data = tokio::fs::read(path)
        .await
        .map_err(|e| UploadError::Other(format!("не прочитать {}: {e}", path.display())))?;
    let client = http_client().map_err(|e| UploadError::Other(format!("{e:#}")))?;
    let mut sent: u64 = 0;

    while sent < total {
        let end = (sent + CHUNK as u64).min(total) - 1;
        let slice = &data[sent as usize..=(end as usize)];
        let range = format!("bytes {sent}-{end}/{total}");

        let mut attempt = 0u32;
        let body_resp = loop {
            let r = client
                .put(&location)
                .bearer_auth(&session.token)
                .header("Content-Range", &range)
                .header("Content-Type", "video/mp4")
                .body(slice.to_vec())
                .send()
                .await;

            match r {
                Ok(resp) => {
                    let st = resp.status();
                    if st.is_success() || st.as_u16() == 308 {
                        break resp;
                    }
                    let text = resp.text().await.unwrap_or_default();
                    let e = ApiError::classify(st.as_u16(), &text);
                    if !e.is_retriable() || attempt >= retries {
                        return Err(e.into());
                    }
                    tracing::warn!("чанк {range} не принят ({}), пробую ещё", e);
                    attempt += 1;
                    tokio::time::sleep(backoff(attempt, base_delay)).await;
                }
                Err(e) => {
                    let err = ApiError::Network(e.to_string());
                    if attempt >= retries {
                        return Err(err.into());
                    }
                    attempt += 1;
                    tokio::time::sleep(backoff(attempt, base_delay)).await;
                }
            }
        };

        let status = body_resp.status();
        let text = body_resp.text().await.unwrap_or_default();

        // 308 = "принято, жди дальше", финальный ответ приходит с телом
        if status.as_u16() != 308 && !text.trim().is_empty() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
                    sent = total;
                    on_progress(100);
                    return Ok(id.to_string());
                }
            }
            if status.is_success() {
                // пустой ответ без id — считаем успехом, id заберём отдельно
                sent = total;
                on_progress(100);
                return Ok(String::new());
            }
        }

        sent = end + 1;
        let pct = (sent * 100 / total).min(100) as u8;
        on_progress(pct);
    }
    Ok(String::new())
}

/// Загрузить прямо из потока ffmpeg — на диск не пишем вообще
///
/// Протокол resumable при неизвестном размере: каждый чанк обязан нести
/// `Content-Range: bytes {start}-{end}/*`. Без него Google считает, что
/// пришло ВСЁ видео, закрывает сессию после первого чанка — и получается
/// «Processing abandoned» на обрезанном файле.
pub async fn upload_stream<R>(
    session: &Session,
    reader: &mut R,
    body: &InsertBody,
    retries: u32,
    base_delay: u64,
    mut on_progress: impl FnMut(u64),
) -> Result<String, UploadError>
where
    R: tokio::io::AsyncBufRead + Unpin + Send,
{
    let location = start_resumable(session, body, None, "video/mp4")
        .await
        .map_err(UploadError::Api)?;
    let client = http_client().map_err(|e| UploadError::Other(format!("{e:#}")))?;

    /// Сколько байт подтверждено Google (из заголовка Range ответа 308).
    /// "bytes=0-12345" -> 12346 байт принято (границы inclusive)
    fn confirmed(range_header: Option<&reqwest::header::HeaderValue>) -> Option<u64> {
        let s = range_header?.to_str().ok()?;
        s.rsplit('-').next()?.trim().parse::<u64>().ok().map(|e| e + 1)
    }

    let mut sent: u64 = 0; // байт, уже принятых Google

    loop {
        let buf = match reader.fill_buf().await {
            Ok(b) => b,
            Err(e) => return Err(UploadError::Render(format!("чтение потока: {e}"))),
        };
        if buf.is_empty() {
            // ffmpeg закончил — финализируем сессию: «всего sent байт»
            let mut attempt = 0u32;
            loop {
                let r = client
                    .put(&location)
                    .bearer_auth(&session.token)
                    .header("Content-Range", format!("bytes */{sent}"))
                    .header("Content-Length", "0")
                    .send()
                    .await;
                match r {
                    Ok(resp) => {
                        let st = resp.status();
                        let text = resp.text().await.unwrap_or_default();
                        if st.is_success() {
                            on_progress(sent);
                            return extract_id(&text);
                        }
                        if st.as_u16() != 308 {
                            let e = ApiError::classify(st.as_u16(), &text);
                            if !e.is_retriable() || attempt >= retries {
                                return Err(e.into());
                            }
                        }
                    }
                    Err(e) => {
                        if attempt >= retries {
                            return Err(UploadError::Api(ApiError::Network(e.to_string())));
                        }
                    }
                }
                attempt += 1;
                tokio::time::sleep(backoff(attempt, base_delay)).await;
            }
        }

        let take = buf.len().min(CHUNK);
        let chunk = buf[..take].to_vec();
        reader.consume(take);
        let chunk_len = chunk.len() as u64;
        let chunk_end_incl = sent + chunk_len - 1;

        // Досылка чанка: после сбоя сети Google мог принять часть байт —
        // продолжаем с подтверждённого смещения внутри этого же чанка.
        let mut offset: u64 = 0; // сколько байт ЧАНКА уже принято
        let mut attempt = 0u32;
        loop {
            let start = sent + offset;
            let body_slice = &chunk[offset as usize..];
            let r = client
                .put(&location)
                .bearer_auth(&session.token)
                .header("Content-Type", "video/mp4")
                .header("Content-Range", format!("bytes {start}-{chunk_end_incl}/*"))
                .header("Content-Length", body_slice.len().to_string())
                .body(body_slice.to_vec())
                .send()
                .await;
            match r {
                Ok(resp) => {
                    let st = resp.status();
                    if st.as_u16() == 308 {
                        let got = confirmed(resp.headers().get("range")).unwrap_or(start);
                        let got = got.min(chunk_end_incl + 1);
                        if got > sent + offset {
                            attempt = 0; // есть прогресс
                        }
                        offset = got - sent;
                        if offset >= chunk_len {
                            sent += chunk_len;
                            on_progress(sent);
                            break;
                        }
                        continue; // досылаем остаток чанка
                    }
                    let text = resp.text().await.unwrap_or_default();
                    if st.is_success() {
                        // сессия закрыта Google — это был последний чанк
                        sent += chunk_len;
                        on_progress(sent);
                        return extract_id(&text);
                    }
                    let e = ApiError::classify(st.as_u16(), &text);
                    if !e.is_retriable() || attempt >= retries {
                        return Err(e.into());
                    }
                    tracing::warn!("чанк {start}-{chunk_end_incl} не принят ({}), пробую ещё", e);
                    attempt += 1;
                }
                Err(e) => {
                    // сеть упала — спрашиваем Google, сколько он уже принял
                    if attempt >= retries {
                        return Err(UploadError::Api(ApiError::Network(e.to_string())));
                    }
                    if let Ok(pr) = client
                        .put(&location)
                        .bearer_auth(&session.token)
                        .header("Content-Range", "bytes */*")
                        .header("Content-Length", "0")
                        .send()
                        .await
                    {
                        let pst = pr.status();
                        if pst.is_success() {
                            let text = pr.text().await.unwrap_or_default();
                            return extract_id(&text);
                        }
                        if pst.as_u16() == 308 {
                            if let Some(got) = confirmed(pr.headers().get("range")) {
                                let got = got.min(chunk_end_incl + 1);
                                if got > sent {
                                    offset = got - sent;
                                    attempt = 0; // часть уже там — есть прогресс
                                }
                            }
                        }
                    } else {
                        let _ = e;
                    }
                    attempt += 1;
                }
            }
            tokio::time::sleep(backoff(attempt, base_delay)).await;
        }
    }
}

fn extract_id(text: &str) -> Result<String, UploadError> {
    if text.trim().is_empty() {
        return Ok(String::new());
    }
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| UploadError::Other(format!("плохой ответ после загрузки: {e}")))?;
    Ok(v.get("id")
        .and_then(|x| x.as_str())
        .map(String::from)
        .unwrap_or_default())
}

/// Загрузить видео из памяти одним куском (размер известен — полный Content-Range).
/// Этот путь проверен на живом канале: файл проходит обработку YouTube.
async fn upload_memory(
    session: &Session,
    data: Vec<u8>,
    body: &InsertBody,
) -> Result<String, UploadError> {
    let total = data.len() as u64;
    let location = start_resumable(session, body, Some(total), "video/mp4")
        .await
        .map_err(UploadError::Api)?;
    let client = http_client().map_err(|e| UploadError::Other(format!("{e:#}")))?;

    let mut attempt = 0u32;
    loop {
        let r = client
            .put(&location)
            .bearer_auth(&session.token)
            .header("Content-Type", "video/mp4")
                .header("Content-Range", format!("bytes 0-{}/{}", total - 1, total))
            .body(data.clone())
            .send()
            .await;
        match r {
            Ok(resp) => {
                let st = resp.status();
                let text = resp.text().await.unwrap_or_default();
                if st.is_success() {
                    return extract_id(&text);
                }
                let e = ApiError::classify(st.as_u16(), &text);
                if !e.is_retriable() || attempt >= 3 {
                    return Err(e.into());
                }
                attempt += 1;
            }
            Err(e) => {
                if attempt >= 3 {
                    return Err(UploadError::Api(ApiError::Network(e.to_string())));
                }
                attempt += 1;
            }
        }
        tokio::time::sleep(backoff(attempt, 5)).await;
    }
}

/// Загрузить то, что вернул рендер: файл или поток
pub async fn upload_rendered(
    session: &Session,
    rendered: Rendered,
    body: &InsertBody,
    retries: u32,
    base_delay: u64,
    mut on_progress: impl FnMut(u64),
) -> Result<String, UploadError> {
    match rendered {
        Rendered::File(p) => {
            let id = upload_file(session, &p, body, retries, base_delay, |mut pct| {
            on_progress(u64::from(pct));
        })
        .await?;
            // файл больше не нужен
            let _ = std::fs::remove_file(&p);
            Ok(id)
        }
        Rendered::Memory(data) => upload_memory(session, data, body).await,
        Rendered::Stream(s) => {
            let mut r = tokio::io::BufReader::with_capacity(1 << 20, s);
            upload_stream(session, &mut r, body, retries, base_delay, on_progress).await
        }
    }
}
