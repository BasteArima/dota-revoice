//! Сборка `pak02_dir.vpk`: (по желанию) вся озвучка из пака Valve + мои фразы.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::vpk::{VpkBuilder, VpkReader};
use crate::{mp3, vsnd};

pub struct Summary {
    pub replaced: usize,
    /// Мои фразы, которых нет в базовом паке (опечатка в имени или новый файл).
    pub not_in_base: Vec<String>,
    pub copied: usize,
    pub size_bytes: u64,
}

/// Все mp3 в `export/sounds/**` → путь ресурса без расширения (`sounds/vo/hero/name`).
pub fn collect_export(export_root: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let mut out = BTreeMap::new();
    let sounds = export_root.join("sounds");
    if sounds.is_dir() {
        walk(&sounds, "sounds", &mut out)?;
    }
    Ok(out)
}

fn walk(dir: &Path, rel: &str, out: &mut BTreeMap<String, PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_ascii_lowercase();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk(&path, &format!("{rel}/{name}"), out)?;
        } else if let Some(stem) = name.strip_suffix(".mp3") {
            out.insert(format!("{rel}/{stem}"), path);
        }
    }
    Ok(())
}

/// `replacements`: путь ресурса без расширения → mp3-файл.
/// `progress` получает долю готовности 0..1.
pub fn build(
    base: Option<&Path>,
    replacements: &BTreeMap<String, PathBuf>,
    out_path: &Path,
    progress: impl Fn(f32),
) -> Result<Summary> {
    let mut vpk = VpkBuilder::default();
    let mut compiled = BTreeMap::new();
    for (resource, file) in replacements {
        let bytes = std::fs::read(file).with_context(|| format!("не читается {}", file.display()))?;
        let stream = mp3::parse(&bytes).with_context(|| format!("не разобрал mp3 {}", file.display()))?;
        compiled.insert(format!("{resource}.vsnd_c"), vsnd::build(resource, &stream));
    }

    let mut not_in_base = Vec::new();
    let mut copied = 0;
    if let Some(base) = base {
        let reader = VpkReader::open(base)?;
        not_in_base = compiled.keys().filter(|p| !reader.contains(p)).cloned().collect();
        let total = reader.entries().len().max(1);
        for (i, path) in reader.entries().keys().enumerate() {
            if !compiled.contains_key(path) {
                vpk.add(path, reader.read(path)?)?;
                copied += 1;
            }
            if i % 512 == 0 {
                progress(0.9 * i as f32 / total as f32);
            }
        }
    }
    let replaced = compiled.len() - not_in_base.len();
    for (path, data) in compiled {
        vpk.add(&path, data)?;
    }

    progress(0.9);
    if let Some(dir) = out_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = File::create(out_path).with_context(|| format!("не создаётся {}", out_path.display()))?;
    vpk.write_to(BufWriter::new(file)).with_context(|| format!("не записался {}", out_path.display()))?;
    progress(1.0);
    Ok(Summary { replaced, not_in_base, copied, size_bytes: std::fs::metadata(out_path)?.len() })
}
