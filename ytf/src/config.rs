use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Настройки канала. Лежат в `_config.json` внутри папки канала.
/// Всё опционально: если файла нет — берутся дефолты из settings.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ChannelConfig {
    /// Имя канала на YouTube (для показа). ВАЖНО: папку канала с токеном
    /// связывает ключ в tokens.json, а не это поле.
    pub title: Option<String>,

    /// Отключить канал целиком (не публиковать, не рендерить)
    pub enabled: bool,

    /// `false` — публиковать сразу (`public`),
    /// `true`  — отложить на publish_at (см. settings.publish)
    pub schedule: bool,

    /// `random`    — случайный трек и случайная картинка из пула
    /// `single`    — всегда single_music + single_pictures (релизный режим)
    /// `loop`      — вместо картинки крутится случайное видео из pictures/
    pub mode: RenderMode,

    /// Режим релиза: конкретный трек
    pub single_music: Option<String>,
    /// Режим релиза: конкретные картинки (по кругу, если несколько)
    pub single_pictures: Vec<String>,

    /// Не рандомизировать теги/описание для этого канала
    pub no_randomize: bool,

    /// Переопределение времени публикации (HH:MM, местное время канала)
    pub publish_at: Option<String>,

    /// Категория YouTube. 10 = Music
    pub category_id: String,

    /// Пометить видео как «для детей»
    pub made_for_kids: bool,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            title: None,
            enabled: true,
            schedule: true,
            mode: RenderMode::Random,
            single_music: None,
            single_pictures: Vec::new(),
            no_randomize: false,
            publish_at: None,
            category_id: "10".into(),
            made_for_kids: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderMode {
    /// случайная картинка + случайный трек
    Random,
    /// один конкретный трек + конкретные картинки (релиз)
    Single,
    /// вместо картинки — случайное видео из pictures/, зацикленное
    Loop,
}

/// Глобальные настройки рендера
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct RenderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub crf: u32,
    pub preset: String,
    pub audio_bitrate: String,
    pub audio_rate_hz: u32,
    /// «Выход из темноты» — секунды. 0 = выключено
    pub fade_from_black_secs: f64,
    /// Бесшовность: строить палиндром (прямой+обратный) для режима loop
    pub seamless_loop: bool,
    /// Обрезать кадр по центру (true) или вписать с чёрными полями (false)
    pub crop_to_fill: bool,
    /// Порог «кадр чёрный»: 90-й перцентиль яркости ниже этого = тёмный
    pub black_frame_threshold: u8,
    /// Порог «кадр плоский»: разброс p90-p10 ниже этого = однородный
    pub black_frame_flatness: u8,
    /// Сколько секунд от начала брать кадр для обложки, 0 = середина
    pub thumb_at_secs: Option<f64>,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30,
            crf: 22,
            preset: "veryfast".into(),
            audio_bitrate: "160k".into(),
            audio_rate_hz: 44_100,
            fade_from_black_secs: 10.0,
            seamless_loop: true,
            crop_to_fill: true,
            black_frame_threshold: 30,
            black_frame_flatness: 12,
            thumb_at_secs: None,
        }
    }
}

/// Глобальные настройки публикации
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct PublishConfig {
    /// Час публикации в местном времени (0-23). 22 = 22:00 по Омску
    pub hour: u32,
    pub minute: u32,
    /// Часовой пояс каналов. None = системный
    pub timezone: Option<String>,
    /// Сколько воркеров рендера. None = авто (ядра/2)
    pub workers: Option<usize>,
    /// Сколько попыток загрузки
    pub upload_retries: u32,
    /// Сколько ждать между попытками, секунды (умножается на 2^n)
    pub retry_base_delay_secs: u64,
    /// Заливать свою обложку, если YouTube взял чёрную
    pub fix_black_thumbnail: bool,
    /// Удалять файл сразу после успешной загрузки
    pub delete_after_upload: bool,
    /// Сколько дней подряд проверять занятость (1 = только сегодня)
    pub horizon_days: i64,
    /// Рендерить в обход диска (ffmpeg -> stdout -> upload)
    pub stream_to_upload: bool,
}

impl Default for PublishConfig {
    fn default() -> Self {
        Self {
            hour: 22,
            minute: 0,
            timezone: Some("Asia/Omsk".into()),
            workers: None,
            upload_retries: 3,
            retry_base_delay_secs: 5,
            fix_black_thumbnail: true,
            delete_after_upload: true,
            horizon_days: 1,
            stream_to_upload: true,
        }
    }
}

/// Корневой `settings.json`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Settings {
    pub render: RenderConfig,
    pub publish: PublishConfig,
    /// Ограничения API
    pub api: ApiConfig,
    /// Ключ для отчётов/логов
    pub log_level: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ApiConfig {
    /// Бакет videos.insert в сутки (официально 100)
    pub insert_bucket_per_day: u32,
    /// Общий бакет в сутки на всё остальное
    pub general_units_per_day: u32,
    /// Порог квоты, при котором перестаём брать новые задачи (%)
    pub quota_warn_percent: u32,
    /// Максимум параллельных загрузок
    pub max_parallel_uploads: usize,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            insert_bucket_per_day: 100,
            general_units_per_day: 10_000,
            quota_warn_percent: 85,
            max_parallel_uploads: 4,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("не прочитать {}", path.display()))?;
        let s: Settings = serde_json::from_str(&raw)
            .with_context(|| format!("не распарсить {}", path.display()))?;
        Ok(s)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(path, raw).with_context(|| format!("не записать {}", path.display()))?;
        Ok(())
    }
}

/// Канал, найденный на диске: путь + конфиг + токен
#[derive(Debug, Clone)]
pub struct Channel {
    /// Имя папки канала — это ключ в tokens.json
    pub folder: String,
    /// Человек (родительская папка)
    pub person: String,
    pub path: PathBuf,
    pub config: ChannelConfig,
    pub music: Vec<PathBuf>,
    pub pictures: Vec<PathBuf>,
    pub videos: Vec<PathBuf>,
    pub names: Vec<PathBuf>,
    pub descriptions: Vec<PathBuf>,
    pub tags_video: Vec<PathBuf>,
    pub tags_desc: Vec<PathBuf>,
}

impl Channel {
    pub fn label(&self) -> String {
        self.config
            .title
            .clone()
            .unwrap_or_else(|| self.folder.clone())
    }

    pub fn is_ready(&self) -> bool {
        !self.music.is_empty() && (!self.pictures.is_empty() || !self.videos.is_empty())
    }
}
