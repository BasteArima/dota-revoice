//! Минимальный разбор MPEG Layer III: считаем кадры и вычищаем теги.
//!
//! Движок хранит в .vsnd_c «голый» поток кадров, а число сэмплов считает
//! как `кадры × сэмплов_в_кадре` (проверено на озвучке Bounty Hunter:
//! 129024 = 112 × 1152). Поэтому ID3-теги и служебный Xing/Info-кадр
//! выкидываем, остальное копируем байт в байт.

use anyhow::{Context, Result, bail, ensure};

#[derive(Debug, Clone)]
pub struct Mp3Stream {
    pub sample_rate: u32,
    pub channels: u32,
    pub samples_per_frame: u32,
    pub frame_count: u32,
    /// Только аудиокадры, без тегов.
    pub data: Vec<u8>,
}

impl Mp3Stream {
    pub fn sample_count(&self) -> u32 {
        self.frame_count * self.samples_per_frame
    }

    pub fn duration_secs(&self) -> f32 {
        self.sample_count() as f32 / self.sample_rate as f32
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Version {
    Mpeg1,
    Mpeg2,
    Mpeg25,
}

#[derive(Debug, Clone, Copy)]
struct FrameHeader {
    version: Version,
    has_crc: bool,
    sample_rate: u32,
    channels: u32,
    frame_len: usize,
}

const BITRATES_V1: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
const BITRATES_V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

fn parse_header(b: &[u8]) -> Option<FrameHeader> {
    if b.len() < 4 || b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
        return None;
    }
    let version = match (b[1] >> 3) & 0b11 {
        0b00 => Version::Mpeg25,
        0b10 => Version::Mpeg2,
        0b11 => Version::Mpeg1,
        _ => return None,
    };
    // Только Layer III.
    if (b[1] >> 1) & 0b11 != 0b01 {
        return None;
    }
    let has_crc = b[1] & 1 == 0;
    let bitrate_idx = (b[2] >> 4) as usize;
    let sr_idx = ((b[2] >> 2) & 0b11) as usize;
    let padding = ((b[2] >> 1) & 1) as usize;
    // 0 = free format, 15 = запрещено; sr_idx 3 = зарезервировано.
    if bitrate_idx == 0 || bitrate_idx == 15 || sr_idx == 3 {
        return None;
    }
    let base_sr = [44100, 48000, 32000][sr_idx];
    let (sample_rate, bitrate, coef) = match version {
        Version::Mpeg1 => (base_sr, BITRATES_V1[bitrate_idx], 144),
        Version::Mpeg2 => (base_sr / 2, BITRATES_V2[bitrate_idx], 72),
        Version::Mpeg25 => (base_sr / 4, BITRATES_V2[bitrate_idx], 72),
    };
    let channels = if b[3] >> 6 == 0b11 { 1 } else { 2 };
    let frame_len = (coef * bitrate * 1000 / sample_rate) as usize + padding;
    Some(FrameHeader { version, has_crc, sample_rate, channels, frame_len })
}

/// Xing/Info/VBRI — служебный «пустой» кадр от энкодера, в игре он не нужен.
fn is_info_frame(frame: &[u8], h: &FrameHeader) -> bool {
    let side_info = match (h.version, h.channels) {
        (Version::Mpeg1, 1) => 17,
        (Version::Mpeg1, _) => 32,
        (_, 1) => 9,
        _ => 17,
    };
    let at = 4 + side_info + if h.has_crc { 2 } else { 0 };
    let tag = |o: usize| frame.get(o..o + 4);
    matches!(tag(at), Some(b"Xing") | Some(b"Info")) || tag(36) == Some(b"VBRI")
}

fn skip_id3v2(bytes: &[u8]) -> usize {
    let mut pos = 0;
    // Тегов может быть несколько подряд.
    while bytes.len() >= pos + 10 && &bytes[pos..pos + 3] == b"ID3" {
        let s = &bytes[pos + 6..pos + 10];
        let size = (s[0] as usize) << 21 | (s[1] as usize) << 14 | (s[2] as usize) << 7 | s[3] as usize;
        let footer = if bytes[pos + 5] & 0x10 != 0 { 10 } else { 0 };
        pos += 10 + size + footer;
    }
    pos
}

pub fn parse(bytes: &[u8]) -> Result<Mp3Stream> {
    let mut end = bytes.len();
    if end >= 128 && &bytes[end - 128..end - 125] == b"TAG" {
        end -= 128;
    }
    let mut pos = skip_id3v2(bytes);

    // Ищем первый кадр, за которым сразу идёт второй, — чтобы не принять мусор за синхрослово.
    let first = loop {
        ensure!(pos + 4 <= end, "не нашёл ни одного MPEG Layer III кадра");
        if let Some(h) = parse_header(&bytes[pos..end]) {
            let next = pos + h.frame_len;
            if next == end || bytes.get(next..end).and_then(parse_header).is_some() {
                break h;
            }
        }
        pos += 1;
    };

    let mut data = Vec::with_capacity(end - pos);
    let mut frame_count = 0u32;
    let mut first_frame = true;
    while pos + 4 <= end {
        let Some(h) = parse_header(&bytes[pos..end]) else {
            // Хвост вроде APE-тега — дальше аудио нет.
            break;
        };
        ensure!(
            h.sample_rate == first.sample_rate && h.channels == first.channels,
            "в файле меняется частота или число каналов (смещение {pos})"
        );
        let frame = bytes
            .get(pos..pos + h.frame_len)
            .with_context(|| format!("обрезанный последний кадр (смещение {pos})"))?;
        let skip = first_frame && is_info_frame(frame, &h);
        first_frame = false;
        if !skip {
            data.extend_from_slice(frame);
            frame_count += 1;
        }
        pos += h.frame_len;
    }
    if frame_count == 0 {
        bail!("в файле нет аудиокадров");
    }

    Ok(Mp3Stream {
        sample_rate: first.sample_rate,
        channels: first.channels,
        samples_per_frame: if first.version == Version::Mpeg1 { 1152 } else { 576 },
        frame_count,
        data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MPEG1 L3, 64 кбит/с, 44.1 кГц, моно, без паддинга: 144·64000/44100 = 208 байт.
    fn frame() -> Vec<u8> {
        let mut f = vec![0xFF, 0xFB, 0x50, 0xC0];
        f.resize(208, 0);
        f
    }

    #[test]
    fn counts_frames_and_strips_tags() {
        let mut file = b"ID3\x03\x00\x00\x00\x00\x00\x05abcde".to_vec();
        for _ in 0..3 {
            file.extend(frame());
        }
        file.extend(b"TAG");
        file.resize(file.len() + 125, 0);

        let s = parse(&file).unwrap();
        assert_eq!((s.sample_rate, s.channels, s.frame_count), (44100, 1, 3));
        assert_eq!(s.sample_count(), 3 * 1152);
        assert_eq!(s.data, [frame(), frame(), frame()].concat());
    }

    #[test]
    fn drops_xing_frame() {
        let mut info = frame();
        info[4 + 17..4 + 21].copy_from_slice(b"Info");
        let file = [info, frame(), frame()].concat();
        let s = parse(&file).unwrap();
        assert_eq!(s.frame_count, 2);
    }
}
