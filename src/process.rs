//! Обработка записи перед игрой: шумодав → тон → усиление → лимитер, потом обрезка.
//!
//! Всё неразрушающее: исходный wav не меняется, настройки хранятся в проекте,
//! а mp3 для игры каждый раз пересобирается из исходника.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::audio::{RATE, fade_edges, resample};

/// Частота, на которой работает RNNoise.
const DENOISE_RATE: u32 = 48000;
/// Потолок лимитера: −0.5 dBFS, запас под mp3.
const CEILING: f32 = 0.944;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub denoise: bool,
    pub gain_db: f32,
    /// Сдвиг высоты голоса в полутонах.
    pub pitch: f32,
}

/// Пределы усиления: и для ползунка, и для подгонки под оригинал.
pub const GAIN_RANGE: std::ops::RangeInclusive<f32> = -20.0..=24.0;

/// Вся запись целиком с шумодавом и усилением — её показываем на волне.
pub fn process(raw: &[f32], s: Settings) -> Vec<f32> {
    amplify(&shape(raw, s), s.gain_db)
}

/// Тяжёлая часть обработки — шумодав и тон. От усиления не зависит, поэтому
/// её результат можно запомнить и менять громкость без пересчёта.
pub fn shape(raw: &[f32], s: Settings) -> Vec<f32> {
    let mut out: Cow<[f32]> = Cow::Borrowed(raw);
    if s.denoise {
        out = Cow::Owned(denoise(raw));
    }
    if s.pitch.abs() > 0.01 {
        out = Cow::Owned(pitch_shift(&out, s.pitch));
    }
    out.into_owned()
}

/// Усиление с лимитером — быстро, годится на каждый кадр, пока тянут ползунок.
pub fn amplify(shaped: &[f32], gain_db: f32) -> Vec<f32> {
    if gain_db.abs() <= 0.01 {
        return shaped.to_vec();
    }
    let g = db_to_gain(gain_db);
    let mut v: Vec<f32> = shaped.iter().map(|x| x * g).collect();
    limit(&mut v);
    v
}

/// Кусок для игры: `range` в сэмплах, с затуханием по краям, если что-то отрезано.
pub fn cut(full: &[f32], range: (usize, usize)) -> Vec<f32> {
    let (a, b) = (range.0.min(full.len()), range.1.min(full.len()));
    let mut part = full[a..b.max(a)].to_vec();
    if (a, b) != (0, full.len()) {
        fade_edges(&mut part);
    }
    part
}

/// Подавление шума нейросетью RNNoise (nnnoiseless). Длина сохраняется.
pub fn denoise(samples: &[f32]) -> Vec<f32> {
    use nnnoiseless::DenoiseState;
    let Ok(up) = resample(samples, RATE, DENOISE_RATE) else { return samples.to_vec() };
    let frame = DenoiseState::FRAME_SIZE;
    let mut state = DenoiseState::new();
    let mut out = Vec::with_capacity(up.len() + frame);
    let mut input = vec![0f32; frame];
    let mut output = vec![0f32; frame];
    for chunk in up.chunks(frame) {
        // RNNoise ждёт значения в масштабе 16-битного PCM.
        input.fill(0.0);
        for (dst, &src) in input.iter_mut().zip(chunk) {
            *dst = src * 32768.0;
        }
        state.process_frame(&mut output, &input);
        out.extend(output.iter().take(chunk.len()).map(|v| v / 32768.0));
    }
    let mut back = resample(&out, DENOISE_RATE, RATE).unwrap_or_else(|_| samples.to_vec());
    back.resize(samples.len(), 0.0);
    back
}

/// Сдвиг высоты на `semitones` без изменения длительности: растягиваем по времени
/// (WSOLA) и «проигрываем» быстрее или медленнее, пересчитывая частоту.
pub fn pitch_shift(samples: &[f32], semitones: f32) -> Vec<f32> {
    if semitones.abs() < 0.01 || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = 2f32.powf(semitones / 12.0);
    let stretched = time_stretch(samples, ratio);
    // Частоту округляем до сотен: ресемплеру проще, а ошибка высоты меньше 0.2%.
    let from = ((RATE as f32 * ratio / 100.0).round() * 100.0) as u32;
    let mut out = resample(&stretched, from, RATE).unwrap_or_else(|_| samples.to_vec());
    out.resize(samples.len(), 0.0);
    out
}

/// WSOLA: окна по 23 мс с перекрытием 50%, каждое следующее берём там,
/// где оно лучше всего продолжает предыдущее (по корреляции) — без «бульканья».
fn time_stretch(x: &[f32], alpha: f32) -> Vec<f32> {
    const N: usize = 1024;
    const HOP: usize = N / 2;
    const TOL: isize = 256;
    let window: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / N as f32).cos()).collect();
    let get = |i: isize| if i >= 0 && (i as usize) < x.len() { x[i as usize] } else { 0.0 };
    let target_len = (x.len() as f32 * alpha) as usize;
    let mut out = vec![0f32; target_len + N];
    let mut norm = vec![0f32; target_len + N];
    let analysis_hop = HOP as f32 / alpha;
    let mut prev = 0isize;
    for k in 0..=target_len / HOP {
        let ideal = (k as f32 * analysis_hop) as isize;
        let pos = if k == 0 {
            0
        } else {
            let natural = prev + HOP as isize;
            let (mut best, mut best_c) = (ideal, f32::MIN);
            let mut d = -TOL;
            while d <= TOL {
                let cand = ideal + d;
                let c: f32 = (0..HOP).step_by(2).map(|i| get(natural + i as isize) * get(cand + i as isize)).sum();
                if c > best_c {
                    best_c = c;
                    best = cand;
                }
                d += 2;
            }
            best
        };
        let o = k * HOP;
        for i in 0..N {
            out[o + i] += get(pos + i as isize) * window[i];
            norm[o + i] += window[i];
        }
        prev = pos;
    }
    for (v, n) in out.iter_mut().zip(&norm) {
        if *n > 1e-3 {
            *v /= n;
        }
    }
    out.truncate(target_len);
    out
}

/// Интегральная громкость в LUFS (EBU R128). `None` — слишком коротко или тишина.
pub fn loudness_lufs(samples: &[f32]) -> Option<f32> {
    let mut m = ebur128::EbuR128::new(1, RATE, ebur128::Mode::I).ok()?;
    m.add_frames_f32(samples).ok()?;
    let l = m.loudness_global().ok()? as f32;
    (l.is_finite() && l > -70.0).then_some(l)
}

/// Сколько dB добавить к записи, чтобы по громкости совпасть с оригиналом.
/// Две итерации: лимитер немного «съедает» громкость, второй проход это добирает.
pub fn match_gain(original: &[f32], raw: &[f32], s: Settings, range: (usize, usize)) -> Option<f32> {
    match_gain_shaped(original, &shape(raw, s), range)
}

/// То же для записи, уже прошедшей шумодав и тон (`shape`).
pub fn match_gain_shaped(original: &[f32], shaped: &[f32], range: (usize, usize)) -> Option<f32> {
    let target = loudness_lufs(original)?;
    let mut gain = 0.0f32;
    for _ in 0..2 {
        let rendered = cut(&amplify(shaped, gain), range);
        gain += target - loudness_lufs(&rendered)?;
        gain = gain.clamp(*GAIN_RANGE.start(), *GAIN_RANGE.end());
    }
    Some((gain * 10.0).round() / 10.0)
}

fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Простой лимитер: огибающая с мгновенной атакой и плавным отпусканием.
fn limit(samples: &mut [f32]) {
    let release = (-1.0 / (0.05 * RATE as f32)).exp(); // ~50 мс
    let mut env = 0f32;
    for x in samples.iter_mut() {
        let a = x.abs();
        env = if a > env { a } else { a + (env - a) * release };
        if env > CEILING {
            *x *= CEILING / env;
        }
        *x = x.clamp(-CEILING, CEILING);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32, amp: f32) -> Vec<f32> {
        (0..(secs * RATE as f32) as usize).map(|i| (i as f32 * 300.0 * std::f32::consts::TAU / RATE as f32).sin() * amp).collect()
    }

    #[test]
    fn gain_matches_loudness() {
        let original = tone(2.0, 0.5);
        let quiet = tone(2.0, 0.1);
        let range = (0, quiet.len());
        let gain = match_gain(&original, &quiet, Settings::default(), range).unwrap();
        assert!((12.0..=15.0).contains(&gain), "gain {gain}");
        let louder = cut(&process(&quiet, Settings { gain_db: gain, ..Settings::default() }), range);
        let diff = loudness_lufs(&louder).unwrap() - loudness_lufs(&original).unwrap();
        assert!(diff.abs() < 0.5, "разница {diff}");
    }

    #[test]
    fn amplify_after_shape_is_the_same_as_process() {
        let s = Settings { denoise: false, gain_db: 6.0, pitch: 2.0 };
        let raw = tone(0.5, 0.3);
        assert_eq!(amplify(&shape(&raw, s), s.gain_db), process(&raw, s));
        assert_eq!(amplify(&raw, 0.0), raw);
    }

    #[test]
    fn limiter_keeps_peaks_under_ceiling() {
        let loud = process(&tone(1.0, 0.8), Settings { gain_db: 12.0, ..Settings::default() });
        assert!(loud.iter().all(|x| x.abs() <= CEILING + 1e-6));
    }

    #[test]
    fn denoise_keeps_length_and_quiets_noise() {
        // Белый шум без голоса RNNoise должен заметно приглушить.
        let mut seed = 12345u32;
        let noise: Vec<f32> = (0..RATE as usize)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                ((seed >> 16) as f32 / 32768.0 - 1.0) * 0.05
            })
            .collect();
        let out = denoise(&noise);
        assert_eq!(out.len(), noise.len());
        let energy = |s: &[f32]| s.iter().map(|x| x * x).sum::<f32>();
        assert!(energy(&out) < energy(&noise) * 0.5);
    }

    #[test]
    fn pitch_shift_changes_frequency_not_length() {
        let src = tone(1.0, 0.5); // 300 Гц
        let crossings = |s: &[f32]| s[4410..39690].windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count() as f32 / 0.8;
        for (semi, hz) in [(12.0, 600.0), (-12.0, 150.0), (3.0, 356.8)] {
            let out = pitch_shift(&src, semi);
            assert_eq!(out.len(), src.len());
            let f = crossings(&out);
            assert!((f - hz).abs() < hz * 0.03, "{semi} полутонов: ждали {hz} Гц, вышло {f}");
        }
    }

    #[test]
    fn cut_fades_only_when_trimmed() {
        let full = vec![0.5f32; 1000];
        assert_eq!(cut(&full, (0, 1000)), full);
        let part = cut(&full, (100, 900));
        assert_eq!(part.len(), 800);
        assert_eq!(part[0], 0.0);
    }
}
