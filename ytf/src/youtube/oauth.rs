use anyhow::{bail, Context as _, Result};
use serde::Deserialize;
use std::io::Write;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::settings::Context;
use crate::tokens::TokenStore;

#[derive(Debug, Deserialize)]
struct TokenResp {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Получить refresh_token для папки канала.
///
/// Схема максимально простая для пользователя:
///   1. программа поднимает локальный сервер-перехватчик на 127.0.0.1:<случайный порт>
///   2. печатает ссылку Google и открывает её в браузере по умолчанию
///      (или ссылку вставляют в браузер профиля канала в Nstbrowser)
///   3. пользователь нажимает «Продолжить»/«Allow»
///   4. Google перебрасывает на http://localhost:<порт>/?code=... —
///      код программа ловит САМА, копировать ничего не нужно
pub async fn run(ctx: &Context, folder: &str, title: Option<&str>) -> Result<()> {
    let cs = ctx.root.join("client_secret.json");
    if !cs.exists() {
        bail!("нет client_secret.json в {}", ctx.root.display());
    }
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&cs)?)?;
    let inst = v
        .get("installed")
        .or_else(|| v.get("web"))
        .context("в client_secret.json нет секции installed — нужен Desktop app OAuth client")?;
    let client_id = inst["client_id"].as_str().context("нет client_id")?.to_string();
    let client_secret = inst["client_secret"].as_str().context("нет client_secret")?.to_string();
    let project = client_id.split('-').next().unwrap_or("?").to_string();

    // upload — заливка, readonly — чтение сниппетов (проверка обложки)
    let scopes = vec![
        crate::tokens::SCOPE_UPLOAD,
        crate::tokens::SCOPE_READONLY,
    ];
    let scope_q = urlencode(&scopes.join(" "));

    // локальный перехватчик ответа Google
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let redirect = format!("http://localhost:{port}");

    let url = format!(
        "https://accounts.google.com/o/oauth2/auth\
         ?client_id={}\
         &redirect_uri={}\
         &response_type=code\
         &scope={}\
         &access_type=offline\
         &prompt=consent",
        urlencode(&client_id),
        urlencode(&redirect),
        scope_q
    );

    println!("\n  Выдача доступа каналу: {folder}\n");
    println!("  Проект Google Cloud: {project}\n");
    println!("  ─────────────────────────────────────────────────────────────");
    println!("  1. Открой ЭТУ ссылку в браузере аккаунта канала");
    println!("     (если канал заведён в Nstbrowser — открой ссылку в окне");
    println!("      ПРОФИЛЯ этого канала):");
    println!();
    println!("  {url}");
    println!();
    println!("  2. Нажми «Продолжить» / «Allow» (если Google предупреждает, что");
    println!("     приложение не проверено: «Дополнительные настройки» →");
    println!("     «Перейти на ...» → «Продолжить»).");
    println!("  3. Дальше ВСЁ АВТОМАТИЧЕСКИ: страница сама сообщит об успехе.");
    println!("  ─────────────────────────────────────────────────────────────\n");

    // пробуем открыть браузер по умолчанию (удобно, если нужный аккаунт уже там)
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", &url])
        .spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(&url).spawn();

    println!("  Жду разрешения (после нажатия «Продолжить» всё случится само)...\n");
    let code = wait_for_code(&listener).await?;
    println!("  ✓ Код получен, меняю на токен...");

    let client = super::auth::http_client()?;
    let resp = client
        .post(super::TOKEN_URL)
        .form(&[
            ("code", code.as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("Google отказал: {text}");
    }

    let tr: TokenResp = serde_json::from_str(&text)?;
    if let Some(e) = tr.error {
        bail!("{e}: {}", tr.error_description.unwrap_or_default());
    }
    let refresh = tr
        .refresh_token
        .context("Google не вернул refresh_token. Отзови доступ приложения и повтори: \
                 https://myaccount.google.com/permissions")?;

    let granted = tr.scope.unwrap_or_else(|| scopes.join(" "));
    println!("  Выдано scopes: {granted}");

    let mut store = TokenStore::load(ctx).unwrap_or_default();
    store.channels.insert(
        folder.to_string(),
        crate::tokens::ChannelToken {
            title: title.unwrap_or(folder).to_string(),
            client_id: client_id.clone(),
            client_secret,
            refresh_token: refresh,
            scopes: granted.split(' ').map(|s| s.to_string()).collect(),
        },
    );
    store.save(ctx)?;

    println!("\n  Записано в {}", ctx.tokens_file().display());
    println!("  Проверяю...\n");
    crate::tokens::check_all(ctx).await?;
    Ok(())
}

/// Принять один ответ Google на локальном сервере и вытащить ?code=
async fn wait_for_code(listener: &tokio::net::TcpListener) -> Result<String> {
    const OK_PAGE: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
        Connection: close\r\n\r\n<!doctype html><html><head><meta charset=utf-8>\
        <title>Готово</title></head><body style=\"font-family:sans-serif;text-align:center;\
        padding-top:60px\"><h2>✓ Токен получен</h2><p>Эту вкладку можно закрыть,\
        всё сохранилось в программе.</p></body></html>";
    const DENY_PAGE: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
        Connection: close\r\n\r\n<!doctype html><html><head><meta charset=utf-8>\
        <title>Отказ</title></head><body style=\"font-family:sans-serif;text-align:center;\
        padding-top:60px\"><h2>Доступ не выдан</h2><p>Вернись в терминал — там подробности.\
        Эту вкладку можно закрыть.</p></body></html>";
    const NOT_FOUND: &str = "HTTP/1.1 404 Not Found\r\nConnection: close\r\n\
        Content-Length: 0\r\n\r\n";

    loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = vec![0u8; 16384];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        if n == 0 {
            continue;
        }
        let req = String::from_utf8_lossy(&buf[..n]);
        let line = req.lines().next().unwrap_or("");
        // "GET /?code=...&scope=... HTTP/1.1"
        let Some(path) = line.split_whitespace().nth(1) else {
            let _ = sock.write_all(NOT_FOUND.as_bytes()).await;
            continue;
        };

        if let Some(code) = extract_code(path) {
            let _ = sock.write_all(OK_PAGE.as_bytes()).await;
            let _ = sock.flush().await;
            return Ok(code);
        }
        if let Some(err) = capture_error(path) {
            let _ = sock.write_all(DENY_PAGE.as_bytes()).await;
            let _ = sock.flush().await;
            bail!("Google вернул отказ: {err}");
        }
        // favicon и прочий шум — просто 404 и ждём дальше
        let _ = sock.write_all(NOT_FOUND.as_bytes()).await;
    }
}

fn capture_error(path: &str) -> Option<String> {
    let q = path.split('?').nth(1)?;
    for pair in q.split('&') {
        if let Some(v) = pair.strip_prefix("error=") {
            let desc = q
                .split('&')
                .find_map(|p| p.strip_prefix("error_description="))
                .unwrap_or("");
            let desc = urldecode(desc);
            return Some(format!("{} {}", urldecode(v), desc).trim().to_string());
        }
    }
    None
}

fn extract_code(s: &str) -> Option<String> {
    if let Some(i) = s.find("code=") {
        let rest = &s[i + 5..];
        let end = rest.find('&').unwrap_or(rest.len());
        let c = &rest[..end];
        if !c.is_empty() {
            return Some(urldecode(c));
        }
    }
    // некоторые клиенты отдают код в виде 4/xxxx
    if s.starts_with("4/") && s.len() > 2 {
        return Some(s.to_string());
    }
    None
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn код_из_url_вытаскивается() {
        assert_eq!(
            extract_code("/?code=4%2F0AXabc&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fyoutube.upload"),
            Some("4/0AXabc".into())
        );
        assert_eq!(extract_code("/favicon.ico"), None);
    }

    #[test]
    fn ошибка_вытаскивается() {
        assert_eq!(
            capture_error("/?error=access_denied&error_description=User%20denied"),
            Some("access_denied User denied".into())
        );
    }
}
