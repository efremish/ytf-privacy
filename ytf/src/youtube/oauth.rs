use anyhow::{bail, Context as _, Result};
use serde::Deserialize;
use std::io::Write;
use std::process::Stdio;

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

/// Получить refresh_token для папки канала (схема «перенос ссылки»).
///
/// Работает с любым браузером, включая профили Nstbrowser с прокси:
///   1. программа печатает ссылку Google и кладёт её в буфер обмена
///   2. пользователь вставляет ссылку в ОКНО ПРОФИЛЯ канала (Nstbrowser)
///      или в браузер, где залогинен аккаунт канала
///   3. нажимает «Продолжить» / «Allow»
///   4. браузер покажет «страница недоступна» — это норма: в адресной
///      строке будет http://localhost/?code=...
///   5. пользователь копирует адрес из строки и вставляет в программу
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

    let redirect = "http://localhost";
    let url = format!(
        "https://accounts.google.com/o/oauth2/auth\
         ?client_id={}\
         &redirect_uri={}\
         &response_type=code\
         &scope={}\
         &access_type=offline\
         &prompt=consent",
        urlencode(&client_id),
        urlencode(redirect),
        scope_q
    );

    println!("\n  Подключение канала: {folder}");
    println!("  Проект Google Cloud: {project}");
    println!("  ────────────────────────────────────────────────────────────────");
    println!("  ШАГ 1. Ссылка для разрешения доступа (она УЖЕ в буфере обмена):");
    println!();
    println!("  {url}");
    println!();
    println!("  ШАГ 2. Вставь эту ссылку в адресную строку ОКНА ПРОФИЛЯ КАНАЛА");
    println!("         в Nstbrowser (где залогинен нужный YouTube-аккаунт)");
    println!("         и открой. Подойдёт и обычный браузер с этим аккаунтом.");
    println!();
    println!("  ШАГ 3. Нажми «Продолжить» / «Allow».");
    println!("         Если Google предупреждает «приложение не проверено»:");
    println!("         «Дополнительные настройки» → «Перейти на страницу (небезопасно)»");
    println!("         → «Продолжить».");
    println!();
    println!("  ШАГ 4. Браузер покажет «страница недоступна» — ЭТО НОРМАЛЬНО.");
    println!("         В адресной строке будет адрес вида");
    println!("         http://localhost/?code=4/0AX...&scope=...");
    println!();
    println!("  ШАГ 5. Скопируй ВЕСЬ адрес из строки браузера и вставь сюда.");
    println!("  ────────────────────────────────────────────────────────────────\n");

    copy_to_clipboard(&url);
    println!("  (ссылка скопирована в буфер обмена — просто Ctrl+V / Cmd+V)\n");

    print!("  Вставь адрес сюда и нажми Enter: ");
    std::io::stdout().flush().ok();

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let pasted = input.trim();

    if pasted.is_empty() {
        bail!("пустая строка — запускай заново");
    }
    let code = extract_code(pasted)
        .ok_or_else(|| anyhow::anyhow!("в вставленном тексте не нашлось ?code=... — убедись, \
                 что копируешь адрес ПОСЛЕ нажатия «Продолжить»"))?;

    println!("\n  ✓ Код принят, меняю на токен...");

    let client = super::auth::http_client()?;
    let resp = client
        .post(super::TOKEN_URL)
        .form(&[
            ("code", code.as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("redirect_uri", redirect),
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
    println!("  Проверяю канал...\n");
    crate::tokens::check_all(ctx).await?;
    Ok(())
}

/// Скопировать ссылку в буфер обмена (best-effort, ошибка не критична).
fn copy_to_clipboard(s: &str) {
    #[cfg(target_os = "macos")]
    {
        if let Ok(mut child) = std::process::Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
            if let Some(mut si) = child.stdin.take() {
                let _ = si.write_all(s.as_bytes());
                drop(si);
                let _ = child.wait();
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(mut child) = std::process::Command::new("cmd")
            .args(["/c", "clip"])
            .stdin(Stdio::piped())
            .spawn()
        {
            if let Some(mut si) = child.stdin.take() {
                let _ = si.write_all(s.as_bytes());
                drop(si);
                let _ = child.wait();
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(mut child) = std::process::Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .spawn()
        {
            if let Some(mut si) = child.stdin.take() {
                let _ = si.write_all(s.as_bytes());
                drop(si);
                let _ = child.wait();
            }
        }
    }
}

/// Вытащить ?code= из вставленного адреса (или принять голый код "4/xxx")
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

/// Вытащить error= из вставленного адреса, чтобы объяснить отказ
fn capture_error(s: &str) -> Option<String> {
    let q = s.split('?').nth(1)?;
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
            extract_code("http://localhost/?code=4%2F0AXabc&scope=https%3A%2F%2Fwww.googleapis.com"),
            Some("4/0AXabc".into())
        );
        assert_eq!(extract_code("http://localhost/?state=x&error=access_denied"), None);
    }

    #[test]
    fn голый_код_тоже_принимается() {
        assert_eq!(extract_code("4/0AXabc-XYZ"), Some("4/0AXabc-XYZ".into()));
    }

    #[test]
    fn ошибка_вытаскивается() {
        assert_eq!(
            capture_error("http://localhost/?error=access_denied&error_description=User%20denied"),
            Some("access_denied User denied".into())
        );
    }
}
