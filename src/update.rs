//! Обновления: спрашиваем у GitHub последний релиз, сравниваем версии и обновляемся на месте.
//!
//! Программа раздаётся одним файлом, поэтому обновление — замена этого файла: архив
//! релиза скачивается, сверяется по SHA-256 (GitHub отдаёт его для каждого файла релиза),
//! из него достаётся программа. Запущенный exe Windows удалить не даёт, а переименовать —
//! да: текущий становится `<имя>.old`, новый ложится под его именем и запускается, старый
//! процесс закрывается, новый при старте удаляет `.old`. На Linux так же. На macOS
//! программа — папка `.app`, там обновление пока со страницы релиза.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use crate::{tr, trf};

pub const REPO: &str = "BasteArima/dota-revoice";
/// Аргумент, с которым обновлённая программа запускается: `--updated-from 0.5.0`.
pub const UPDATED_FROM: &str = "--updated-from";

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    /// Страница релиза.
    pub url: String,
    /// «Что нового» — пункты из описания релиза.
    pub notes: Vec<String>,
    pub assets: Vec<Asset>,
    /// Можно обновиться на месте: есть архив для этой системы и в папку программы можно писать.
    pub installable: bool,
}

#[derive(Debug, Clone)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// `sha256:…` от GitHub; у старых релизов может не быть.
    pub digest: Option<String>,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

fn user_agent() -> String {
    format!("dota-revoice/{}", current_version())
}

/// Последний релиз на GitHub; `None`, если релизов ещё нет.
pub fn latest_release() -> Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let response = ureq::get(&url).set("User-Agent", &user_agent()).set("Accept", "application/vnd.github+json").timeout(Duration::from_secs(5)).call();
    let body = match response {
        Ok(r) => r.into_string()?,
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(e) => return Err(e).context(tr!("GitHub не ответил", "GitHub didn't respond")),
    };
    let release: GithubRelease = serde_json::from_str(&body).context(tr!("непонятный ответ GitHub", "unexpected response from GitHub"))?;
    let assets = release
        .assets
        .into_iter()
        .map(|a| Asset { name: a.name, url: a.browser_download_url, size: a.size, digest: a.digest })
        .collect();
    let mut release = Release {
        version: release.tag_name.trim_start_matches('v').to_owned(),
        url: release.html_url,
        notes: notes_of(release.body.as_deref().unwrap_or_default()),
        assets,
        installable: false,
    };
    release.installable = asset(&release).is_some() && exe_dir_writable();
    Ok(Some(release))
}

/// Пункты «Что нового» из описания релиза: строки списка (`- …`) до того, что дописывает
/// GitHub (`## What's Changed`, `**Full Changelog**`).
pub fn notes_of(body: &str) -> Vec<String> {
    body.lines()
        .map(str::trim)
        .take_while(|l| !l.starts_with("## What") && !l.starts_with("**Full Changelog**"))
        .filter_map(|l| l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")))
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Новая ли версия `latest` по сравнению с `current` («0.2.0» > «0.1.9»).
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.trim_start_matches('v').split(['.', '-']).map_while(|p| p.parse().ok()).collect() };
    parse(latest) > parse(current)
}

/// Версия этой сборки. В отладочной её можно подменить (`DOTA_REVOICE_VERSION=0.4.0`),
/// чтобы проверить обновление на настоящем релизе.
pub fn current_version() -> String {
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("DOTA_REVOICE_VERSION") {
        return v;
    }
    env!("CARGO_PKG_VERSION").to_owned()
}

/// Есть ли обновление для этой сборки.
pub fn check() -> Result<Option<Release>> {
    Ok(latest_release()?.filter(|r| is_newer(&r.version, &current_version())))
}

/// Архив этой системы в релизе: `…-windows-x64.zip`, `…-linux-x64.tar.gz`. На macOS
/// программа — папка `.app`, её так не заменить.
pub fn asset(release: &Release) -> Option<&Asset> {
    let suffix = if cfg!(windows) {
        "-windows-x64.zip"
    } else if cfg!(target_os = "linux") {
        "-linux-x64.tar.gz"
    } else {
        return None;
    };
    release.assets.iter().find(|a| a.name.ends_with(suffix))
}

/// Можно ли писать в папку программы (в Program Files — нельзя).
fn exe_dir_writable() -> bool {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) else { return false };
    let probe = dir.join(format!(".dota_revoice_update_{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Скачивает архив релиза, сверяет SHA-256 и достаёт из него программу.
/// `progress(скачано, всего)` зовётся по ходу; `cancel` прерывает скачивание.
pub fn download(asset: &Asset, cancel: &AtomicBool, mut progress: impl FnMut(u64, u64)) -> Result<Vec<u8>> {
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout_read(Duration::from_secs(30)).build();
    let response = agent.get(&asset.url).set("User-Agent", &user_agent()).call().context(tr!("архив обновления не скачивается", "can't download the update archive"))?;
    let total = response.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(asset.size);
    let mut reader = response.into_reader();
    let mut data = Vec::with_capacity(total as usize);
    let mut buf = [0u8; 64 * 1024];
    loop {
        ensure!(!cancel.load(Ordering::Relaxed), tr!("отменено", "cancelled"));
        let n = reader.read(&mut buf).context(tr!("скачивание прервалось", "the download was interrupted"))?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
        progress(data.len() as u64, total);
    }
    verify(&data, asset)?;
    program_from(&data, &asset.name)
}

/// Сверка с размером и SHA-256 из релиза: битый или подменённый архив не ставим.
fn verify(data: &[u8], asset: &Asset) -> Result<()> {
    ensure!(data.len() as u64 == asset.size, trf!("архив скачался не целиком ({} из {} байт)", "the archive is incomplete ({} of {} bytes)", data.len(), asset.size));
    if let Some(want) = asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")) {
        let got: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
        ensure!(got.eq_ignore_ascii_case(want), tr!("архив не совпал с релизом (SHA-256)", "the archive doesn't match the release (SHA-256)"));
    }
    Ok(())
}

/// Программа из архива релиза: `DotaRevoice/DotaRevoice.exe` в zip, `DotaRevoice/dota-revoice` в tar.gz.
pub fn program_from(archive: &[u8], name: &str) -> Result<Vec<u8>> {
    let not_found = || anyhow::anyhow!(tr!("в архиве обновления нет программы", "the update archive has no program in it"));
    if name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(Cursor::new(archive)).context(tr!("архив обновления не читается", "can't read the update archive"))?;
        let index = (0..zip.len()).find(|&i| zip.name_for_index(i).is_some_and(|n| n.to_ascii_lowercase().ends_with("dotarevoice.exe"))).ok_or_else(not_found)?;
        let mut out = Vec::new();
        zip.by_index(index)?.read_to_end(&mut out)?;
        return Ok(out);
    }
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(archive).read_to_end(&mut tar).context(tr!("архив обновления не читается", "can't read the update archive"))?;
    tar_file(&tar, "/dota-revoice").ok_or_else(not_found)
}

/// Файл из tar по концу имени. Заголовок — 512 байт: имя (100), …, размер восьмеричным
/// числом со смещения 124, тип со смещения 156; данные — следом, кратно 512.
fn tar_file(tar: &[u8], suffix: &str) -> Option<Vec<u8>> {
    let mut pos = 0;
    while let Some(header) = tar.get(pos..pos + 512) {
        if header.iter().all(|&b| b == 0) {
            return None;
        }
        let name = String::from_utf8_lossy(&header[..100]).trim_end_matches('\0').to_owned();
        let size_text = String::from_utf8_lossy(&header[124..136]);
        let size = usize::from_str_radix(size_text.trim_matches(|c: char| c == '\0' || c == ' '), 8).ok()?;
        let regular = matches!(header[156], b'0' | 0);
        let data = pos + 512;
        if regular && name.ends_with(suffix) {
            return tar.get(data..data + size).map(<[u8]>::to_vec);
        }
        pos = data + size.div_ceil(512) * 512;
    }
    None
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    exe.with_file_name(name)
}

/// Ставит новую программу на место текущей: текущая становится `<имя>.old`, новая
/// ложится под её именем. При ошибке всё возвращается как было. Возвращает путь программы.
pub fn install(program: &[u8]) -> Result<PathBuf> {
    let exe = std::env::current_exe().context(tr!("не найти, где лежит программа", "can't find where the app is"))?;
    install_at(&exe, program)?;
    Ok(exe)
}

fn install_at(exe: &Path, program: &[u8]) -> Result<()> {
    let (new, old) = (sibling(exe, ".new"), sibling(exe, ".old"));
    std::fs::write(&new, program).with_context(|| trf!("не записывается {}", "can't write {}", new.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
    }
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(exe, &old) {
        let _ = std::fs::remove_file(&new);
        return Err(e).context(tr!("текущую программу не переименовать", "can't rename the current app"));
    }
    if let Err(e) = std::fs::rename(&new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(e).context(tr!("новую программу не поставить на место", "can't put the new app in place"));
    }
    Ok(())
}

/// Запускает обновлённую программу; она скажет, с какой версии обновилась.
/// Не запустилась — прежняя возвращается на место.
pub fn relaunch(exe: &Path) -> Result<()> {
    let started = std::process::Command::new(exe).arg(UPDATED_FROM).arg(current_version()).current_dir(exe.parent().unwrap_or(Path::new("."))).spawn();
    if let Err(e) = started {
        let _ = std::fs::remove_file(exe);
        let _ = std::fs::rename(sibling(exe, ".old"), exe);
        bail!(trf!("новая версия не запустилась ({e}) — вернул прежнюю", "the new version didn't start ({e}) — restored the old one"));
    }
    Ok(())
}

/// Убирает прежнюю программу после обновления. Пока старый процесс не закрылся, файл
/// занят — тогда `false`, попробовать позже.
pub fn remove_old() -> bool {
    let Ok(exe) = std::env::current_exe() else { return true };
    let _ = std::fs::remove_file(sibling(&exe, ".new"));
    let old = sibling(&exe, ".old");
    !old.exists() || std::fs::remove_file(&old).is_ok()
}

/// С какой версии программа только что обновилась (`--updated-from 0.5.0`).
pub fn updated_from() -> Option<String> {
    let mut args = std::env::args().skip_while(|a| a != UPDATED_FROM);
    args.next()?;
    args.next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
    }

    #[test]
    fn notes_from_release_body() {
        let body = "- Обновление прямо из программы\n* Звуки предметов\n\nтекст\n## What's Changed\n* PR by @x\n**Full Changelog**: https://…";
        assert_eq!(notes_of(body), ["Обновление прямо из программы", "Звуки предметов"]);
        assert!(notes_of("**Full Changelog**: https://github.com/x/compare/v1...v2").is_empty());
    }

    fn asset_for(data: &[u8], name: &str) -> Asset {
        let digest: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
        Asset { name: name.into(), url: String::new(), size: data.len() as u64, digest: Some(format!("sha256:{digest}")) }
    }

    #[test]
    fn verifies_size_and_sha256() {
        let data = b"archive";
        assert!(verify(data, &asset_for(data, "a.zip")).is_ok());
        assert!(verify(b"archivX", &asset_for(data, "a.zip")).is_err(), "подменён");
        assert!(verify(b"arch", &asset_for(data, "a.zip")).is_err(), "не целиком");
        let mut old = asset_for(data, "a.zip");
        old.digest = None;
        assert!(verify(data, &old).is_ok(), "без SHA-256 — по размеру");
    }

    #[test]
    fn program_out_of_zip_and_tar_gz() {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("DotaRevoice/README.md", opts).unwrap();
        zip.write_all(b"readme").unwrap();
        zip.start_file("DotaRevoice/DotaRevoice.exe", opts).unwrap();
        zip.write_all(b"MZ new").unwrap();
        let zip = zip.finish().unwrap().into_inner();
        assert_eq!(program_from(&zip, "DotaRevoice-v0.6.0-windows-x64.zip").unwrap(), b"MZ new");

        // tar: заголовок на 512 байт, данные кратно 512, в конце — нули.
        let mut tar = Vec::new();
        for (name, data) in [("DotaRevoice/README.md", &b"readme"[..]), ("DotaRevoice/dota-revoice", &b"ELF new"[..])] {
            let mut header = [0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
            header[156] = b'0';
            tar.extend_from_slice(&header);
            tar.extend_from_slice(data);
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        }
        tar.extend_from_slice(&[0u8; 1024]);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar).unwrap();
        let gz = gz.finish().unwrap();
        assert_eq!(program_from(&gz, "DotaRevoice-v0.6.0-linux-x64.tar.gz").unwrap(), b"ELF new");
        assert!(program_from(&gz, "x.zip").is_err());
    }

    #[test]
    fn install_swaps_files_and_keeps_old() {
        let dir = std::env::temp_dir().join(format!("dota_revoice_update_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("DotaRevoice.exe");
        std::fs::write(&exe, b"old").unwrap();
        install_at(&exe, b"new").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(sibling(&exe, ".old")).unwrap(), b"old");
        assert!(!sibling(&exe, ".new").exists());
        // Второе обновление: прежний `.old` заменяется.
        install_at(&exe, b"newer").unwrap();
        assert_eq!(std::fs::read(sibling(&exe, ".old")).unwrap(), b"new");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
