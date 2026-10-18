//! Двоичный KV3 v5 (`05 33 56 4B`), сжатый LZ4: так Valve хранит описание звука (блок
//! CTRL у `.vsnd_c`) и звуковые события (`.vsndevts_c`). Полный разбор не нужен:
//! хватает распакованных кусков — таблицы строк и потока чисел, где ключи и строковые
//! значения идут по порядку (`DOTA_Item.BattleFury`, `type`, …, `vsnd_files`, путь).

/// Распакованный KV3. Заголовок бывает двух видов: с двумя сжатыми кусками (размеры со
/// смещения 72, данные со 120) и с одним (размеры со смещения 48, данные с 72).
pub(crate) struct Kv3 {
    /// Первый кусок: в начале — строки (`string_bytes` байт), разделённые нулём.
    first: Vec<u8>,
    /// Второй кусок (только у вида с двумя): числа — ссылки на строки и значения.
    second: Vec<u8>,
    string_bytes: usize,
}

const LZ4: u32 = 1;

pub(crate) fn decode(kv: &[u8]) -> Option<Kv3> {
    if kv.get(..4)? != [0x05, b'3', b'V', b'K'] || u32_at(kv, 20)? != LZ4 {
        return None;
    }
    let size = |at: usize| u32_at(kv, at).map(|v| v as usize);
    let two = || {
        let (u1, c1, u2, c2) = (size(72)?, size(76)?, size(80)?, size(84)?);
        let first = lz4_block(kv.get(120..120 + c1)?, u1)?;
        let second = lz4_block(kv.get(120 + c1..120 + c1 + c2)?, u2)?;
        Some(Kv3 { first, second, string_bytes: size(28)? })
    };
    two().or_else(|| Some(Kv3 { first: lz4_block(kv.get(72..72 + size(52)?)?, size(48)?)?, second: Vec::new(), string_bytes: 0 }))
}

impl Kv3 {
    /// Всё распакованное подряд — для поиска чисел и строк без разбора.
    pub(crate) fn bytes(&self) -> Vec<u8> {
        [self.first.as_slice(), &self.second].concat()
    }

    /// Звуковые события: имя → файлы (`sounds/items/blade_fury.vsnd`). Событие — строка,
    /// за которой в потоке чисел идёт ключ `type`; его файлы — пути `sounds/…` до
    /// следующего события.
    pub(crate) fn sound_events(&self) -> Vec<(String, Vec<String>)> {
        let strings: Vec<&str> = self.first.get(..self.string_bytes).unwrap_or_default().split(|&b| b == 0).map(|s| std::str::from_utf8(s).unwrap_or_default()).collect();
        let refs: Vec<&str> =
            self.second.as_chunks::<4>().0.iter().filter_map(|b| usize::try_from(i32::from_le_bytes(*b)).ok().and_then(|i| strings.get(i).copied())).collect();
        let mut events: Vec<(String, Vec<String>)> = Vec::new();
        for (i, s) in refs.iter().enumerate() {
            // Имя события — `Раздел.Имя`: так ключ `type` после числа-ссылки не спутать с событием.
            if refs.get(i + 1) == Some(&"type") && s.contains('.') && !s.contains('/') {
                events.push((s.to_string(), Vec::new()));
            } else if s.starts_with("sounds/")
                && let Some((_, files)) = events.last_mut()
            {
                files.push(s.to_string());
            }
        }
        events
    }
}

/// Один блок LZ4 (без кадра): литералы и повторы из уже распакованного. `None` — данные
/// испорчены или распаковались не в `size` байт.
pub(crate) fn lz4_block(src: &[u8], size: usize) -> Option<Vec<u8>> {
    // Длина 15 продолжается байтами, пока они равны 255.
    let length = |i: &mut usize, mut n: usize| -> Option<usize> {
        if n == 15 {
            loop {
                let b = *src.get(*i)?;
                *i += 1;
                n += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        Some(n)
    };
    let mut out = Vec::with_capacity(size);
    let mut i = 0;
    while i < src.len() {
        let token = src[i];
        i += 1;
        let literals = length(&mut i, (token >> 4) as usize)?;
        out.extend_from_slice(src.get(i..i + literals)?);
        i += literals;
        if i >= src.len() {
            break; // последний кусок — одни литералы
        }
        let offset = u16::from_le_bytes([*src.get(i)?, *src.get(i + 1)?]) as usize;
        i += 2;
        let matched = length(&mut i, (token & 15) as usize)? + 4;
        if offset == 0 || offset > out.len() || out.len() + matched > size {
            return None;
        }
        for _ in 0..matched {
            out.push(out[out.len() - offset]);
        }
    }
    (out.len() == size).then_some(out)
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lz4_literals_and_repeats() {
        // «abc» литералами, потом повтор с отступом 3 длиной 6, потом литерал «!».
        let src = [0x32, b'a', b'b', b'c', 3, 0, 0x10, b'!'];
        assert_eq!(lz4_block(&src, 10).as_deref(), Some(b"abcabcabc!".as_slice()));
        assert_eq!(lz4_block(&src, 11), None, "размер не сошёлся");
        assert_eq!(lz4_block(&[0x02, b'a', 9, 0], 6), None, "отступ дальше начала");
    }

    /// Только литералы: LZ4-блок, который распаковывается в `data` как есть.
    fn literal(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0xF0];
        let mut n = data.len() - 15;
        while n >= 255 {
            out.push(255);
            n -= 255;
        }
        out.push(n as u8);
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn sound_events_from_two_chunks() {
        let strings = b"DOTA_Item.BattleFury\0type\0vsnd_files\0sounds/items/blade_fury.vsnd\0Item.RoshansBanner.Cast\0sounds/items/lotus_grant.vsnd\0sounds/items/xp_fountain_channel.vsnd\0";
        // Ссылки: событие, type, vsnd_files, файл; второе событие и два файла; 99 — не строка.
        let refs: Vec<u8> = [0i32, 1, 2, 3, 4, 1, 2, 99, 5, 6].iter().flat_map(|r| r.to_le_bytes()).collect();
        let (c1, c2) = (literal(strings), literal(&refs));
        let mut kv = vec![0x05, b'3', b'V', b'K'];
        kv.resize(20, 0);
        kv.extend_from_slice(&LZ4.to_le_bytes());
        kv.resize(28, 0);
        kv.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        kv.resize(72, 0);
        for v in [strings.len(), c1.len(), refs.len(), c2.len()] {
            kv.extend_from_slice(&(v as u32).to_le_bytes());
        }
        kv.resize(120, 0);
        kv.extend_from_slice(&c1);
        kv.extend_from_slice(&c2);
        let events = decode(&kv).unwrap().sound_events();
        assert_eq!(
            events,
            [
                ("DOTA_Item.BattleFury".to_owned(), vec!["sounds/items/blade_fury.vsnd".to_owned()]),
                ("Item.RoshansBanner.Cast".to_owned(), vec!["sounds/items/lotus_grant.vsnd".to_owned(), "sounds/items/xp_fountain_channel.vsnd".to_owned()]),
            ]
        );
    }
}
