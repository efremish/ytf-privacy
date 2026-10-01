use anyhow::{Context as _, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::config::{Channel, ChannelConfig, RenderMode};
use crate::settings::Context;

const AUDIO_EXT: &[&str] = &["mp3", "wav", "flac", "m4a", "aac", "ogg"];
const IMAGE_EXT: &[&str] = &["jpg", "jpeg", "png", "webp"];
const VIDEO_EXT: &[&str] = &["mp4", "mov", "mkv", "webm"];

/// Просканировать `People/<человек>/<канал>/` и собрать все каналы
pub fn scan_all(ctx: &Context) -> Result<Vec<Channel>> {
    let people = ctx.people_dir();
    if !people.exists() {
        anyhow::bail!(
            "нет папки {}. Создай структуру People/<человек>/<канал>/",
            people.display()
        );
    }

    let mut out = Vec::new();
    for person in sorted_dirs(&people)? {
        for chan_dir in sorted_dirs(&person)? {
            match load_channel(&person, &chan_dir) {
                Ok(c) => out.push(c),
                Err(e) => tracing::warn!("{}/{}: пропущен — {:#}", person_name(&person), chan_name(&chan_dir), e),
            }
        }
    }
    Ok(out)
}

pub fn scan_one(ctx: &Context, folder: &str) -> Result<Channel> {
    let all = scan_all(ctx)?;
    all.into_iter()
        .find(|c| c.folder.eq_ignore_ascii_case(folder))
        .with_context(|| format!("канал «{folder}» не найден в People/"))
}

fn load_channel(person: &Path, chan_dir: &Path) -> Result<Channel> {
    let folder = chan_name(chan_dir);
    let person = person_name(person);

    let config = match chan_dir.join("_config.json").exists() {
        true => {
            let raw = std::fs::read_to_string(chan_dir.join("_config.json"))?;
            serde_json::from_str::<ChannelConfig>(&raw)
                .with_context(|| format!("плохой _config.json в {}", chan_dir.display()))?
        }
        false => ChannelConfig::default(),
    };

    let mut music = files_in(&chan_dir.join("music"), AUDIO_EXT);
    let mut pictures = files_in(&chan_dir.join("pictures"), IMAGE_EXT);
    let videos = files_in(&chan_dir.join("pictures"), VIDEO_EXT);

    // В режиме single картинки берём из _config.json, а не из пула
    if config.mode == RenderMode::Single && !config.single_pictures.is_empty() {
        pictures = config
            .single_pictures
            .iter()
            .map(|n| chan_dir.join("pictures").join(n))
            .filter(|p| p.exists())
            .collect();
    }
    if config.mode == RenderMode::Single {
        if let Some(m) = &config.single_music {
            let p = chan_dir.join("music").join(m);
            if p.exists() {
                music = vec![p];
            }
        }
    }

    Ok(Channel {
        folder,
        person,
        path: chan_dir.to_path_buf(),
        config,
        music,
        pictures,
        videos,
        names: files_in(&chan_dir.join("names"), &["txt"]),
        descriptions: files_in(&chan_dir.join("description"), &["txt"]),
        tags_video: files_in(&chan_dir.join("tags_for_video"), &["txt"]),
        tags_desc: files_in(&chan_dir.join("tags_for_description"), &["txt"]),
    })
}

pub fn print_library(ctx: &Context) -> Result<()> {
    let channels = scan_all(ctx)?;
    println!("Каналов найдено: {}\n", channels.len());

    for c in &channels {
        let mark = if c.is_ready() { "+" } else { "!" };
        println!("[{mark}] {} / {}", c.person, c.folder);
        println!("    канал на YT : {}", c.label());
        println!("    режим       : {:?}", c.config.mode);
        println!("    треков      : {}", c.music.len());
        println!("    картинок    : {}", c.pictures.len());
        println!("    видео       : {}", c.videos.len());
        if let Some(m) = &c.config.single_music {
            println!("    релиз-трек  : {m}");
        }
        if let Some(t) = &c.config.publish_at {
            println!("    время       : {t}");
        }
        if !c.is_ready() {
            println!("    ВНИМАНИЕ    : нет {}",
                if c.music.is_empty() { "треков" }
                else { "картинок/видео" });
        }
        println!();
    }

    // Сводка
    let total_music: usize = channels.iter().map(|c| c.music.len()).sum();
    let total_pics: usize = channels.iter().map(|c| c.pictures.len()).sum();
    let total_vids: usize = channels.iter().map(|c| c.videos.len()).sum();
    println!("Итого: треков {total_music}, картинок {total_pics}, видео {total_vids}");

    let mut by_person: HashMap<&str, usize> = HashMap::new();
    for c in &channels {
        *by_person.entry(c.person.as_str()).or_default() += 1;
    }
    if by_person.len() > 1 {
        println!("Людей: {}", by_person.len());
        for (p, n) in by_person {
            println!("  {p}: {n} каналов");
        }
    }
    Ok(())
}

fn sorted_dirs(p: &Path) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(p)
        .with_context(|| format!("не прочитать {}", p.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !is_hidden(p))
        .collect();
    v.sort_by_key(|p| folder_key(p));
    Ok(v)
}

fn files_in(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    if !dir.exists() {
        return Vec::new();
    }
    let mut v: Vec<PathBuf> = WalkDir::new(dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .map(|e| exts.contains(&e.as_str()))
                .unwrap_or(false)
        })
        .collect();
    v.sort_by_key(|p| folder_key(p));
    v
}

fn is_hidden(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.') || n.starts_with('_') || n == "__pycache__")
        .unwrap_or(false)
}

fn folder_key(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase()
}

fn person_name(p: &Path) -> String {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string()
}

fn chan_name(p: &Path) -> String {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string()
}
