use anyhow::Result;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

/// Реестр того, что уже использовалось — чтобы картинки и музыка
/// не повторялись между каналами, даже если жанр один и тот же.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsedRegistry {
    /// ключ "трек::картинка"
    #[serde(default)]
    pub pairs: BTreeSet<String>,
    /// по скольку раз использовался каждый трек
    #[serde(default)]
    pub music: std::collections::BTreeMap<String, u32>,
    /// по скольку раз использовалась каждая картинка
    #[serde(default)]
    pub picture: BTreeSet<String>,
}

impl UsedRegistry {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|r| serde_json::from_str(&r).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn note(&mut self, music: &Path, picture: Option<&Path>) {
        let m = fname(music);
        if let Some(p) = picture {
            let pn = fname(p);
            self.pairs.insert(format!("{m}::{pn}"));
            self.picture.insert(pn);
        }
        *self.music.entry(m).or_insert(0) += 1;
    }

    /// Выбрать трек и сразу записать выбор на диск под локом.
    ///
    /// Так два окна программы не выберут один и тот же трек: второе окно
    /// перечитает `used.json` уже с пометкой первого.
    pub fn pick_and_note(
        path: &Path,
        music: &[std::path::PathBuf],
        pictures: &[std::path::PathBuf],
        seed: u64,
    ) -> Result<Option<(std::path::PathBuf, Option<std::path::PathBuf>)>> {
        let _guard = crate::lock::state_lock(path)?;
        let mut r = Self::load(path);
        let got = r.pick(music, pictures, seed);
        if let Some((t, p)) = &got {
            r.note(t, p.as_deref());
            r.save(path)?;
        }
        Ok(got)
    }

    /// Выбрать трек и картинку, которые ещё не использовались в этой комбинации
    pub fn pick(
        &mut self,
        music: &[std::path::PathBuf],
        pictures: &[std::path::PathBuf],
        seed: u64,
    ) -> Option<(std::path::PathBuf, Option<std::path::PathBuf>)> {
        if music.is_empty() {
            return None;
        }
        let mut rng = StdRng::seed_from_u64(seed);

        // сперва треки, которые вообще не брали
        let fresh_tracks: Vec<_> = music
            .iter()
            .filter(|m| !self.music.contains_key(&fname(m)))
            .cloned()
            .collect();

        let track = if !fresh_tracks.is_empty() {
            let mut f = fresh_tracks;
            f.shuffle(&mut rng);
            f[0].clone()
        } else {
            // все треки уже были — берём наименее использованный
            let mut sorted: Vec<_> = music.to_vec();
            sorted.sort_by_key(|m| self.music.get(&fname(m)).copied().unwrap_or(0));
            sorted[0].clone()
        };

        let picture = self.pick_picture(pictures, &track, &mut rng);
        Some((track, picture))
    }

    fn pick_picture(
        &mut self,
        pictures: &[std::path::PathBuf],
        track: &Path,
        rng: &mut StdRng,
    ) -> Option<std::path::PathBuf> {
        if pictures.is_empty() {
            return None;
        }
        let t = fname(track);
        // сперва те, что не были в паре с этим треком
        let mut fresh: Vec<_> = pictures
            .iter()
            .filter(|p| !self.pairs.contains(&format!("{t}::{}", fname(p))))
            .collect();
        if fresh.is_empty() {
            fresh = pictures.iter().collect();
        }
        fresh.shuffle(rng);
        Some(fresh[0].clone())
    }
}

fn fname(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ytf-plan-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn two_windows_do_not_pick_same_track() {
        let dir = temp_dir("pick");
        let used = dir.join("used.json");
        let music: Vec<PathBuf> = (0..20).map(|i| PathBuf::from(format!("t{i}.mp3"))).collect();

        let mut handles = Vec::new();
        for t in 0..2u64 {
            let used = used.clone();
            let music = music.clone();
            handles.push(std::thread::spawn(move || {
                let mut got = Vec::new();
                for i in 0..5u64 {
                    let (track, _) = UsedRegistry::pick_and_note(&used, &music, &[], t * 100 + i)
                        .unwrap()
                        .expect("трек должен найтись");
                    got.push(track);
                }
                got
            }));
        }

        let mut all = Vec::new();
        for h in handles {
            all.extend(h.join().unwrap());
        }
        all.sort();
        all.dedup();
        assert_eq!(all.len(), 10, "два окна выбрали один и тот же трек");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
