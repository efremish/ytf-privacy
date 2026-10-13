//! Логи: одновременно в консоль И в файл `logs/ytf-YYYY-MM-DD.log`.
//!
//! Ротация — смена файла при переходе через полночь (важно для `ytf web`,
//! который живёт сутками). Старые файлы старше 14 дней удаляются при старте.
//! Уровень — через env `YTF_LOG` (по умолчанию `info`). Поле `log_level`
//! в settings.json оставлено для совместимости, но не используется.

use std::path::PathBuf;
use std::sync::Mutex;

const KEEP_DAYS: i64 = 14;

struct DayFile {
    dir: PathBuf,
    date: String,
    file: Option<std::fs::File>,
}

impl DayFile {
    fn new(dir: PathBuf) -> Self {
        Self { dir, date: String::new(), file: None }
    }

    /// Дописать кусок текста в файл текущего дня. Ошибки игнорируем —
    /// логгер не должен ронять программу.
    fn append(&mut self, buf: &[u8]) {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        if self.date != today || self.file.is_none() {
            match open_day(&self.dir, &today) {
                Ok(f) => {
                    self.date = today;
                    self.file = Some(f);
                }
                Err(_) => return,
            }
        }
        if let Some(f) = self.file.as_mut() {
            use std::io::Write;
            let _ = f.write_all(buf);
            let _ = f.flush();
        }
    }
}

fn open_day(dir: &PathBuf, day: &str) -> std::io::Result<std::fs::File> {
    std::fs::create_dir_all(dir)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("ytf-{day}.log")))
}

/// Пишет в stdout и одновременно в файл дня. ANSI-краски отключены
/// (в файле им не место), таймстамп есть всегда.
struct Tee {
    file: std::sync::Arc<Mutex<DayFile>>,
}

impl std::io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stdout().write_all(buf);
        let _ = std::io::stdout().flush();
        if let Ok(mut f) = self.file.lock() {
            f.append(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = std::io::stdout().flush();
        Ok(())
    }
}

/// Удалить логи старше KEEP_DAYS. Имя файла — ytf-YYYY-MM-DD.log,
/// дату вытаскиваем прямо из имени, парсить содержимое не надо.
fn prune_old(dir: &PathBuf) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let today = chrono::Local::now().date_naive();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(day) = name
            .strip_prefix("ytf-")
            .and_then(|s| s.strip_suffix(".log"))
        else {
            continue;
        };
        let Ok(d) = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d") else { continue };
        if (today - d).num_days() > KEEP_DAYS {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Подключить логирование. Звать один раз, после Context::new (нужен корень).
/// Возвращает путь к файлу сегодняшнего лога — показываем его пользователю.
pub fn init(root: &std::path::Path) -> PathBuf {
    let dir = root.join("logs");
    let _ = std::fs::create_dir_all(&dir);
    prune_old(&dir);

    let tee = Tee { file: std::sync::Arc::new(Mutex::new(DayFile::new(dir.clone()))) };
    let filter = tracing_subscriber::EnvFilter::try_from_env("YTF_LOG")
        .unwrap_or_else(|_| "info".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(false)
        .with_writer(move || tee.clone())
        .try_init();
    // файл дня «прогреваем», чтобы путь точно существовал к моменту показа
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let _ = open_day(&dir, &today);

    dir.join(format!("ytf-{today}.log"))
}

impl Clone for Tee {
    fn clone(&self) -> Self {
        Self { file: self.file.clone() }
    }
}
