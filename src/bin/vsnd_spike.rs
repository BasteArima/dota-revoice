//! Утилита для отладки: собрать VPK из пар «путь в игре = mp3-файл».
//!
//! vsnd_spike [--base <pak01_dir.vpk>] <out.vpk> <путь_в_игре>=<файл.mp3> ...
//!
//! С `--base` в пак сначала копируется всё из указанного пака
//! (например, русская озвучка), а перечисленные файлы подменяются.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use dota_revoice::pack;

const USAGE: &str = "использование: vsnd_spike [--base <pak01_dir.vpk>] <out.vpk> <путь_в_игре>=<файл.mp3> ...\n\
                     пример: vsnd_spike out/pak02_dir.vpk sounds/vo/bounty_hunter/bount_kill_01=kill.mp3";

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let base = match args.iter().position(|a| a == "--base") {
        Some(i) if i + 1 < args.len() => Some(args.drain(i..i + 2).nth(1).unwrap()),
        Some(_) => bail!("после --base нужен путь к паку\n{USAGE}"),
        None => None,
    };
    let Some((out_path, pairs)) = args.split_first().filter(|(_, p)| !p.is_empty()) else {
        bail!("{USAGE}");
    };

    let mut replacements = BTreeMap::new();
    for pair in pairs {
        let (game_path, file) = pair
            .split_once('=')
            .with_context(|| format!("ожидал <путь_в_игре>=<файл.mp3>, получил {pair}"))?;
        let resource = game_path.trim_end_matches(".vsnd_c").trim_end_matches(".vsnd").to_ascii_lowercase();
        replacements.insert(resource, PathBuf::from(file));
    }

    let summary = pack::build(base.as_deref().map(Path::new), &replacements, Path::new(out_path), |_| {})?;
    for p in &summary.not_in_base {
        eprintln!("  внимание: в базовом паке нет {p} — это новый файл, а не замена");
    }
    println!(
        "готово: {out_path} — заменено {}, скопировано {}, {:.1} МБ",
        summary.replaced,
        summary.copied,
        summary.size_bytes as f64 / 1_048_576.0
    );
    Ok(())
}
