//! Обработка записи перед игрой: шумодав → усиление → лимитер, потом обрезка.
//!
//! Всё неразрушающее: исходный wav не меняется, настройки хранятся в проекте,
//! а mp3 для игры каждый раз пересобирается из исходника.

use std::borrow::Cow;

use crate::audio::{RATE, fade_edges, resample};

/// Частота, на которой работает RNNoise.
const DENOISE_RATE: u32 = 48000;
/// Потолок лимитера: −0.5 dBFS, запас под mp3.
const CEILING: f32 = 0.944;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Settings {
    pub denoise: bool,
    pub gain_db: f32,
}

/// Вся запись целиком с шумодавом и усилением — её показываем на волне.
pub fn process(raw: &[f32], s: Settings) -> Vec<f32> {
    let mut out: Cow<[f32]> = Cow::Borrowed(raw);
    if s.denoise {
        out = Cow::Owned(denoise(raw));
    }
    if s.gain_db.abs() > 0.01 {
        let g = db_to_gain(s.gain_db);
        let mut v: Vec<f32> = out.iter().map(|x| x * g).collect();
        limit(&mut v);
        out = Cow::Owned(v);
    }
    out.into_owned()
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
    let target = loudness_lufs(original)?;
    let mut gain = 0.0f32;
    for _ in 0..2 {
        let rendered = cut(&process(raw, Settings { gain_db: gain, ..s }), range);
        gain += target - loudness_lufs(&rendered)?;
        gain = gain.clamp(-20.0, 24.0);
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
        let louder = cut(&process(&quiet, Settings { gain_db: gain, denoise: false }), range);
        let diff = loudness_lufs(&louder).unwrap() - loudness_lufs(&original).unwrap();
        assert!(diff.abs() < 0.5, "разница {diff}");
    }

    #[test]
    fn limiter_keeps_peaks_under_ceiling() {
        let loud = process(&tone(1.0, 0.8), Settings { gain_db: 12.0, denoise: false });
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
    fn cut_fades_only_when_trimmed() {
        let full = vec![0.5f32; 1000];
        assert_eq!(cut(&full, (0, 1000)), full);
        let part = cut(&full, (100, 900));
        assert_eq!(part.len(), 800);
        assert_eq!(part[0], 0.0);
    }
}
