//! Язык интерфейса: русский или английский. Тексты пишутся парами прямо в коде —
//! `tr!("Герои", "Heroes")`, `trf!("Записано {n}", "Recorded {n}")`, — поэтому
//! непереведённой строки быть не может, а подстановки проверяет компилятор.

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn set_english(on: bool) {
    ENGLISH.store(on, Ordering::Relaxed);
}

pub fn english() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// Язык системы — для первого запуска: русский для русскоязычных и соседних локалей.
pub fn system_prefers_russian() -> bool {
    sys_locale::get_locales().any(|l| {
        let lang = l.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
        matches!(lang.as_str(), "ru" | "uk" | "be" | "kk")
    })
}

/// Нет ли в строке подстановок `{…}` — для проверки в `tr!` при компиляции.
#[doc(hidden)]
pub const fn plain(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' || b[i] == b'}' {
            return false;
        }
        i += 1;
    }
    true
}

/// Текст без подстановок на текущем языке: `&'static str`.
#[macro_export]
macro_rules! tr {
    ($ru:literal, $en:literal $(,)?) => {{
        const _: () = assert!($crate::i18n::plain($ru) && $crate::i18n::plain($en), "в tr! без подстановок, с ними — trf!");
        if $crate::i18n::english() { $en } else { $ru }
    }};
}

/// `format!` на текущем языке: оба шаблона получают одни и те же аргументы.
#[macro_export]
macro_rules! trf {
    ($ru:literal, $en:literal $(, $($arg:tt)*)?) => {
        if $crate::i18n::english() { format!($en $(, $($arg)*)?) } else { format!($ru $(, $($arg)*)?) }
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn plain_detects_placeholders() {
        assert!(super::plain("Герои"));
        assert!(!super::plain("Записано {n}"));
        assert!(!super::plain("{}"));
    }
}
