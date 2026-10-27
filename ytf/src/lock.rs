//! Межпроцессные блокировки.
//!
//! Нужны, чтобы два окна программы (два `ytf web` на разных портах) могли
//! работать с одной папкой: не терять записи журнала, не выбирать один и
//! тот же трек и не публиковать один канал дважды.
//!
//! Блокировка — это каталог: `create_dir` атомарен и на Windows, и на Unix,
//! а «зависший» лок определяется по времени изменения каталога.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct StateLock {
    path: PathBuf,
}

impl StateLock {
    /// `timeout` — сколько ждать освобождения, `stale_after` — когда лок
    /// считать брошенным (процесс упал, не сняв его).
    pub fn acquire(path: &Path, timeout: Duration, stale_after: Duration) -> Result<Self> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let start = Instant::now();
        loop {
            match std::fs::create_dir(path) {
                Ok(()) => return Ok(Self { path: path.to_path_buf() }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if is_stale(path, stale_after) {
                        let _ = std::fs::remove_dir_all(path);
                        continue;
                    }
                    if start.elapsed() > timeout {
                        anyhow::bail!("файл занят другим окном программы");
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => {
                    return Err(e).with_context(|| format!("не смог создать лок {}", path.display()))
                }
            }
        }
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn is_stale(path: &Path, stale_after: Duration) -> bool {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .map(|age| age > stale_after)
        .unwrap_or(false)
}

/// Короткий лок для файла состояния (`history.json`, `used.json`).
/// Критическая секция — миллисекунды, поэтому 20 секунд ожидания с запасом.
pub fn state_lock(path: &Path) -> Result<StateLock> {
    let name = format!(
        "{}.lock",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    StateLock::acquire(
        &path.with_file_name(name),
        Duration::from_secs(20),
        Duration::from_secs(120),
    )
}

/// Лок на канал: не даёт двум окнам публиковать один и тот же канал.
/// Держится всю публикацию (рендер + загрузка), поэтому «протухает» только
/// через 3 часа — если процесс упал, лок можно снять удалением `.work/locks`.
pub fn channel_lock(root: &Path, key: &str) -> Result<StateLock> {
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        format!("{:016x}", h.finish())
    };
    StateLock::acquire(
        &root.join(".work").join("locks").join(format!("ch-{hash}.lock")),
        Duration::from_secs(3),
        Duration::from_secs(3 * 3600),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn temp_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ytf-lock-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn lock_excludes_and_releases() {
        let dir = temp_dir("excl");
        let path = dir.join("state.lock");

        let first = StateLock::acquire(&path, Duration::from_secs(1), Duration::from_secs(60)).unwrap();
        let err = StateLock::acquire(&path, Duration::from_millis(80), Duration::from_secs(60))
            .unwrap_err();
        assert!(err.to_string().contains("занят"), "неожиданная ошибка: {err}");

        drop(first);
        let _second = StateLock::acquire(&path, Duration::from_secs(1), Duration::from_secs(60)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
