use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};

use crate::config::Settings;

/// Общий контекст: корень проекта, пути, настройки, бинарники ffmpeg
#[derive(Clone)]
pub struct Context {
    pub root: PathBuf,
    pub settings: Settings,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Context {
    pub fn new(root: Option<String>) -> Result<Self> {
        let root = match root {
            Some(r) => PathBuf::from(r),
            None => default_root()?,
        };
        let root = root
            .canonicalize()
            .with_context(|| format!("корень проекта не найден: {}", root.display()))?;

        let settings = Settings::load(&root.join("settings.json"))?;
        let (ffmpeg, ffprobe) = find_ffmpeg(&root)?;

        Ok(Self { root, settings, ffmpeg, ffprobe })
    }

    pub fn people_dir(&self) -> PathBuf {
        self.root.join("People")
    }

    pub fn tokens_file(&self) -> PathBuf {
        self.root.join("tokens.json")
    }

    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    pub fn work_dir(&self) -> PathBuf {
        self.root.join(".work")
    }

    pub fn history_file(&self) -> PathBuf {
        self.root.join("history.json")
    }

    pub fn state_file(&self) -> PathBuf {
        self.root.join("state.json")
    }

    pub fn used_file(&self) -> PathBuf {
        self.root.join("used.json")
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(self.work_dir())?;
        Ok(())
    }
}

/// Корень по умолчанию: два уровня выше от бинарника.
///   <root>/ytf/target/release/ytf.exe  ->  <root>
///   <root>/ytf/ytf                     ->  <root>
fn default_root() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("не найти путь к себе")?;
    let mut dir = exe.parent().map(Path::to_path_buf);

    for _ in 0..5 {
        let Some(d) = dir else { break };
        if d.join("settings.json").exists() {
            return Ok(d);
        }
        // папка называется ytf — её родитель и есть корень
        if d.file_name().map(|n| n == "ytf").unwrap_or(false) {
            if let Some(p) = d.parent() {
                return Ok(p.to_path_buf());
            }
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    anyhow::bail!(
        "не могу определить корень проекта. Задай явно: ytf --root <путь> root/settings.json"
    )
}

/// Ищем ffmpeg: сперва рядом с проектом (`tools/ffmpeg/**`), потом в PATH.
/// Папки macos/windows/linux внутри tools/ffmpeg фильтруются по текущей ОС,
/// чтобы не подхватить бинарник чужой платформы.
fn find_ffmpeg(root: &Path) -> Result<(PathBuf, PathBuf)> {
    let mut ff = None;
    let mut fp = None;

    let want_exe = cfg!(windows);
    let this_os: &str = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    };

    let tools = root.join("tools").join("ffmpeg");
    if tools.exists() {
        for entry in walkdir::WalkDir::new(&tools).max_depth(4) {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_file() {
                continue;
            }
            // пропускаем бинарники чужих платформ
            let foreign_platform = entry
                .path()
                .components()
                .any(|c| {
                    let s = c.as_os_str().to_string_lossy();
                    matches!(s.as_ref(), "macos" | "windows" | "linux") && s != this_os
                });
            if foreign_platform {
                continue;
            }
            let name = entry.file_name().to_string_lossy();
            if want_exe {
                match name.as_ref() {
                    "ffmpeg.exe" if ff.is_none() => ff = Some(entry.path().to_path_buf()),
                    "ffprobe.exe" if fp.is_none() => fp = Some(entry.path().to_path_buf()),
                    _ => {}
                }
            } else if !name.ends_with(".exe") {
                match name.as_ref() {
                    "ffmpeg" if ff.is_none() => ff = Some(entry.path().to_path_buf()),
                    "ffprobe" if fp.is_none() => fp = Some(entry.path().to_path_buf()),
                    _ => {}
                }
            }
        }
    }

    let ff = match ff {
        Some(p) => p,
        None => which("ffmpeg").context(
            "не найден ffmpeg. Положи бинарники в tools/ffmpeg/**/bin \
             или поставь ffmpeg в PATH (macOS: brew install ffmpeg)",
        )?,
    };
    let fp = match fp {
        Some(p) => p,
        None => which("ffprobe").context("не найден ffprobe рядом с ffmpeg")?,
    };

    Ok((ff, fp))
}

fn which(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|s| s.to_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };

    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let cand = dir.join(format!("{name}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}
