//! Звук: чтение файлов, ресемплинг, воспроизведение и запись.
//!
//! Внутри программы всё хранится как моно f32 на 44100 Гц — как озвучка в игре.
//! Устройства могут работать на другой частоте (часто 48000), поэтому на выходе
//! пересчитываем на лету линейной интерполяцией (для прослушки хватает),
//! а записанное приводим к 44100 нормальным ресемплером.

use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use rubato::{FftFixedIn, Resampler};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use crate::{tr, trf};

pub const RATE: u32 = 44100;
const PEAK_BINS: usize = 2048;

/// Звук в памяти плюс заранее посчитанная огибающая для волны.
#[derive(Clone)]
pub struct Clip {
    pub samples: Arc<Vec<f32>>,
    /// (минимум, максимум) по `PEAK_BINS` равным кускам.
    pub peaks: Arc<Vec<(f32, f32)>>,
    /// Громкость в LUFS, а для совсем коротких клипов — RMS в dBFS.
    pub loudness: f32,
}

impl Clip {
    pub fn new(samples: Vec<f32>) -> Self {
        let peaks = compute_peaks(&samples, PEAK_BINS);
        let mut clip = Self { samples: Arc::new(samples), peaks: Arc::new(peaks), loudness: 0.0 };
        clip.loudness = crate::process::loudness_lufs(&clip.samples).unwrap_or_else(|| clip.rms_db());
        clip
    }

    pub fn duration_secs(&self) -> f32 {
        self.samples.len() as f32 / RATE as f32
    }

    /// Пиковый уровень в dBFS.
    pub fn peak_db(&self) -> f32 {
        let peak = self.samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        to_db(peak)
    }

    /// Громкость (RMS) в dBFS — грубый ориентир, чтобы попасть в оригинал.
    pub fn rms_db(&self) -> f32 {
        if self.samples.is_empty() {
            return f32::NEG_INFINITY;
        }
        let sum: f64 = self.samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
        to_db((sum / self.samples.len() as f64).sqrt() as f32)
    }
}

pub fn to_db(amplitude: f32) -> f32 {
    if amplitude <= 0.0 { f32::NEG_INFINITY } else { 20.0 * amplitude.log10() }
}

pub fn compute_peaks(samples: &[f32], bins: usize) -> Vec<(f32, f32)> {
    if samples.is_empty() {
        return Vec::new();
    }
    (0..bins)
        .map(|i| {
            let start = i * samples.len() / bins;
            let end = ((i + 1) * samples.len() / bins).max(start + 1).min(samples.len());
            samples[start..end].iter().fold((0f32, 0f32), |(lo, hi), &s| (lo.min(s), hi.max(s)))
        })
        .collect()
}

/// Границы голоса в записи (в сэмплах): отрезаем тишину и шум по краям.
/// Порог — на 26 dB ниже самого громкого места, но не ниже шума комнаты +10 dB.
pub fn detect_voice(samples: &[f32]) -> Option<(usize, usize)> {
    const WINDOW: usize = RATE as usize / 100; // 10 мс
    const PAD_BEFORE: usize = 5; // 50 мс запаса до начала
    const PAD_AFTER: usize = 12; // 120 мс после — не съесть хвост слова
    let rms: Vec<f32> = samples
        .chunks(WINDOW)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let peak = rms.iter().copied().fold(0f32, f32::max);
    if peak <= 0.0 {
        return None;
    }
    let mut sorted = rms.clone();
    sorted.sort_by(f32::total_cmp);
    let floor = sorted[sorted.len() / 10];
    let threshold = (peak * 0.05).max(floor * 3.16).max(0.003);
    let first = rms.iter().position(|&r| r > threshold)?;
    let last = rms.iter().rposition(|&r| r > threshold)?;
    let start = first.saturating_sub(PAD_BEFORE) * WINDOW;
    let end = ((last + 1 + PAD_AFTER) * WINDOW).min(samples.len());
    (end > start).then_some((start, end))
}

/// Короткое нарастание и затухание по краям, чтобы на срезе не щёлкало.
pub fn fade_edges(samples: &mut [f32]) {
    let n = (RATE as usize / 200).min(samples.len() / 2); // 5 мс
    let len = samples.len();
    for i in 0..n {
        let k = i as f32 / n as f32;
        samples[i] *= k;
        samples[len - 1 - i] *= k;
    }
}

/// Читает mp3/wav/ogg/flac/m4a и голосовые (Ogg Opus), приводит к моно 44100 Гц.
pub fn decode_file(path: &Path) -> Result<Vec<f32>> {
    if crate::opus::is_ogg_opus(path) {
        return crate::opus::decode_file(path);
    }
    let file = File::open(path).with_context(|| trf!("не открывается {}", "can't open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .with_context(|| trf!("неизвестный формат: {}", "unknown format: {}", path.display()))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .context(tr!("в файле нет аудиодорожки", "the file has no audio track"))?;
    let track_id = track.id;
    let rate = track.codec_params.sample_rate.context(tr!("неизвестна частота дискретизации", "unknown sample rate"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context(tr!("кодек не поддерживается", "codec not supported"))?;

    let mut mono = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // Битый кадр — пропускаем, как делают плееры.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buf.copy_interleaved_ref(decoded);
        mono.extend(buf.samples().chunks(channels).map(|f| f.iter().sum::<f32>() / channels as f32));
    }
    resample(&mono, rate, RATE)
}

pub fn resample(input: &[f32], from: u32, to: u32) -> Result<Vec<f32>> {
    if from == to || input.is_empty() {
        return Ok(input.to_vec());
    }
    let mut r = FftFixedIn::<f32>::new(from as usize, to as usize, 1024, 2, 1)
        .map_err(|e| anyhow!(trf!("ресемплер: {e}", "resampler: {e}")))?;
    let expected = (input.len() as u64 * to as u64 / from as u64) as usize;
    let mut out = Vec::with_capacity(expected + 2048);
    let mut pos = 0;
    loop {
        let need = r.input_frames_next();
        if pos + need > input.len() {
            break;
        }
        let chunk = r.process(&[&input[pos..pos + need]], None).map_err(|e| anyhow!(trf!("ресемплер: {e}", "resampler: {e}")))?;
        out.extend_from_slice(&chunk[0]);
        pos += need;
    }
    if pos < input.len() {
        let chunk = r.process_partial(Some(&[&input[pos..]]), None).map_err(|e| anyhow!(trf!("ресемплер: {e}", "resampler: {e}")))?;
        out.extend_from_slice(&chunk[0]);
    }
    // Хвост, застрявший в задержке ресемплера.
    let tail = r.process_partial::<&[f32]>(None, None).map_err(|e| anyhow!(trf!("ресемплер: {e}", "resampler: {e}")))?;
    out.extend_from_slice(&tail[0]);
    let delay = r.output_delay().min(out.len());
    out.drain(..delay);
    out.truncate(expected);
    Ok(out)
}

/// Что сейчас играет — чтобы рисовать бегунок на нужной волне.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Track {
    Original,
    Mine,
    Compare,
    /// Служебный сигнал (отсчёт перед записью).
    Cue,
}

/// Короткий «бип» для отсчёта: синус с мягкими краями.
pub fn beep(hz: f32, secs: f32) -> Vec<f32> {
    let mut s: Vec<f32> = (0..(secs * RATE as f32) as usize)
        .map(|i| (i as f32 * hz * std::f32::consts::TAU / RATE as f32).sin() * 0.35)
        .collect();
    fade_edges(&mut s);
    s
}

#[derive(Default)]
struct PlayState {
    clip: Option<Arc<Vec<f32>>>,
    track: Option<Track>,
    /// Позиция в сэмплах клипа (44100 Гц), дробная из-за пересчёта частоты.
    pos: f64,
}

pub struct Player {
    _stream: cpal::Stream,
    state: Arc<Mutex<PlayState>>,
}

impl Player {
    pub fn new() -> Result<Self> {
        let device = cpal::default_host().default_output_device().context(tr!("нет устройства вывода звука", "no audio output device"))?;
        let config = device.default_output_config().context(tr!("устройство вывода не отдаёт настройки", "the output device doesn't report its settings"))?;
        let state = Arc::new(Mutex::new(PlayState::default()));
        let step = RATE as f64 / config.sample_rate().0 as f64;
        let stream = match config.sample_format() {
            SampleFormat::F32 => build_output::<f32>(&device, &config.into(), state.clone(), step)?,
            SampleFormat::I16 => build_output::<i16>(&device, &config.into(), state.clone(), step)?,
            SampleFormat::U16 => build_output::<u16>(&device, &config.into(), state.clone(), step)?,
            other => return Err(anyhow!(trf!("формат вывода {other:?} не поддерживается", "output format {other:?} isn't supported"))),
        };
        stream.play()?;
        Ok(Self { _stream: stream, state })
    }

    pub fn play(&self, track: Track, samples: Arc<Vec<f32>>) {
        let mut s = self.state.lock().unwrap();
        s.clip = Some(samples);
        s.track = Some(track);
        s.pos = 0.0;
    }

    pub fn stop(&self) {
        let mut s = self.state.lock().unwrap();
        s.clip = None;
        s.track = None;
    }

    /// Что играет и на какой доле (0..1).
    pub fn now_playing(&self) -> Option<(Track, f32)> {
        let s = self.state.lock().unwrap();
        let len = s.clip.as_ref()?.len().max(1);
        Some((s.track?, (s.pos / len as f64) as f32))
    }

    pub fn is_playing(&self, track: Track) -> bool {
        self.now_playing().is_some_and(|(t, _)| t == track)
    }
}

fn build_output<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    state: Arc<Mutex<PlayState>>,
    step: f64,
) -> Result<cpal::Stream> {
    let channels = config.channels as usize;
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let mut s = state.lock().unwrap();
            for frame in data.chunks_mut(channels) {
                let mut value = 0.0f32;
                if let Some(clip) = &s.clip {
                    let i = s.pos as usize;
                    if i + 1 < clip.len() {
                        let t = (s.pos - i as f64) as f32;
                        value = clip[i] * (1.0 - t) + clip[i + 1] * t;
                        s.pos += step;
                    } else {
                        s.clip = None;
                        s.track = None;
                    }
                }
                frame.fill(T::from_sample(value));
            }
        },
        |e| eprintln!("ошибка вывода звука: {e}"),
        None,
    )?;
    Ok(stream)
}

#[derive(Default)]
struct RecState {
    recording: bool,
    buffer: Vec<f32>,
    /// Пик с момента последнего опроса интерфейсом.
    level: f32,
    clipped: bool,
}

/// Микрофон. Поток открыт всё время, чтобы индикатор уровня работал и до записи.
pub struct Recorder {
    _stream: cpal::Stream,
    state: Arc<Mutex<RecState>>,
    device_rate: u32,
    pub device_name: String,
}

impl Recorder {
    pub fn input_device_names() -> Vec<String> {
        cpal::default_host()
            .input_devices()
            .map(|it| it.filter_map(|d| d.name().ok()).collect())
            .unwrap_or_default()
    }

    /// `name = None` — устройство по умолчанию.
    pub fn new(name: Option<&str>) -> Result<Self> {
        let host = cpal::default_host();
        let device = match name {
            Some(n) => host
                .input_devices()?
                .find(|d| d.name().is_ok_and(|dn| dn == n))
                .with_context(|| trf!("микрофон «{n}» не найден", "microphone “{n}” not found"))?,
            None => host.default_input_device().context(tr!("нет микрофона", "no microphone"))?,
        };
        let device_name = device.name().unwrap_or_else(|_| tr!("микрофон", "microphone").into());
        let config = device.default_input_config().context(tr!("микрофон не отдаёт настройки", "the microphone doesn't report its settings"))?;
        let state = Arc::new(Mutex::new(RecState::default()));
        let stream = match config.sample_format() {
            SampleFormat::F32 => build_input::<f32>(&device, &config.clone().into(), state.clone())?,
            SampleFormat::I16 => build_input::<i16>(&device, &config.clone().into(), state.clone())?,
            SampleFormat::U16 => build_input::<u16>(&device, &config.clone().into(), state.clone())?,
            other => return Err(anyhow!(trf!("формат микрофона {other:?} не поддерживается", "microphone format {other:?} isn't supported"))),
        };
        stream.play()?;
        Ok(Self { _stream: stream, state, device_rate: config.sample_rate().0, device_name })
    }

    pub fn start(&self) {
        let mut s = self.state.lock().unwrap();
        s.buffer.clear();
        s.clipped = false;
        s.recording = true;
    }

    pub fn is_recording(&self) -> bool {
        self.state.lock().unwrap().recording
    }

    /// Останавливает запись и возвращает её в 44100 Гц.
    pub fn stop(&self) -> Result<Vec<f32>> {
        let raw = {
            let mut s = self.state.lock().unwrap();
            s.recording = false;
            std::mem::take(&mut s.buffer)
        };
        resample(&raw, self.device_rate, RATE)
    }

    pub fn cancel(&self) {
        let mut s = self.state.lock().unwrap();
        s.recording = false;
        s.buffer.clear();
    }

    /// Пик с прошлого вызова (0..1) и был ли перегруз в текущей записи.
    pub fn take_level(&self) -> (f32, bool) {
        let mut s = self.state.lock().unwrap();
        let level = std::mem::take(&mut s.level);
        (level, s.clipped)
    }

    /// Огибающая того, что уже записано, — для живой волны.
    pub fn live_peaks(&self, bins: usize) -> (Vec<(f32, f32)>, f32) {
        let s = self.state.lock().unwrap();
        (compute_peaks(&s.buffer, bins), s.buffer.len() as f32 / self.device_rate as f32)
    }
}

fn build_input<T: SizedSample>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    state: Arc<Mutex<RecState>>,
) -> Result<cpal::Stream>
where
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            let mut s = state.lock().unwrap();
            for frame in data.chunks(channels) {
                let v = frame.iter().map(|&x| x.to_sample::<f32>()).sum::<f32>() / channels as f32;
                s.level = s.level.max(v.abs());
                if s.recording {
                    if v.abs() >= 0.99 {
                        s.clipped = true;
                    }
                    s.buffer.push(v);
                }
            }
        },
        |e| eprintln!("ошибка микрофона: {e}"),
        None,
    )?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_keeps_length_and_tone() {
        // 1 секунда синуса 440 Гц на 48000 → 44100.
        let src: Vec<f32> = (0..48000).map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48000.0).sin() * 0.5).collect();
        let out = resample(&src, 48000, 44100).unwrap();
        assert_eq!(out.len(), 44100);
        // Сравниваем середину с эталонным синусом на новой частоте.
        let max_err = (10000..30000)
            .map(|i| (out[i] - (i as f32 * 440.0 * std::f32::consts::TAU / 44100.0).sin() * 0.5).abs())
            .fold(0f32, f32::max);
        assert!(max_err < 0.02, "ошибка {max_err}");
    }

    #[test]
    fn finds_voice_between_silences() {
        // 0.5 с тихого шума, 1 с «голоса», 0.5 с шума.
        let mut s = Vec::new();
        let noise = |i: usize| ((i * 7919 % 1000) as f32 / 1000.0 - 0.5) * 0.002;
        s.extend((0..22050).map(noise));
        s.extend((0..44100).map(|i| (i as f32 * 220.0 * std::f32::consts::TAU / 44100.0).sin() * 0.4));
        s.extend((0..22050).map(noise));
        let (start, end) = detect_voice(&s).unwrap();
        let (a, b) = (start as f32 / 44100.0, end as f32 / 44100.0);
        assert!((0.43..=0.5).contains(&a), "начало {a}");
        assert!((1.5..=1.64).contains(&b), "конец {b}");
        assert_eq!(detect_voice(&[0.0; 4410]), None);
    }

    #[test]
    fn decodes_example_mp3() {
        let p = Path::new("example/sounds/vo/bounty_hunter/bount_kill_01.mp3");
        if !p.exists() {
            return; // выгрузки из игры нет в репозитории
        }
        let s = decode_file(p).unwrap();
        let secs = s.len() as f32 / RATE as f32;
        assert!((2.8..3.0).contains(&secs), "{secs}");
    }
}
