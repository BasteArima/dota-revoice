//! Запись скомпилированного звука Source 2 (`.vsnd_c`) с mp3 внутри.
//!
//! Раскладка снята с файлов игры (`sounds/vo/...` и `sounds/weapons/...`):
//!
//! ```text
//! 0x00  u32 размер заголовка (всё до аудиопотока)
//! 0x04  u16 12 (версия заголовка), u16 1 (версия ресурса)
//! 0x08  u32 8 (смещение таблицы блоков), u32 2 (число блоков)
//! 0x10  "REDI" u32 rel_offset u32 size   — служебная инфа компилятора
//! 0x1C  "DATA" u32 rel_offset u32 size   — параметры звука, 40 байт
//! ....  REDI, DATA, затем mp3-поток как есть
//! ```
//!
//! Все смещения внутри файла относительные: считаются от позиции самого поля.
//! Блок фонем (движение губ) не пишем: у звуков способностей его тоже нет.

use crate::mp3::Mp3Stream;
use crate::tr;

const HEADER_VERSION: u16 = 12;
const RESOURCE_VERSION: u16 = 1;
/// Тип 2 (mp3) в битах 0–1, 16 бит в битах 2–6, формат 2 в битах 9–10.
/// Каналы (биты 7–8) и частота (биты 14+) подставляются отдельно.
const MP3_INFO_BITS: u32 = 2 | (16 << 2) | (2 << 9);
const DATA_SIZE: usize = 40;

/// Буфер с отложенными относительными ссылками на строки.
#[derive(Default)]
struct RelWriter {
    buf: Vec<u8>,
    string_refs: Vec<(usize, String)>,
}

impl RelWriter {
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn str_ref(&mut self, s: impl Into<String>) {
        self.string_refs.push((self.buf.len(), s.into()));
        self.u32(0);
    }

    fn patch_rel(&mut self, field: usize, target: usize) {
        let rel = (target - field) as u32;
        self.buf[field..field + 4].copy_from_slice(&rel.to_le_bytes());
    }

    /// Ставит блок `idx` таблицы REDI на текущую позицию.
    fn start_block(&mut self, idx: usize, count: u32) {
        let field = idx * 8;
        self.patch_rel(field, self.buf.len());
        self.buf[field + 4..field + 8].copy_from_slice(&count.to_le_bytes());
    }

    /// Дописывает пул строк и проставляет ссылки на них.
    fn finish(mut self) -> Vec<u8> {
        for (field, s) in std::mem::take(&mut self.string_refs) {
            let at = self.buf.len();
            self.buf.extend_from_slice(s.as_bytes());
            self.buf.push(0);
            self.patch_rel(field, at);
        }
        while !self.buf.len().is_multiple_of(8) {
            self.buf.push(0);
        }
        self.buf
    }
}

/// REDI в том же виде, что пишет `resourcecompiler`: входные файлы,
/// аргумент компиляции, версия компилятора и флаг `IsChildResource`.
fn build_redi(resource_path: &str, mp3_crc: u32) -> Vec<u8> {
    const BLOCKS: usize = 10;
    const INPUT_DEPS: usize = 0;
    const ARGUMENT_DEPS: usize = 2;
    const SPECIAL_DEPS: usize = 3;
    const EXTRA_INT: usize = 7;

    let mut w = RelWriter::default();
    w.buf.resize(BLOCKS * 8, 0);

    // Входные файлы: (имя, путь поиска, crc, флаги).
    w.start_block(INPUT_DEPS, 2);
    w.str_ref(format!("{resource_path}.vsnd"));
    w.str_ref("dota");
    w.u32(0);
    w.u32(1);
    w.str_ref(format!("{resource_path}.mp3"));
    w.str_ref("dota");
    w.u32(mp3_crc);
    w.u32(2);

    w.start_block(ARGUMENT_DEPS, 1);
    w.str_ref("___OverrideInputData___");
    w.str_ref("BinaryBlobArg");
    w.u32(0);
    w.u32(0);

    w.start_block(SPECIAL_DEPS, 1);
    w.str_ref("Sound Compiler Version");
    w.str_ref("CompileSound");
    w.u32(1);
    w.u32(0);

    w.start_block(EXTRA_INT, 1);
    w.str_ref("IsChildResource");
    w.u32(0);

    w.finish()
}

fn build_data(mp3: &Mp3Stream, loop_start: i32) -> [u8; DATA_SIZE] {
    let info = MP3_INFO_BITS | (mp3.channels << 7) | (mp3.sample_rate << 14);
    let fields: [u32; 10] = [
        info,
        loop_start as u32, // начало петли: у реплик 0, у звуков эффектов −1 (не зацикливать)
        mp3.sample_count(),
        mp3.duration_secs().to_bits(),
        0, // смещение блока фонем: нет
        0,
        0,
        mp3.data.len() as u32,
        0,
        0,
    ];
    let mut out = [0u8; DATA_SIZE];
    for (i, v) in fields.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

/// `resource_path` — путь внутри игры без расширения,
/// например `sounds/vo/bounty_hunter/bount_kill_01`.
pub fn build(resource_path: &str, mp3: &Mp3Stream) -> Vec<u8> {
    build_looped(resource_path, mp3, 0)
}

/// То же с заданным началом петли: −1 — играть один раз, 0 — зацикливать целиком.
/// Звуки эффектов берут его у оригинала (`loop_start`).
pub fn build_looped(resource_path: &str, mp3: &Mp3Stream, loop_start: i32) -> Vec<u8> {
    let redi = build_redi(resource_path, crc32fast::hash(&mp3.data));
    let data = build_data(mp3, loop_start);

    let redi_pos = 16 + 2 * 12;
    let data_pos = redi_pos + redi.len();
    let header_size = data_pos + data.len();

    let mut out = Vec::with_capacity(header_size + mp3.data.len());
    out.extend_from_slice(&(header_size as u32).to_le_bytes());
    out.extend_from_slice(&HEADER_VERSION.to_le_bytes());
    out.extend_from_slice(&RESOURCE_VERSION.to_le_bytes());
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    for (name, pos, size) in [(b"REDI", redi_pos, redi.len()), (b"DATA", data_pos, data.len())] {
        let field = out.len() + 4;
        out.extend_from_slice(name);
        out.extend_from_slice(&((pos - field) as u32).to_le_bytes());
        out.extend_from_slice(&(size as u32).to_le_bytes());
    }
    out.extend_from_slice(&redi);
    out.extend_from_slice(&data);
    out.extend_from_slice(&mp3.data);
    out
}

/// mp3-поток из `.vsnd_c` игры: всё после заголовка. Поле формата в DATA у новых
/// ресурсов (RED2 + CTRL) устроено иначе, поэтому mp3 узнаём по самому потоку.
pub fn mp3_of(vsnd_c: &[u8]) -> anyhow::Result<&[u8]> {
    let header = vsnd_c.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let stream = header.and_then(|h| vsnd_c.get(h..)).filter(|s| !s.is_empty());
    let Some(stream) = stream else { anyhow::bail!(tr!("в звуке нет аудиопотока", "the sound has no audio stream")) };
    let mp3 = stream.starts_with(b"ID3") || (stream.len() > 1 && stream[0] == 0xFF && stream[1] & 0xE0 == 0xE0);
    anyhow::ensure!(mp3, tr!("звук не в mp3", "the sound isn't mp3"));
    Ok(stream)
}

/// Несжатый звук: 16-битные сэмплы как есть.
pub struct Pcm {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

/// Достаёт несжатый звук (PCM 16 бит) — так лежат реплики немой Марси и почти все звуки
/// способностей и предметов. Блок DATA бывает двух видов:
/// - старый, 68 байт (Марси): частота (u16), тип (0 — PCM), число каналов;
/// - нынешний, 40 байт, как у mp3: битовое поле — тип в битах 0–1 (1 — wav), разрядность
///   в битах 2–6, каналы в битах 7–8, частота с бита 14.
///
/// В обоих со смещения 28 — размер потока в байтах; поток идёт сразу за заголовком.
/// У новых ресурсов (RED2) DATA пустой, а описание звука — в блоке CTRL (`ctrl_pcm`).
pub fn pcm_of(vsnd_c: &[u8]) -> anyhow::Result<Pcm> {
    let not_pcm = || anyhow::anyhow!(tr!("звук не в PCM 16 бит", "the sound isn't 16-bit PCM"));
    let header = u32_at(vsnd_c, 0).ok_or_else(not_pcm)? as usize;
    let Some(data) = block(vsnd_c, b"DATA").filter(|d| d.len() >= 32) else {
        let (sample_rate, channels) = ctrl_pcm(vsnd_c, header).ok_or_else(not_pcm)?;
        let stream = vsnd_c.get(header..).ok_or_else(not_pcm)?;
        let samples = stream.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)).collect();
        return Ok(Pcm { sample_rate, channels, samples });
    };
    let (sample_rate, channels) = if data.len() == DATA_SIZE {
        let bits = u32_at(data, 0).ok_or_else(not_pcm)?;
        anyhow::ensure!(bits & 3 == 1 && (bits >> 2) & 31 == 16, not_pcm());
        (bits >> 14, ((bits >> 7) & 3) as u16)
    } else {
        anyhow::ensure!(data[2] == 0, not_pcm());
        (u16::from_le_bytes([data[0], data[1]]) as u32, data[3] as u16)
    };
    anyhow::ensure!((1..=2).contains(&channels) && sample_rate > 0, not_pcm());
    let size = u32_at(data, 28).ok_or_else(not_pcm)? as usize;
    // В старом DATA со смещения 8 — число сэмплов: у PCM оно ровно сходится с размером
    // потока (так у всех таких звуков игры), а mp3 за PCM не примешь.
    if data.len() != DATA_SIZE {
        let count = u32_at(data, 8).ok_or_else(not_pcm)? as usize;
        anyhow::ensure!(count * channels as usize * 2 == size, not_pcm());
    }
    let stream = vsnd_c.get(header..header + size).ok_or_else(|| anyhow::anyhow!(tr!("обрезанный звук", "truncated sound")))?;
    let samples = stream.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)).collect();
    Ok(Pcm { sample_rate, channels, samples })
}

/// Частота и каналы несжатого звука нового ресурса. CTRL — двоичный KV3 v5, сжатый LZ4;
/// разбирать его целиком не нужно: в распакованных данных есть строка формата `PCM16`,
/// частота (одно из обычных значений) и число сэмплов N, у которого N × каналы × 2 —
/// ровно размер потока за заголовком. Так однозначно читаются все 387 таких звуков игры.
fn ctrl_pcm(vsnd_c: &[u8], header: usize) -> Option<(u32, u16)> {
    const RATES: [u32; 8] = [44100, 48000, 22050, 32000, 24000, 16000, 11025, 8000];
    let kv = crate::kv3::decode(block(vsnd_c, b"CTRL")?)?.bytes();
    if !kv.split(|&b| b == 0).any(|s| s == b"PCM16") {
        return None;
    }
    let size = vsnd_c.len().checked_sub(header)?;
    let ints: Vec<u32> = (0..kv.len().saturating_sub(3)).filter_map(|i| u32_at(&kv, i)).collect();
    let rate = ints.iter().copied().find(|v| RATES.contains(v))?;
    let fits = |ch: usize| ints.iter().any(|&n| n > 0 && n as usize * ch * 2 == size);
    match (fits(1), fits(2)) {
        (true, false) => Some((rate, 1)),
        (false, true) => Some((rate, 2)),
        _ => None,
    }
}

/// Начало петли звука из игры: −1 — играет один раз, 0 и больше — зациклен.
/// `None` — блока DATA нет (новые ресурсы хранят это в CTRL).
pub fn loop_start(vsnd_c: &[u8]) -> Option<i32> {
    let data = block(vsnd_c, b"DATA").filter(|d| d.len() >= 8)?;
    Some(u32_at(data, 4)? as i32)
}

/// Блок ресурса по тегу: таблица блоков начинается со смещения из байт 8..12
/// (от позиции 8), в ней по 12 байт — тег, смещение (от поля смещения) и размер.
pub(crate) fn block<'a>(res: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    let table = 8 + u32_at(res, 8)? as usize;
    let count = u32_at(res, 12)? as usize;
    (0..count).find_map(|i| {
        let pos = table + i * 12;
        if res.get(pos..pos + 4)? != tag {
            return None;
        }
        let start = pos + 4 + u32_at(res, pos + 4)? as usize;
        res.get(start..start + u32_at(res, pos + 8)? as usize)
    })
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Как у Марси: заголовок с одним блоком DATA на 68 байт, потом сэмплы.
    #[test]
    fn pcm_out_of_old_vsnd() {
        let samples: [i16; 4] = [0, -15, 1000, -32768];
        let mut f = Vec::new();
        f.extend_from_slice(&96u32.to_le_bytes()); // размер заголовка
        f.extend_from_slice(&[12, 0, 1, 0]); // версии
        f.extend_from_slice(&8u32.to_le_bytes()); // таблица блоков — с позиции 16
        f.extend_from_slice(&1u32.to_le_bytes());
        f.extend_from_slice(b"DATA");
        f.extend_from_slice(&8u32.to_le_bytes()); // данные блока — с позиции 28
        f.extend_from_slice(&68u32.to_le_bytes());
        let mut data = [0u8; 68];
        data[..4].copy_from_slice(&[0x44, 0xAC, 0, 1]); // 44100 Гц, PCM16, моно
        data[8..12].copy_from_slice(&4u32.to_le_bytes()); // 4 сэмпла
        data[28..32].copy_from_slice(&8u32.to_le_bytes());
        f.extend_from_slice(&data);
        assert_eq!(f.len(), 96);
        f.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
        let pcm = pcm_of(&f).unwrap();
        assert_eq!((pcm.sample_rate, pcm.channels), (44100, 1));
        assert_eq!(pcm.samples, samples);
        assert!(mp3_of(&f).is_err());
        assert!(pcm_of(&f[..100]).is_err(), "обрезанный поток");
    }

    /// Новый ресурс: DATA пустой, описание в CTRL (KV3 v5, LZ4 из одних литералов).
    #[test]
    fn pcm_out_of_red2_ctrl() {
        let samples: [i16; 4] = [5, -6, 700, -800];
        // Распакованное: строки, частота и число сэмплов (2 на канал — значит, стерео).
        let mut kv = b"m_nFormat\0PCM16\0\0\0".to_vec();
        kv.extend_from_slice(&44100u32.to_le_bytes());
        kv.extend_from_slice(&2u32.to_le_bytes());
        // LZ4: всё одним литералом (длина 15 + продолжение).
        let mut lz = vec![0xF0, (kv.len() - 15) as u8];
        lz.extend_from_slice(&kv);
        let mut ctrl = vec![0x05, b'3', b'V', b'K'];
        ctrl.resize(20, 0);
        ctrl.extend_from_slice(&1u32.to_le_bytes()); // LZ4
        ctrl.resize(48, 0);
        ctrl.extend_from_slice(&(kv.len() as u32).to_le_bytes());
        ctrl.extend_from_slice(&(lz.len() as u32).to_le_bytes());
        ctrl.resize(72, 0);
        ctrl.extend_from_slice(&lz);
        // Ресурс: таблица из двух блоков (DATA пустой, CTRL), потом поток.
        let header = 16 + 24 + ctrl.len();
        let mut f = Vec::new();
        f.extend_from_slice(&(header as u32).to_le_bytes());
        f.extend_from_slice(&[12, 0, 5, 0]);
        f.extend_from_slice(&8u32.to_le_bytes());
        f.extend_from_slice(&2u32.to_le_bytes());
        f.extend_from_slice(b"DATA");
        f.extend_from_slice(&20u32.to_le_bytes()); // к концу таблицы, размер 0
        f.extend_from_slice(&0u32.to_le_bytes());
        f.extend_from_slice(b"CTRL");
        f.extend_from_slice(&8u32.to_le_bytes()); // сразу за таблицей
        f.extend_from_slice(&(ctrl.len() as u32).to_le_bytes());
        f.extend_from_slice(&ctrl);
        assert_eq!(f.len(), header);
        f.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
        let pcm = pcm_of(&f).unwrap();
        assert_eq!((pcm.sample_rate, pcm.channels), (44100, 2));
        assert_eq!(pcm.samples, samples);
    }

    /// Как у звуков способностей: 40-байтный DATA с типом wav, стерео, 44100.
    #[test]
    fn pcm_out_of_current_vsnd() {
        let samples: [i16; 4] = [1, -2, 300, -400];
        let mut f = Vec::new();
        f.extend_from_slice(&68u32.to_le_bytes());
        f.extend_from_slice(&[12, 0, 1, 0]);
        f.extend_from_slice(&8u32.to_le_bytes());
        f.extend_from_slice(&1u32.to_le_bytes());
        f.extend_from_slice(b"DATA");
        f.extend_from_slice(&8u32.to_le_bytes());
        f.extend_from_slice(&40u32.to_le_bytes());
        let mut data = [0u8; 40];
        data[..4].copy_from_slice(&0x2b11_1941u32.to_le_bytes()); // wav, 16 бит, стерео, 44100
        data[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        data[28..32].copy_from_slice(&8u32.to_le_bytes());
        f.extend_from_slice(&data);
        assert_eq!(f.len(), 68);
        f.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
        let pcm = pcm_of(&f).unwrap();
        assert_eq!((pcm.sample_rate, pcm.channels), (44100, 2));
        assert_eq!(pcm.samples, samples);
        assert_eq!(loop_start(&f), Some(-1));
    }

    #[test]
    fn mp3_back_out_of_vsnd() {
        let mut data = vec![0xFF, 0xFB, 0x50, 0xC4];
        data.resize(500, 0x55);
        let mp3 = Mp3Stream { sample_rate: 44100, channels: 1, samples_per_frame: 1152, frame_count: 1, data };
        let f = build("sounds/vo/axe/axe_kill_01", &mp3);
        assert_eq!(mp3_of(&f).unwrap(), mp3.data.as_slice());
        assert!(mp3_of(&f[..f.len() - 500]).is_err(), "без потока");
        assert!(pcm_of(&f).is_err(), "mp3 — не PCM");
        assert_eq!(loop_start(&f), Some(0), "реплика не зациклена, но петля с начала");
        assert_eq!(loop_start(&build_looped("sounds/items/cheese", &mp3, -1)), Some(-1));
    }

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn layout_matches_game_files() {
        let mp3 = Mp3Stream {
            sample_rate: 44100,
            channels: 1,
            samples_per_frame: 1152,
            frame_count: 112,
            data: vec![0xAB; 23324],
        };
        let f = build("sounds/vo/bounty_hunter/bount_kill_01", &mp3);
        let header = u32_at(&f, 0) as usize;
        assert_eq!(f.len(), header + 23324);
        assert_eq!(&f[16..20], b"REDI");
        assert_eq!(u32_at(&f, 20), 0x14);
        assert_eq!(&f[28..32], b"DATA");
        let data = 32 + u32_at(&f, 32) as usize;
        assert_eq!(data + DATA_SIZE, header);
        // Значения из настоящего bount_kill_01.vsnd_c.
        assert_eq!(u32_at(&f, data), 0x2b11_04c2);
        assert_eq!(u32_at(&f, data + 8), 129024);
        assert_eq!(u32_at(&f, data + 12), 0x403b_3ee7);
        assert_eq!(u32_at(&f, data + 28), 23324);
    }
}
