//! Чтение Ogg Opus — так Telegram и другие мессенджеры сохраняют голосовые.
//! symphonia Opus не декодирует, поэтому контейнер разбираем крейтом `ogg`,
//! а пакеты — `opus-decoder` (чистый Rust, без C).

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use opus_decoder::OpusDecoder;

use crate::audio::{RATE, resample};

/// Opus всегда декодируется на 48 кГц.
const OPUS_RATE: u32 = 48000;
/// Самый длинный пакет Opus — 120 мс.
const MAX_FRAME: usize = OPUS_RATE as usize * 120 / 1000;

/// Похож ли файл на Ogg Opus: заголовок `OpusHead` в первой странице.
pub fn is_ogg_opus(path: &Path) -> bool {
    let mut head = [0u8; 64];
    let n = File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    head[..n].starts_with(b"OggS") && head[..n].windows(8).any(|w| w == b"OpusHead")
}

/// Читает Ogg Opus в моно 44100 Гц.
/// Декодер молодой — если он запаникует на странном файле, это станет ошибкой, а не падением.
pub fn decode_file(path: &Path) -> Result<Vec<f32>> {
    std::panic::catch_unwind(|| decode(path)).unwrap_or_else(|_| bail!("не удалось декодировать Opus в {}", path.display()))
}

fn decode(path: &Path) -> Result<Vec<f32>> {
    let file = File::open(path).with_context(|| format!("не открывается {}", path.display()))?;
    let mut reader = ogg::PacketReader::new(BufReader::new(file));

    let head = reader.read_packet()?.context("пустой файл")?.data;
    ensure!(head.len() >= 19 && head.starts_with(b"OpusHead"), "это не Ogg Opus");
    let channels = head[9] as usize;
    let pre_skip = u16::from_le_bytes([head[10], head[11]]) as usize;
    let gain_db = i16::from_le_bytes([head[16], head[17]]) as f32 / 256.0;
    let mapping = head[18];
    if mapping != 0 || !(1..=2).contains(&channels) {
        bail!("многоканальный Opus ({channels} кан.) не поддерживается");
    }

    // Второй пакет — OpusTags (метаданные), он не нужен.
    reader.read_packet()?.context("обрезанный файл: нет OpusTags")?;

    let mut decoder = OpusDecoder::new(OPUS_RATE, channels).map_err(|e| anyhow::anyhow!("Opus: {e:?}"))?;
    let mut pcm = vec![0f32; MAX_FRAME * channels];
    let mut mono = Vec::new();
    while let Some(packet) = reader.read_packet()? {
        let n = decoder.decode_float(&packet.data, &mut pcm, false).map_err(|e| anyhow::anyhow!("Opus: {e:?}"))?;
        mono.extend(pcm[..n * channels].chunks(channels).map(|f| f.iter().sum::<f32>() / channels as f32));
    }
    // Первые pre_skip сэмплов — разгон декодера, в звук не входят.
    mono.drain(..pre_skip.min(mono.len()));
    if gain_db != 0.0 {
        let g = 10f32.powf(gain_db / 20.0);
        mono.iter_mut().for_each(|s| *s *= g);
    }
    resample(&mono, OPUS_RATE, RATE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_telegram_like_voice() {
        // 1.5 с синуса 440 Гц, ffmpeg -c:a libopus -b:a 24k -application voip
        let path = Path::new("tests/data/voice_440hz.ogg");
        assert!(is_ogg_opus(path));
        let s = decode_file(path).unwrap();
        let secs = s.len() as f32 / RATE as f32;
        assert!((1.45..1.55).contains(&secs), "длительность {secs}");

        // Частота: считаем переходы через ноль в середине записи.
        let mid = &s[RATE as usize / 4..RATE as usize * 5 / 4];
        let crossings = mid.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((430..=450).contains(&crossings), "≈440 Гц, а вышло {crossings}");
        // Синус из ffmpeg — амплитуда 1/8, плюс volume=0.5; libopus выдаёт пик −23.6 dB.
        let peak = mid.iter().fold(0f32, |m, x| m.max(x.abs()));
        assert!((0.05..0.08).contains(&peak), "амплитуда {peak}");
    }
}
