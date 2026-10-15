//! Сохранение фраз: mp3 для игры (как у Valve: 64 кбит/с, моно, 44100 Гц)
//! и wav без потерь — чтобы потом можно было переобработать.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, MonoPcm, Quality};

use crate::audio::RATE;

pub fn encode_mp3(samples: &[f32]) -> Result<Vec<u8>> {
    let mut b = Builder::new().context("LAME не инициализировался")?;
    b.set_num_channels(1).map_err(|e| anyhow!("LAME: {e:?}"))?;
    b.set_sample_rate(RATE).map_err(|e| anyhow!("LAME: {e:?}"))?;
    b.set_brate(Bitrate::Kbps64).map_err(|e| anyhow!("LAME: {e:?}"))?;
    b.set_quality(Quality::Best).map_err(|e| anyhow!("LAME: {e:?}"))?;
    // Без служебного Info-кадра: игре он не нужен.
    b.set_to_write_vbr_tag(false).map_err(|e| anyhow!("LAME: {e:?}"))?;
    let mut enc = b.build().map_err(|e| anyhow!("LAME: {e:?}"))?;

    let clamped: Vec<f32> = samples.iter().map(|s| s.clamp(-1.0, 1.0)).collect();
    let mut out = Vec::with_capacity(mp3lame_encoder::max_required_buffer_size(clamped.len()));
    enc.encode_to_vec(MonoPcm(clamped.as_slice()), &mut out).map_err(|e| anyhow!("LAME: {e:?}"))?;
    enc.flush_to_vec::<FlushNoGap>(&mut out).map_err(|e| anyhow!("LAME: {e:?}"))?;
    Ok(out)
}

pub fn write_wav(path: &Path, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate: RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).with_context(|| format!("не создаётся {}", path.display()))?;
    for &s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    w.finalize()?;
    Ok(())
}

/// Пишет фразу в оба места, создавая папки.
pub fn save_take(mp3_path: &Path, wav_path: &Path, samples: &[f32]) -> Result<()> {
    for p in [mp3_path, wav_path] {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let mp3 = encode_mp3(samples)?;
    std::fs::write(mp3_path, mp3).with_context(|| format!("не записался {}", mp3_path.display()))?;
    write_wav(wav_path, samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mp3_roundtrip_is_game_compatible() {
        let tone: Vec<f32> = (0..RATE).map(|i| (i as f32 * 300.0 * std::f32::consts::TAU / RATE as f32).sin() * 0.3).collect();
        let bytes = encode_mp3(&tone).unwrap();
        let stream = crate::mp3::parse(&bytes).unwrap();
        assert_eq!((stream.sample_rate, stream.channels), (44100, 1));
        // Секунда звука ≈ 38 кадров по 1152 сэмпла (+ задержка кодировщика).
        assert!((38..=42).contains(&stream.frame_count), "{}", stream.frame_count);
        // Ровно 64 кбит/с: кадры по 208–209 байт.
        assert!(stream.data.len() / stream.frame_count as usize >= 208);
    }
}
