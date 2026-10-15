//! Проверка обновлений: спрашиваем у GitHub последний релиз и сравниваем версии.
//! Сама программа себя не перезаписывает — только показывает ссылку на скачивание.

use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

pub const REPO: &str = "BasteArima/dota-revoice";

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    pub url: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
}

/// Последний релиз на GitHub; `None`, если релизов ещё нет.
pub fn latest_release() -> Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let response = ureq::get(&url)
        .set("User-Agent", concat!("dota-revoice/", env!("CARGO_PKG_VERSION")))
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(5))
        .call();
    let body = match response {
        Ok(r) => r.into_string()?,
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(e) => return Err(e).context("GitHub не ответил"),
    };
    let release: GithubRelease = serde_json::from_str(&body).context("непонятный ответ GitHub")?;
    Ok(Some(Release { version: release.tag_name.trim_start_matches('v').to_owned(), url: release.html_url }))
}

/// Новая ли версия `latest` по сравнению с `current` («0.2.0» > «0.1.9»).
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.trim_start_matches('v').split(['.', '-']).map_while(|p| p.parse().ok()).collect() };
    parse(latest) > parse(current)
}

/// Есть ли обновление для этой сборки.
pub fn check() -> Result<Option<Release>> {
    Ok(latest_release()?.filter(|r| is_newer(&r.version, env!("CARGO_PKG_VERSION"))))
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
}
