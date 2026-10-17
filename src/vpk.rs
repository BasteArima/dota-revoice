//! VPK v2: чтение паков игры и запись своих.
//!
//! Пишем «одним файлом»: все данные лежат прямо в `pakNN_dir.vpk`
//! (индекс архива 0x7FFF). Так устроены моды, которые сейчас работают
//! в `dota_russian`: без подписи, с секцией из трёх MD5.
//! Читаем и такие, и многотомные паки Valve (`pak01_dir.vpk` + `pak01_000.vpk`…).

use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use md5::{Digest, Md5};
use crate::{tr, trf};

const SIGNATURE: u32 = 0x55AA_1234;
const VERSION: u32 = 2;
const HEADER_SIZE: usize = 28;
const EMBEDDED_ARCHIVE: u16 = 0x7FFF;
const ENTRY_TERMINATOR: u16 = 0xFFFF;

#[derive(Default)]
pub struct VpkBuilder {
    /// ext → каталог → имя → данные. BTreeMap даёт стабильный порядок.
    tree: BTreeMap<String, BTreeMap<String, BTreeMap<String, Vec<u8>>>>,
}

impl VpkBuilder {
    /// `path` — путь внутри игры с расширением, например
    /// `sounds/vo/bounty_hunter/bount_kill_01.vsnd_c`.
    pub fn add(&mut self, path: &str, data: Vec<u8>) -> Result<()> {
        ensure!(path.is_ascii(), trf!("путь должен быть в ASCII: {path}", "the path must be ASCII: {path}"));
        let path = path.replace('\\', "/").to_ascii_lowercase();
        let Some((dir, file)) = path.rsplit_once('/') else {
            bail!(trf!("у файла нет каталога: {path}", "file has no directory: {path}"));
        };
        let Some((name, ext)) = file.rsplit_once('.') else {
            bail!(trf!("у файла нет расширения: {path}", "file has no extension: {path}"));
        };
        ensure!(!dir.is_empty() && !name.is_empty() && !ext.is_empty(), trf!("кривой путь: {path}", "bad path: {path}"));
        let files = self.tree.entry(ext.into()).or_default().entry(dir.into()).or_default();
        ensure!(files.insert(name.into(), data).is_none(), trf!("файл добавлен дважды: {path}", "file added twice: {path}"));
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.tree.values().flat_map(|d| d.values()).map(|f| f.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn write(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_to(&mut out).expect("запись в Vec не падает");
        out
    }

    /// Пишет пак потоком, не собирая его целиком в памяти.
    pub fn write_to(&self, w: impl Write) -> io::Result<()> {
        let tree: Tree = self
            .tree
            .iter()
            .map(|(ext, dirs)| {
                let dirs = dirs.iter().map(|(dir, files)| {
                    (dir.as_str(), files.iter().map(|(name, bytes)| (name.as_str(), Item::Inline(bytes))).collect())
                });
                (ext.as_str(), dirs.collect())
            })
            .collect();
        write_tree(&tree, w)
    }
}

/// Файл в оглавлении: данные внутри нашего файла или ссылка в том Valve (`pak01_00N.vpk`).
enum Item<'a> {
    Inline(&'a [u8]),
    External(&'a VpkEntry),
}

/// Расширение → каталог → имя → файл.
type Tree<'a> = BTreeMap<&'a str, BTreeMap<&'a str, BTreeMap<&'a str, Item<'a>>>>;

/// Наш пак поверх томов Valve: оглавление `base` как есть — его файлы остаются в
/// `pak01_00N.vpk`, игра находит их по имени нашего файла (он тоже `pak01_dir.vpk`),
/// а `own` — замены и новые файлы — лежат прямо в нашем файле. Весит как оглавление
/// Valve плюс свои фразы, а не как вся озвучка.
pub fn write_overlay(base: Option<&VpkReader>, own: &BTreeMap<String, Vec<u8>>, w: impl Write) -> Result<()> {
    let empty = BTreeMap::new();
    let entries = base.map_or(&empty, VpkReader::entries);
    // Данные, которые Valve хранит в самом `_dir`, к нам не попадут сами — переносим их внутрь.
    let mut embedded = BTreeMap::new();
    for (path, e) in entries {
        if e.archive == EMBEDDED_ARCHIVE
            && !own.contains_key(path)
            && let Some(base) = base
        {
            embedded.insert(path.as_str(), base.read(path)?);
        }
    }
    let mut tree: Tree = BTreeMap::new();
    for (path, e) in entries {
        match embedded.get(path.as_str()) {
            Some(bytes) => put(&mut tree, path, Item::Inline(bytes)),
            None if !own.contains_key(path) => put(&mut tree, path, Item::External(e)),
            None => {}
        }
    }
    for (path, bytes) in own {
        put(&mut tree, path, Item::Inline(bytes));
    }
    write_tree(&tree, w)?;
    Ok(())
}

/// Пак Valve: подписан и хранит MD5 своих томов. Наши паки — без подписи и без этой таблицы,
/// поэтому свой `pak01_dir.vpk` отличаем от файла Valve по одному заголовку.
pub fn is_signed(path: &Path) -> bool {
    let mut header = [0u8; HEADER_SIZE];
    let read = File::open(path).and_then(|mut f| f.read_exact(&mut header));
    let field = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
    read.is_ok() && field(0) == SIGNATURE && (field(4) > 0 || field(6) > 0)
}

/// `sounds/vo/axe/axe_kill_01.vsnd_c` → расширение, каталог, имя (пустые у Valve — пробел).
fn put<'a>(tree: &mut Tree<'a>, path: &'a str, item: Item<'a>) {
    let (dir, file) = path.rsplit_once('/').unwrap_or((" ", path));
    let (name, ext) = file.rsplit_once('.').unwrap_or((file, " "));
    tree.entry(ext).or_default().entry(dir).or_default().insert(name, item);
}

fn write_tree(items: &Tree, w: impl Write) -> io::Result<()> {
    let mut tree = Vec::new();
    let mut data_len = 0usize;
    for (ext, dirs) in items {
        push_cstr(&mut tree, ext);
        for (dir, files) in dirs {
            push_cstr(&mut tree, dir);
            for (name, item) in files {
                push_cstr(&mut tree, name);
                match item {
                    Item::Inline(bytes) => {
                        tree.extend_from_slice(&crc32fast::hash(bytes).to_le_bytes());
                        tree.extend_from_slice(&0u16.to_le_bytes()); // preload
                        tree.extend_from_slice(&EMBEDDED_ARCHIVE.to_le_bytes());
                        tree.extend_from_slice(&(data_len as u32).to_le_bytes());
                        tree.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                        tree.extend_from_slice(&ENTRY_TERMINATOR.to_le_bytes());
                        data_len += bytes.len();
                    }
                    Item::External(e) => {
                        tree.extend_from_slice(&e.crc.to_le_bytes());
                        tree.extend_from_slice(&(e.preload.len() as u16).to_le_bytes());
                        tree.extend_from_slice(&e.archive.to_le_bytes());
                        tree.extend_from_slice(&e.offset.to_le_bytes());
                        tree.extend_from_slice(&e.len.to_le_bytes());
                        tree.extend_from_slice(&ENTRY_TERMINATOR.to_le_bytes());
                        tree.extend_from_slice(&e.preload);
                    }
                }
            }
            tree.push(0);
        }
        tree.push(0);
    }
    tree.push(0);
    if data_len > u32::MAX as usize {
        return Err(io::Error::other(tr!("пак больше 4 ГБ не поместится в один файл", "a pack over 4 GB won't fit into one file")));
    }

    // MD5 «всего файла» считаем на лету по мере записи.
    let mut w = HashingWriter { inner: w, md5: Md5::new() };
    for v in [SIGNATURE, VERSION, tree.len() as u32, data_len as u32, 0, 48, 0] {
        w.write_all(&v.to_le_bytes())?;
    }
    w.write_all(&tree)?;
    for item in items.values().flat_map(|d| d.values()).flat_map(|f| f.values()) {
        if let Item::Inline(bytes) = item {
            w.write_all(bytes)?;
        }
    }
    // MD5 дерева, MD5 (пустой) таблицы архивов, MD5 всего, что выше.
    w.write_all(&Md5::digest(&tree))?;
    w.write_all(&Md5::digest([]))?;
    let whole = w.md5.finalize();
    w.inner.write_all(&whole)?;
    w.inner.flush()
}

struct HashingWriter<W> {
    inner: W,
    md5: Md5,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.md5.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn push_cstr(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
}

#[derive(Debug, Clone)]
pub struct VpkEntry {
    pub crc: u32,
    pub preload: Vec<u8>,
    pub archive: u16,
    pub offset: u32,
    pub len: u32,
}

/// Индекс пака в памяти; содержимое файлов читается по запросу.
pub struct VpkReader {
    dir_path: PathBuf,
    data_start: u64,
    entries: BTreeMap<String, VpkEntry>,
    archives: RefCell<HashMap<u16, File>>,
}

impl VpkReader {
    pub fn open(dir_path: impl AsRef<Path>) -> Result<Self> {
        let dir_path = dir_path.as_ref().to_path_buf();
        let mut f = File::open(&dir_path).with_context(|| trf!("не открывается {}", "can't open {}", dir_path.display()))?;
        let mut header = [0u8; HEADER_SIZE];
        f.read_exact(&mut header)?;
        let field = |i: usize| u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
        ensure!(field(0) == SIGNATURE, trf!("это не VPK: {}", "not a VPK: {}", dir_path.display()));
        ensure!(field(1) == VERSION, trf!("поддерживается только VPK v2, а тут v{}", "only VPK v2 is supported, this is v{}", field(1)));
        let mut tree = vec![0u8; field(2) as usize];
        f.read_exact(&mut tree)?;

        let entries = parse_tree(&tree).with_context(|| trf!("битое дерево в {}", "broken tree in {}", dir_path.display()))?;
        Ok(Self {
            dir_path,
            data_start: (HEADER_SIZE + tree.len()) as u64,
            entries,
            archives: RefCell::default(),
        })
    }

    pub fn entries(&self) -> &BTreeMap<String, VpkEntry> {
        &self.entries
    }

    pub fn contains(&self, path: &str) -> bool {
        self.entries.contains_key(path)
    }

    /// Содержимое файла целиком, с проверкой CRC.
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let e = self.entries.get(path).with_context(|| trf!("в паке нет {path}", "{path} isn't in the pack"))?;
        let mut out = Vec::with_capacity(e.preload.len() + e.len as usize);
        out.extend_from_slice(&e.preload);
        if e.len > 0 {
            let mut archives = self.archives.borrow_mut();
            let file = match archives.entry(e.archive) {
                Entry::Occupied(o) => o.into_mut(),
                Entry::Vacant(v) => {
                    let p = self.archive_path(e.archive)?;
                    v.insert(File::open(&p).with_context(|| trf!("нет архива {}", "archive {} is missing", p.display()))?)
                }
            };
            let base = if e.archive == EMBEDDED_ARCHIVE { self.data_start } else { 0 };
            file.seek(SeekFrom::Start(base + e.offset as u64))?;
            let start = out.len();
            out.resize(start + e.len as usize, 0);
            file.read_exact(&mut out[start..])?;
        }
        ensure!(crc32fast::hash(&out) == e.crc, trf!("CRC не сходится у {path}", "CRC mismatch for {path}"));
        Ok(out)
    }

    /// `pak01_dir.vpk` → `pak01_003.vpk`; встроенные данные — сам `_dir`.
    /// Отключённый `pak01_dir.vpk.off` ссылается на те же `pak01_00N.vpk`.
    fn archive_path(&self, index: u16) -> Result<PathBuf> {
        if index == EMBEDDED_ARCHIVE {
            return Ok(self.dir_path.clone());
        }
        let name = self.dir_path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let stem = name
            .strip_suffix(".off")
            .unwrap_or(name)
            .strip_suffix("_dir.vpk")
            .with_context(|| trf!("имя не оканчивается на _dir.vpk: {name}", "the name doesn't end with _dir.vpk: {name}"))?;
        Ok(self.dir_path.with_file_name(format!("{stem}_{index:03}.vpk")))
    }
}

fn parse_tree(tree: &[u8]) -> Result<BTreeMap<String, VpkEntry>> {
    let mut pos = 0usize;
    let cstr = |pos: &mut usize| -> Result<String> {
        let end = tree[*pos..].iter().position(|&b| b == 0).context(tr!("строка без нуля в конце", "string without a trailing zero"))? + *pos;
        let s = std::str::from_utf8(&tree[*pos..end])?.to_owned();
        *pos = end + 1;
        Ok(s)
    };
    let mut entries = BTreeMap::new();
    loop {
        let ext = cstr(&mut pos)?;
        if ext.is_empty() {
            break;
        }
        loop {
            let dir = cstr(&mut pos)?;
            if dir.is_empty() {
                break;
            }
            loop {
                let name = cstr(&mut pos)?;
                if name.is_empty() {
                    break;
                }
                let raw = tree.get(pos..pos + 18).context(tr!("обрезанная запись", "truncated entry"))?;
                let u16_at = |i: usize| u16::from_le_bytes([raw[i], raw[i + 1]]);
                let u32_at = |i: usize| u32::from_le_bytes(raw[i..i + 4].try_into().unwrap());
                ensure!(u16_at(16) == ENTRY_TERMINATOR, trf!("нет терминатора у {dir}/{name}.{ext}", "no terminator for {dir}/{name}.{ext}"));
                let preload_len = u16_at(4) as usize;
                pos += 18;
                let preload = tree.get(pos..pos + preload_len).context(tr!("обрезанный preload", "truncated preload"))?.to_vec();
                pos += preload_len;
                // Valve пишет пробел вместо пустого каталога/расширения.
                let mut path = String::new();
                if dir != " " {
                    path.push_str(&dir);
                    path.push('/');
                }
                path.push_str(&name);
                if ext != " " {
                    path.push('.');
                    path.push_str(&ext);
                }
                entries.insert(
                    path,
                    VpkEntry { crc: u32_at(0), preload, archive: u16_at(6), offset: u32_at(8), len: u32_at(12) },
                );
            }
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_file_layout() {
        let mut b = VpkBuilder::default();
        b.add("sounds/weapons/hero/wisp/relocate.vsnd_c", vec![1, 2, 3]).unwrap();
        let out = b.write();

        let tree_size = u32::from_le_bytes(out[8..12].try_into().unwrap()) as usize;
        let expected_tree = [
            b"vsnd_c\0sounds/weapons/hero/wisp\0relocate\0".as_slice(),
            &crc32fast::hash(&[1, 2, 3]).to_le_bytes(),
            &[0, 0, 0xFF, 0x7F, 0, 0, 0, 0, 3, 0, 0, 0, 0xFF, 0xFF, 0, 0, 0],
        ]
        .concat();
        assert_eq!(&out[28..28 + tree_size], expected_tree);
        assert_eq!(&out[28 + tree_size..28 + tree_size + 3], &[1, 2, 3]);
        assert_eq!(out.len(), 28 + tree_size + 3 + 48);
        let md5_at = out.len() - 48;
        assert_eq!(&out[out.len() - 16..], Md5::digest(&out[..md5_at + 32]).as_slice());
    }

    /// Как у Valve: оглавление в `pak01_dir.vpk`, данные — в томе `pak01_000.vpk`.
    /// Надстройка с тем же именем рядом с томом читает и свои файлы, и чужие из тома.
    #[test]
    fn overlay_references_valve_volumes() {
        let dir = std::env::temp_dir().join(format!("vpk_overlay_{}", std::process::id()));
        let (valve, ours) = (dir.join("valve"), dir.join("ours"));
        let volume = b"KILLLINE-LAUGHLINE".to_vec();
        for d in [&valve, &ours] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("pak01_000.vpk"), &volume).unwrap();
        }
        let kill = VpkEntry { crc: crc32fast::hash(b"KILLLINE"), preload: vec![], archive: 0, offset: 0, len: 8 };
        let laugh = VpkEntry { crc: crc32fast::hash(b"LAUGHLINE"), preload: vec![], archive: 0, offset: 9, len: 9 };
        let mut tree: Tree = BTreeMap::new();
        put(&mut tree, "sounds/vo/axe/axe_kill_01.vsnd_c", Item::External(&kill));
        put(&mut tree, "sounds/vo/axe/axe_laugh_01.vsnd_c", Item::External(&laugh));
        put(&mut tree, "scripts/readme.txt", Item::Inline(b"inline at valve"));
        write_tree(&tree, File::create(valve.join("pak01_dir.vpk")).unwrap()).unwrap();

        let base = VpkReader::open(valve.join("pak01_dir.vpk")).unwrap();
        assert_eq!(base.read("sounds/vo/axe/axe_laugh_01.vsnd_c").unwrap(), b"LAUGHLINE");
        let own = BTreeMap::from([("sounds/vo/axe/axe_kill_01.vsnd_c".to_owned(), b"MY KILL".to_vec())]);
        write_overlay(Some(&base), &own, File::create(ours.join("pak01_dir.vpk")).unwrap()).unwrap();

        let r = VpkReader::open(ours.join("pak01_dir.vpk")).unwrap();
        assert_eq!(r.entries().len(), 3);
        assert_eq!(r.read("sounds/vo/axe/axe_kill_01.vsnd_c").unwrap(), b"MY KILL");
        assert_eq!(r.read("sounds/vo/axe/axe_laugh_01.vsnd_c").unwrap(), b"LAUGHLINE", "чужое — из тома");
        assert_eq!(r.read("scripts/readme.txt").unwrap(), b"inline at valve", "встроенное у Valve — перенесено к нам");
        assert!(!is_signed(&ours.join("pak01_dir.vpk")), "наш пак — без подписи");
        drop((base, r));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_duplicates() {
        let mut b = VpkBuilder::default();
        b.add("a/b.c", vec![]).unwrap();
        assert!(b.add(r"A\B.c", vec![]).is_err());
    }

    #[test]
    fn roundtrip_through_reader() {
        let mut b = VpkBuilder::default();
        b.add("sounds/vo/a/one.vsnd_c", b"first".to_vec()).unwrap();
        b.add("sounds/vo/b/two.vsnd_c", b"second file".to_vec()).unwrap();
        b.add("scripts/x.txt", vec![]).unwrap();

        let dir = std::env::temp_dir().join(format!("vpk_roundtrip_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pak02_dir.vpk");
        b.write_to(File::create(&path).unwrap()).unwrap();

        let r = VpkReader::open(&path).unwrap();
        assert_eq!(r.entries().len(), 3);
        assert_eq!(r.read("sounds/vo/b/two.vsnd_c").unwrap(), b"second file");
        assert_eq!(r.read("sounds/vo/a/one.vsnd_c").unwrap(), b"first");
        assert_eq!(r.read("scripts/x.txt").unwrap(), b"");
        drop(r);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
