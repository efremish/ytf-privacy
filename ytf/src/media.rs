use anyhow::{bail, Context as _, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::process::{ChildStdout, Command};

use crate::config::RenderConfig;
use crate::settings::Context;

/// Что показываем в кадре
#[derive(Debug, Clone)]
pub enum Visual {
    /// статичная картинка
    Still(PathBuf),
    /// короткое видео, зацикленное под бит
    Looped(PathBuf),
}

/// Что получилось на выходе рендера
pub enum Rendered {
    /// файл на диске
    File(PathBuf),
    /// видео целиком в памяти (проверенный путь: заливка одним куском)
    Memory(Vec<u8>),
    /// поток из stdout ffmpeg (диск не трогаем вообще)
    Stream(ChildStdout),
}

pub struct Renderer {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    cfg: RenderConfig,
    work: PathBuf,
    /// кэш палиндромов: путь клипа -> путь готового палиндрома
    pal_cache: std::sync::Mutex<HashMap<PathBuf, PathBuf>>,
}

impl Renderer {
    pub fn new(ctx: &Context) -> Result<Arc<Self>> {
        ctx.ensure_dirs()?;
        Ok(Arc::new(Self {
            ffmpeg: ctx.ffmpeg.clone(),
            ffprobe: ctx.ffprobe.clone(),
            cfg: ctx.settings.render.clone(),
            work: ctx.work_dir(),
            pal_cache: std::sync::Mutex::new(HashMap::new()),
        }))
    }

    pub fn cfg(&self) -> &RenderConfig {
        &self.cfg
    }

    pub fn ffmpeg_path(&self) -> &Path {
        &self.ffmpeg
    }

    // ── probe ────────────────────────────────────────────────

    /// Длительность медиа в секундах
    pub async fn duration(&self, path: &Path) -> Result<f64> {
        let out = Command::new(&self.ffprobe)
            .args(["-v", "error", "-show_entries", "format=duration"])
            .args(["-of", "default=nw=1:nk=1"])
            .arg(path)
            .output()
            .await
            .with_context(|| format!("не запустился ffprobe для {}", path.display()))?;

        let s = String::from_utf8_lossy(&out.stdout);
        let v: f64 = s
            .trim()
            .parse()
            .with_context(|| format!("не распознал длительность: {:?}", s.trim()))?;
        Ok(v)
    }

    // ── палиндром для бесшовного цикла ──────────────────────

    /// Прямой+обратный ход: 8 сек → 16 сек, конец совпадает с началом,
    /// поэтому `-stream_loop` не даёт видимого скачка.
    pub async fn palindrome(&self, clip: &Path) -> Result<PathBuf> {
        if let Some(hit) = self
            .pal_cache
            .lock()
            .ok()
            .and_then(|m| m.get(clip).cloned())
        {
            if hit.exists() {
                return Ok(hit);
            }
        }

        let name = format!(
            "pal_{:x}.mp4",
            fnv(clip.to_string_lossy().as_bytes())
        );
        let dst = self.work.join(name);

        let st = tokio::process::Command::new(&self.ffmpeg)
            .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(clip)
            .args(["-filter_complex", "[0:v]split[a][b];[b]reverse[r];[a][r]concat=n=2:v=1[v]"])
            .args(["-map", "[v]", "-an", "-c:v", "libx264", "-preset", &self.cfg.preset])
            .args(["-crf", &self.cfg.crf.to_string(), "-pix_fmt", "yuv420p"])
            .arg(&dst)
            .status()
            .await?;

        if !st.success() {
            bail!("ffmpeg: палиндром не собрался (exit {st}) для {}", clip.display());
        }
        if let Ok(mut m) = self.pal_cache.lock() {
            m.insert(clip.to_path_buf(), dst.clone());
        }
        Ok(dst)
    }

    // ── основной рендер ──────────────────────────────────────

    /// Собрать ffmpeg-аргументы для рендера.
    /// `dest` = None -> стрим в stdout
    pub async fn build_args(
        &self,
        music: &Path,
        visual: &Visual,
        dest: Option<&Path>,
    ) -> Result<Vec<String>> {
        let c = &self.cfg;
        let mut a: Vec<String> = vec![
            "-y".into(), "-hide_banner".into(), "-loglevel".into(), "error".into(),
        ];

        match visual {
            Visual::Looped(clip) => {
                a.push("-stream_loop".into()); a.push("-1".into());
                a.push("-i".into()); a.push(clip.to_string_lossy().into_owned());
            }
            Visual::Still(img) => {
                a.push("-loop".into()); a.push("1".into());
                a.push("-i".into()); a.push(img.to_string_lossy().into_owned());
            }
        }
        a.push("-i".into()); a.push(music.to_string_lossy().into_owned());

        // кадр: вписать с обрезкой или с чёрными полями
        let geom = if c.crop_to_fill {
            format!(
                "scale={w}:{h}:force_original_aspect_ratio=increase:flags=lanczos,crop={w}:{h}",
                w = c.width, h = c.height
            )
        } else {
            format!(
                "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos,\
                 pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:black",
                w = c.width, h = c.height
            )
        };

        let mut vf = geom;
        if c.fade_from_black_secs > 0.0 {
            // «выход из темноты»: кадр проявляется из чёрного
            vf.push_str(&format!(
                ",fade=t=in:st=0:d={}:color=black",
                trim_num(c.fade_from_black_secs)
            ));
        }
        vf.push_str(",format=yuv420p");

        a.push("-vf".into()); a.push(vf);
        a.push("-r".into()); a.push(c.fps.to_string());
        a.push("-c:v".into()); a.push("libx264".into());
        a.push("-preset".into()); a.push(c.preset.clone());
        a.push("-crf".into()); a.push(c.crf.to_string());
        a.push("-c:a".into()); a.push("aac".into());
        a.push("-b:a".into()); a.push(c.audio_bitrate.clone());
        a.push("-ar".into()); a.push(c.audio_rate_hz.to_string());
        a.push("-shortest".into());

        match dest {
            Some(p) => {
                a.push("-movflags".into()); a.push("+faststart".into());
                a.push(p.to_string_lossy().into_owned());
            }
            None => {
                // фрагментированный MP4 — иначе moov atom пишется в конец
                // и в поток отдать нельзя
                a.push("-movflags".into());
                a.push("frag_keyframe+empty_moov+default_base_moof".into());
                a.push("-f".into()); a.push("mp4".into());
                a.push("pipe:1".into());
            }
        }
        Ok(a)
    }

    /// Рендер в файл
    pub async fn render_to_file(
        &self,
        music: &Path,
        visual: &Visual,
        dest: &Path,
    ) -> Result<()> {
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let args = self.build_args(music, visual, Some(dest)).await?;
        let out = Command::new(&self.ffmpeg)
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await
            .with_context(|| "не запустился ffmpeg")?;

        if !out.status.success() {
            bail!(
                "ffmpeg завершился с {}:\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        if !dest.exists() || std::fs::metadata(dest)?.len() < 1024 {
            bail!("ffmpeg отработал, но файл пустой: {}", dest.display());
        }
        Ok(())
    }

    /// Рендер в память: ffmpeg пишет в stdout, мы читаем до конца и
    /// ОБЯЗАТЕЛЬНО проверяем exit code. Потом заливаем одним куском —
    /// этот путь доказанно проходит обработку YouTube.
    ///
    /// (Стриминг напрямую в upload давал «Processing abandoned»: часть
    /// байтов терялась между stdout ffmpeg и HTTP-чанками.)
    pub async fn render_to_memory(
        &self,
        music: &Path,
        visual: &Visual,
    ) -> Result<Rendered> {
        let args = self.build_args(music, visual, None).await?;
        let mut child = Command::new(&self.ffmpeg)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| "не запустился ffmpeg (memory)")?;

        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("ffmpeg не дал stdout"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("ffmpeg не дал stderr"))?;

        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).await?;
        // вычитать stderr до конца, чтобы получить текст ошибки
        let mut err_buf = Vec::new();
        let _ = stderr.read_to_end(&mut err_buf).await;
        let status = child.wait().await?;

        if !status.success() {
            let s = String::from_utf8_lossy(&err_buf);
            bail!("ffmpeg (memory) завершился с {status}: {}", s.trim());
        }
        if buf.len() < 1024 {
            bail!("ffmpeg (memory) выдал {} байт — пустой результат", buf.len());
        }
        tracing::info!("рендер в память: {} МБ", buf.len() / 1_048_576);
        Ok(Rendered::Memory(buf))
    }

    /// Рендер в stdout — ноль записи на диск (для экономии SSD)
    pub async fn render_to_stream(
        &self,
        music: &Path,
        visual: &Visual,
    ) -> Result<Rendered> {
        let args = self.build_args(music, visual, None).await?;
        let mut child = Command::new(&self.ffmpeg)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| "не запустился ffmpeg (stream)")?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("ffmpeg не дал stdout"))?;

        // stderr надо вычитывать, иначе труба забьётся и ffmpeg встанет
        tokio::spawn(async move {
            if let Some(mut e) = child.stderr.take() {
                let mut buf = Vec::new();
                let _ = e.read_to_end(&mut buf).await;
                if !buf.is_empty() {
                    let s = String::from_utf8_lossy(&buf);
                    if s.trim().len() > 400 {
                        tracing::debug!("ffmpeg stderr: {}", s.trim());
                    }
                }
            }
        });

        Ok(Rendered::Stream(stdout))
    }

    // ── кадры и обложки ──────────────────────────────────────

    /// Вырезать кадр в jpg. `at` = секунды, None = середина
    pub async fn extract_frame(&self, video: &Path, at: Option<f64>, dst: &Path) -> Result<()> {
        let at = match at {
            Some(s) => s,
            None => (self.duration(video).await? / 2.0).max(0.5),
        };
        let st = Command::new(&self.ffmpeg)
            .args(["-y", "-hide_banner", "-loglevel", "error", "-ss"])
            .arg(format!("{at:.3}"))
            .arg("-i")
            .arg(video)
            .args(["-frames:v", "1", "-q:v", "2"])
            .arg(dst)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?;
        if !st.success() || !dst.exists() {
            bail!("не удалось вырезать кадр на {at:.1} сек");
        }
        Ok(())
    }

    /// Кадр считается чёрным, если он одновременно:
    ///   1) тёмный  — 90-й перцентиль яркости ниже `max_luma`
    ///   2) плоский  — разброс p90-p10 меньше `max_spread`
    ///
    /// Одного среднего мало: JPEG даёт на чёрном кадре постоянный сдвиг
    /// (в нашем случае ровно 16 при нуле). А вот у настоящей картинки
    /// разброс яркости большой, даже если она тёмная.
    pub fn frame_is_black(jpeg: &[u8], max_luma: u8, max_spread: u8) -> bool {
        let Some((p10, p90)) = luma_percentiles(jpeg) else { return false };
        let dark = p90 <= max_luma as u16;
        let flat = p90.saturating_sub(p10) <= max_spread as u16;
        dark && flat
    }
}

/// (p10, p90) яркости кадра. None, если не декодируется JPEG
pub fn luma_percentiles(jpeg: &[u8]) -> Option<(u16, u16)> {
    let img = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).ok()?;
    let g = img.to_luma8();
    let px = g.as_raw();
    if px.is_empty() {
        return None;
    }
    // гистограмма вместо сортировки — 256 корзин
    let mut hist = [0u32; 256];
    for v in px {
        hist[*v as usize] += 1;
    }
    let total = px.len() as u64;
    let at = |q: f64| -> u16 {
        let target = (total as f64 * q) as u64;
        let mut acc = 0u64;
        for (i, c) in hist.iter().enumerate() {
            acc += *c as u64;
            if acc > target {
                return i as u16;
            }
        }
        255
    };
    Some((at(0.10), at(0.90)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    /// 2x2 пикселя: [y][x] = [r, g, b]
    fn jpeg_of(px: [[[u8; 3]; 2]; 2]) -> Vec<u8> {
        let img = ImageBuffer::from_fn(2, 2, |x, y| Rgb(px[y as usize][x as usize]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn чёрный_кадр_считается_чёрным() {
        let j = jpeg_of([[[0, 0, 0], [0, 0, 0]], [[0, 0, 0], [0, 0, 0]]]);
        assert!(Renderer::frame_is_black(&j, 30, 12));
    }

    #[test]
    fn яркий_кадр_не_чёрный() {
        let j = jpeg_of([[[200, 180, 160], [210, 190, 170]], [[190, 170, 150], [205, 185, 165]]]);
        assert!(!Renderer::frame_is_black(&j, 30, 12));
    }

    #[test]
    fn тёмная_но_детальная_картинка_не_чёрная() {
        // почти тёмная, но с контрастом — обложку менять не надо
        let j = jpeg_of([[[10, 10, 12], [90, 80, 70]], [[15, 15, 18], [120, 100, 85]]]);
        let (p10, p90) = luma_percentiles(&j).unwrap();
        assert!(p90 > 30 || p90 - p10 > 12, "ожидали разброс, got p10={p10} p90={p90}");
        assert!(!Renderer::frame_is_black(&j, 30, 12));
    }

    #[test]
    fn плоский_серый_кадр_не_считается_чёрным_если_светлый() {
        let j = jpeg_of([[[80, 80, 80], [80, 80, 80]], [[80, 80, 80], [80, 80, 80]]]);
        assert!(!Renderer::frame_is_black(&j, 30, 12));
    }

    #[test]
    fn мусор_не_считается_чёрным() {
        assert!(!Renderer::frame_is_black(b"not a jpeg", 30, 12));
    }
}

fn trim_num(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn fnv(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}
