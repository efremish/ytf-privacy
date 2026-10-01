use anyhow::{Context as _, Result};
use std::path::Path;
use std::rc::Rc;

use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

use crate::config::Channel;

/// Разобранная информация из имени трека
#[derive(Debug, Clone, Default)]
pub struct TrackInfo {
    pub beat_name: String,
    pub bpm: Option<String>,
    pub key: Option<String>,
}

/// `BABY 104BPM D MIN BOUNCE KID` -> beat="BABY", bpm="104 BPM", key="D MIN"
pub fn parse_track(file_stem: &str) -> TrackInfo {
    let raw = file_stem;

    // Имя бита — всё до первой цифры
    let beat_raw = match raw.find(|c: char| c.is_ascii_digit()) {
        Some(i) => raw[..i].trim(),
        None => raw.trim(),
    };
    let beat_name = beat_raw.replace('_', " ").trim().to_string();

    // BPM
    let bpm = {
        let lower = raw.to_ascii_lowercase();
        let b: Vec<char> = lower.chars().collect();
        let mut found = None;
        for i in 0..b.len().saturating_sub(3) {
            if b[i].is_ascii_digit() {
                let mut j = i;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                let rest: String = b[j..].iter().take(3).collect();
                let rest = rest.trim_start();
                if rest.starts_with("bpm") {
                    let digits: String = b[i..j].iter().collect();
                    found = Some(format!("{digits} BPM"));
                    break;
                }
            }
        }
        found
    };

    // Тональность: A-G + #/b + min/major (в любом регистре, с разделителем)
    let key = {
        let norm = raw.replace('\u{0421}', "C").replace('\u{0441}', "c");
        let chars: Vec<char> = norm.chars().collect();
        let mut found = None;
        for i in 0..chars.len() {
            let n = chars[i];
            if !(n.is_ascii_uppercase() && ('A'..='G').contains(&n)) {
                continue;
            }
            let mut j = i + 1;
            if j < chars.len() && (chars[j] == '#' || chars[j] == 'b') {
                j += 1;
            }
            // разделитель между нотой и словом: "D MIN", "G_Maj", "A#minor"
            let mut k = j;
            while k < chars.len() && (chars[k] == ' ' || chars[k] == '_' || chars[k] == '-') {
                k += 1;
            }
            let tail: String = chars[k..].iter().take(8).collect();
            let tl = tail.to_ascii_lowercase();
            // граница слова: после "min"/"maj" не буква (или это "minor"/"major")
            let word = |pfx: &str| -> bool {
                if !tl.starts_with(pfx) {
                    return false;
                }
                match tl.chars().nth(pfx.len()) {
                    None => true,
                    Some(c) => !c.is_ascii_alphabetic(),
                }
            };
            let is_minor = tl.starts_with("minor") || word("min");
            let is_major = tl.starts_with("major") || word("maj");
            if is_minor {
                found = Some(format!("{} MIN", format_note(n, chars.get(i + 1))));
                break;
            } else if is_major {
                found = Some(format!("{} MAJ", format_note(n, chars.get(i + 1))));
                break;
            }
        }
        found
    };

    TrackInfo { beat_name, bpm, key }
}

fn format_note(note: char, accidental: Option<&char>) -> String {
    match accidental {
        Some('#') => format!("{note}#"),
        Some('b') => format!("{note}b"),
        _ => note.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn тональности_в_разных_форматах() {
        assert_eq!(parse_track("BABY 104BPM D MIN BOUNCE KID").key.as_deref(), Some("D MIN"));
        assert_eq!(parse_track("deserve_89bpm_Gmin_bounce kid").key.as_deref(), Some("G MIN"));
        assert_eq!(parse_track("backrooms_104bpm_Cminor_bounce kid").key.as_deref(), Some("C MIN"));
        assert_eq!(parse_track("paradise_109bpm_A#minor_bounce kid").key.as_deref(), Some("A# MIN"));
        assert_eq!(parse_track("INTENSE 125BPM G Maj BOUNCE KID").key.as_deref(), Some("G MAJ"));
        assert_eq!(parse_track("LATE NIGHT 115BPM E Maj BOUNCE KID").key.as_deref(), Some("E MAJ"));
        assert_eq!(parse_track("home_100bpm_Eminor_bounce kid").key.as_deref(), Some("E MIN"));
    }

    #[test]
    fn bpm_парсится() {
        assert_eq!(parse_track("BABY 104BPM D MIN").bpm.as_deref(), Some("104 BPM"));
        assert_eq!(parse_track("deserve_89bpm_Gmin").bpm.as_deref(), Some("89 BPM"));
    }
}

pub struct Metadata {
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub track: TrackInfo,
}

/// Собрать заголовок/описание/теги.
///
/// `randomize == false` — детерминированно: первый шаблон, порядок тегов
/// как в файле, ничего не выкидываем. Только когда жмём кнопку.
pub fn build(ch: &Channel, music: &Path, randomize: bool) -> Result<Metadata> {
    let stem = music
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("UNTITLED");
    let track = parse_track(stem);

    // сид — чтобы при randomize результат был воспроизводим для этого трека
    let seed = fnv(stem.as_bytes()) ^ (track.beat_name.len() as u64);
    let mut rng = StdRng::seed_from_u64(seed);

    let pick = |items: &[std::path::PathBuf], rng: &mut StdRng| -> Option<std::path::PathBuf> {
        if items.is_empty() {
            return None;
        }
        if randomize {
            Some(items[rng.gen_range(0..items.len())].clone())
        } else {
            Some(items[0].clone())
        }
    };

    // ── Заголовок ──
    let mut title_template = ch.label();
    if let Some(f) = pick(&ch.names, &mut rng) {
        if let Ok(text) = std::fs::read_to_string(&f) {
            let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
            if !lines.is_empty() {
                let idx = if randomize { rng.gen_range(0..lines.len()) } else { 0 };
                title_template = lines[idx].to_string();
            }
        }
    }

    // ── Описание ──
    let mut description = format!("\u{1F3B5} {}\n\n#TypeBeat #MusicProduction", ch.label());
    if let Some(f) = pick(&ch.descriptions, &mut rng) {
        if let Ok(text) = std::fs::read_to_string(&f) {
            let t = text.trim();
            if !t.is_empty() {
                description = t.to_string();
            }
        }
    }

    // ── Подстановки ──
    if !track.beat_name.is_empty() {
        title_template = title_template.replace("NAMETRACK", &track.beat_name);
    }
    let title = if !track.beat_name.is_empty() && title_template.contains('~') {
        let head = title_template.split('~').next().unwrap_or("").trim();
        format!("{head} ~ {}", track.beat_name)
    } else {
        title_template.clone()
    };

    if !track.beat_name.is_empty() {
        description = description
            .replace("NAME_XXX", &track.beat_name)
            .replace("NAMETRACK", &track.beat_name);
    }
    if let Some(bpm) = &track.bpm {
        description = description.replace("BPM_XXX", bpm);
    }
    if let Some(k) = &track.key {
        description = description.replace("KEY_XXX", k);
    }

    // первая строка описания = заголовок
    let title_line = title.clone();
    let mut it = description.splitn(2, '\n');
    let first = it.next().unwrap_or("");
    let rest = it.next().unwrap_or("");
    description = format!("{title_line}\n{rest}");

    // ── Теги ──
    let tags = {
        let src = if ch.tags_video.is_empty() { &ch.tags_desc } else { &ch.tags_video };
        let mut t = read_tags(pick(src, &mut rng).as_deref(), &mut rng, randomize);
        if t.is_empty() {
            t = vec!["type beat".into(), "music production".into(), "instrumental".into()];
        }
        t
    };

    // теги в описание
    {
        let mut t = read_tags(pick(&ch.tags_desc, &mut rng).as_deref(), &mut rng, randomize);
        if randomize {
            shuffle_and_drop(&mut t, 0.05, 0.30, &mut rng);
        }
        if !t.is_empty() {
            description = format!("{}\n\n{}", description.trim_end(), t.join(", "));
        }
    }

    Ok(Metadata { title, description, tags, track })
}

fn read_tags(path: Option<&Path>, rng: &mut StdRng, randomize: bool) -> Vec<String> {
    let Some(p) = path else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(p) else { return Vec::new() };
    let mut v: Vec<String> = text
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if randomize {
        shuffle_and_drop(&mut v, 0.10, 0.25, rng);
    }
    v
}

/// Перемешать и выкинуть часть. YouTube сортирует теги по алфавиту,
/// поэтому одной перестановки не видно — нужен ещё и выброс.
fn shuffle_and_drop(v: &mut Vec<String>, min: f64, max: f64, rng: &mut StdRng) {
    if v.len() <= 2 {
        return;
    }
    let mut v2 = v.clone();
    v2.shuffle(rng);
    let pct = min + rng.gen::<f64>() * (max - min);
    let keep = ((v2.len() as f64) * (1.0 - pct)).round() as usize;
    let keep = keep.clamp(2, v2.len());
    v2.truncate(keep);
    *v = v2;
}

fn fnv(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// Утилита: прочитать первый непустой файл-шаблон (для веб-редактора)
pub fn read_template(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))
}

/// Отцовский путь не используется, но держим Rc в комплекте для будущих нужд
pub type Shared = Rc<()>;
