use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::settings::Context;

pub const SCOPE_UPLOAD: &str = "https://www.googleapis.com/auth/youtube.upload";
pub const SCOPE_READONLY: &str = "https://www.googleapis.com/auth/youtube.readonly";
pub const SCOPE_MANAGE: &str = "https://www.googleapis.com/auth/youtube";
pub const SCOPE_ANALYTICS: &str = "https://www.googleapis.com/auth/yt-analytics.readonly";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelToken {
    /// Имя канала на YouTube (для показа)
    #[serde(default)]
    pub title: String,
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl ChannelToken {
    pub fn label(&self) -> &str {
        if self.title.is_empty() { "?" } else { &self.title }
    }
    pub fn has_scope(&self, s: &str) -> bool {
        self.scopes.iter().any(|x| x == s) || self.scopes.iter().any(|x| x.ends_with("/youtube"))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenStore {
    /// ключ — имя папки канала в People/
    #[serde(default)]
    pub channels: BTreeMap<String, ChannelToken>,
}

impl TokenStore {
    pub fn load(ctx: &Context) -> Result<Self> {
        let p = ctx.tokens_file();
        if !p.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&p)
            .with_context(|| format!("не прочитать {}", p.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("не распарсить {}", p.display()))
    }

    pub fn save(&self, ctx: &Context) -> Result<()> {
        let p = ctx.tokens_file();
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(&p, raw).with_context(|| format!("не записать {}", p.display()))?;
        Ok(())
    }

    pub fn get(&self, folder: &str) -> Option<&ChannelToken> {
        self.channels.get(folder)
    }

    /// Импорт из старого tokens.txt + client_secret.json
    pub fn import_legacy(root: &Path) -> Result<usize> {
        let cs = root.join("client_secret.json");
        let txt = root.join("tokens.txt");
        if !cs.exists() || !txt.exists() {
            anyhow::bail!("нужны client_secret.json и tokens.txt в корне проекта");
        }

        let cc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cs)?)?;
        let installed = cc.get("installed").or_else(|| cc.get("web")).context(
            "в client_secret.json нет секции installed — это файл веб-клиента, \
             создай Desktop app OAuth client",
        )?;
        let client_id = installed["client_id"].as_str().context("нет client_id")?.to_string();
        let client_secret = installed["client_secret"].as_str().context("нет client_secret")?.to_string();

        let mut store = TokenStore::default();
        let mut current = String::new();
        let mut count = 0usize;

        for line in std::fs::read_to_string(&txt)?.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            if let Some(name) = line.strip_prefix('#') {
                current = name.trim().to_string();
                continue;
            }
            if let Some((rt, genres)) = line.split_once(" / ") {
                let rt = rt.trim().to_string();
                if rt.is_empty() {
                    continue;
                }
                // Имя папки канала: если в tokens.txt каталог жанров,
                // берём первый жанр как имя папки — её и создаст пользователь.
                let folder = genres
                    .split(',')
                    .map(str::trim)
                    .find(|g| !g.is_empty())
                    .unwrap_or(&current)
                    .to_string();
                store.channels.insert(
                    folder.clone(),
                    ChannelToken {
                        title: current.clone(),
                        client_id: client_id.clone(),
                        client_secret: client_secret.clone(),
                        refresh_token: rt,
                        scopes: vec![SCOPE_UPLOAD.into(), SCOPE_READONLY.into()],
                    },
                );
                count += 1;
            }
        }

        store.save_ctx(root)?;
        Ok(count)
    }

    fn save_ctx(&self, root: &Path) -> Result<()> {
        let p = root.join("tokens.json");
        std::fs::write(&p, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

pub async fn check_all(ctx: &Context) -> Result<()> {
    let store = TokenStore::load(ctx)?;
    if store.channels.is_empty() {
        println!("Токенов нет. Создай {}\n", ctx.tokens_file().display());
        println!("Формат:");
        println!(r#"{{
  "channels": {{
    "<имя папки канала>": {{
      "title": "Канал на YouTube",
      "client_id": "xxx.apps.googleusercontent.com",
      "client_secret": "xxx",
      "refresh_token": "1//...",
      "scopes": ["https://www.googleapis.com/auth/youtube.upload"]
    }}
  }}
}}"#);
        return Ok(());
    }

    let mut ok = 0;
    let mut bad = 0;
    for (folder, tok) in &store.channels {
        match crate::youtube::auth::probe_channel(tok).await {
            Ok(info) => {
                let need_upload = tok.has_scope(crate::tokens::SCOPE_UPLOAD);
                let warn = if need_upload { "" } else { "  [!] НЕТ SCOPE UPLOAD" };
                println!(
                    "[OK] {folder} -> {} (videoId канала: {}){warn}",
                    info.title, info.channel_id
                );
                if !need_upload {
                    bad += 1;
                } else {
                    ok += 1;
                }
            }
            Err(e) => {
                println!("[FAIL] {folder}: {e}");
                bad += 1;
            }
        }
    }
    println!("\nГотово: {ok}, проблем: {bad}");
    Ok(())
}
