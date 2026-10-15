//! Этап 0: собрать VPK из пар «путь в игре = mp3-файл».
//!
//! vsnd_spike [--base <pak01_dir.vpk>] <out.vpk> <путь_в_игре>=<файл.mp3> ...
//!
//! С `--base` в пак сначала копируется всё из указанного пака
//! (например, русская озвучка), а перечисленные файлы подменяются.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use anyhow::{Context, Result, bail};
use dota_revoice::{
    mp3,
    vpk::{VpkBuilder, VpkReader},
    vsnd,
};

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
        let resource = game_path.trim_end_matches(".vsnd_c").trim_end_matches(".vsnd");

        let bytes = std::fs::read(file).with_context(|| format!("не читается {file}"))?;
        let stream = mp3::parse(&bytes).with_context(|| format!("не разобрал mp3 {file}"))?;
        if stream.sample_rate != 44100 {
            eprintln!("  внимание: {file} в {} Гц, озвучка в игре 44100 Гц", stream.sample_rate);
        }
        let compiled = vsnd::build(resource, &stream);
        println!(
            "{resource}.vsnd_c ← {file}: {} Гц, {} кан., {} кадров, {:.3} с, {} байт",
            stream.sample_rate,
            stream.channels,
            stream.frame_count,
            stream.duration_secs(),
            compiled.len()
        );
        replacements.insert(format!("{resource}.vsnd_c").to_ascii_lowercase(), compiled);
    }

    let mut vpk = VpkBuilder::default();
    if let Some(base) = &base {
        let reader = VpkReader::open(base)?;
        for path in replacements.keys() {
            if !reader.contains(path) {
                eprintln!("  внимание: в базовом паке нет {path} — это новый файл, а не замена");
            }
        }
        let mut copied = 0usize;
        for path in reader.entries().keys().filter(|p| !replacements.contains_key(*p)) {
            vpk.add(path, reader.read(path)?)?;
            copied += 1;
        }
        println!("скопировано из {base}: {copied} файлов");
    }
    for (path, data) in replacements {
        vpk.add(&path, data)?;
    }

    if let Some(dir) = Path::new(out_path).parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let file = File::create(out_path).with_context(|| format!("не создаётся {out_path}"))?;
    vpk.write_to(BufWriter::new(file)).with_context(|| format!("не записался {out_path}"))?;
    let size = std::fs::metadata(out_path)?.len();
    println!("готово: {out_path} ({} файлов, {:.1} МБ)", vpk.len(), size as f64 / 1_048_576.0);
    Ok(())
}
