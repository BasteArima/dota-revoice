//! Путь фразы целиком: папка с оригиналами → моя запись → корзина → сборка пака.
//! Без микрофона: вместо записи — синтетический тон.

use std::path::{Path, PathBuf};

use dota_revoice::audio::RATE;
use dota_revoice::project::{Project, Status};
use dota_revoice::vpk::{VpkBuilder, VpkReader};
use dota_revoice::{encode, pack};

fn tone(secs: f32, hz: f32) -> Vec<f32> {
    (0..(secs * RATE as f32) as usize).map(|i| (i as f32 * hz * std::f32::consts::TAU / RATE as f32).sin() * 0.3).collect()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dota_revoice_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Выгрузка «из игры»: X/sounds/vo/testhero/{test_kill_01,test_laugh_01}.mp3
fn fake_dump(root: &Path) -> PathBuf {
    let hero = root.join("sounds/vo/testhero");
    std::fs::create_dir_all(&hero).unwrap();
    for (name, hz) in [("test_kill_01", 220.0), ("test_laugh_01", 330.0)] {
        std::fs::write(hero.join(format!("{name}.mp3")), encode::encode_mp3(&tone(0.8, hz)).unwrap()).unwrap();
    }
    hero
}

#[test]
fn record_trash_and_pack() {
    let root = temp_dir("pipeline");
    let hero = fake_dump(&root);

    let mut p = Project::open(&hero).unwrap();
    assert_eq!(p.resource_dir, "sounds/vo/testhero");
    assert_eq!(p.export_root, root.join("export"));
    let cats: Vec<&str> = p.lines.iter().map(|l| l.category.as_str()).collect();
    assert_eq!(cats, ["kill", "laugh"]);

    // «Запись» первой фразы.
    let kill = p.lines[0].clone();
    encode::save_take(&p.export_mp3(&kill), &p.raw_wav(&kill), &tone(1.2, 440.0)).unwrap();
    p.mark_recorded(&kill).unwrap();
    assert_eq!(p.status(&kill), Status::Recorded);
    assert!(root.join("export/sounds/vo/testhero/test_kill_01.mp3").is_file());
    assert!(root.join("export/.raw/sounds/vo/testhero/test_kill_01.wav").is_file());

    // Пометка «переделать», обрезка и заметка переживают переоткрытие проекта.
    p.set_redo(&kill, true).unwrap();
    p.set_trim(&kill, Some((0.1, 1.0))).unwrap();
    p.set_note(&kill, "  убийство героя ").unwrap();
    let p2 = Project::open(&hero).unwrap();
    assert_eq!(p2.status(&p2.lines[0]), Status::Redo);
    assert_eq!(p2.trim(&p2.lines[0]), Some((0.1, 1.0)));
    assert_eq!(p2.note(&p2.lines[0]), "убийство героя");
    assert_eq!(p2.note(&p2.lines[1]), "");
    let mut p2 = p2;
    let fx = dota_revoice::process::Settings { denoise: true, gain_db: 3.5, pitch: -2.0 };
    p2.set_processing(&kill, fx).unwrap();
    assert_eq!(Project::open(&hero).unwrap().processing(&kill), fx);

    // Дубли: новая запись убирает прежнюю в дубли вместе с настройками.
    p2.archive_current(&kill).unwrap();
    encode::save_raw(&p2.raw_wav(&kill), &tone(0.5, 330.0)).unwrap();
    p2.set_trim(&kill, None).unwrap();
    p2.set_processing(&kill, Default::default()).unwrap();
    let takes = p2.takes(&kill);
    assert_eq!(takes.len(), 1);
    assert!((takes[0].secs - 1.2).abs() < 0.01, "старый дубль длиной 1.2 с");
    // Возвращаем старый: он снова текущий со своей обработкой, а новый ушёл в дубли.
    p2.restore_take(&kill, takes[0].id).unwrap();
    assert_eq!(p2.processing(&kill), fx);
    assert_eq!(p2.trim(&kill), Some((0.1, 1.0)));
    let takes = p2.takes(&kill);
    assert_eq!(takes.len(), 1);
    assert!((takes[0].secs - 0.5).abs() < 0.01, "в дублях теперь новая запись 0.5 с");
    p2.trash_take(&kill, takes[0].id).unwrap();
    assert!(p2.takes(&kill).is_empty());
    assert_eq!(p2.recorded_count(), 1);
    let mut p = p2;

    // Базовый пак «Valve» с обеими фразами — наша заменяет одну, вторая копируется.
    let mut base = VpkBuilder::default();
    base.add("sounds/vo/testhero/test_kill_01.vsnd_c", b"valve kill".to_vec()).unwrap();
    base.add("sounds/vo/testhero/test_laugh_01.vsnd_c", b"valve laugh".to_vec()).unwrap();
    let base_path = root.join("base/pak01_dir.vpk");
    std::fs::create_dir_all(base_path.parent().unwrap()).unwrap();
    std::fs::write(&base_path, base.write()).unwrap();

    let mine = pack::collect_export(&p.export_root).unwrap();
    assert_eq!(mine.keys().collect::<Vec<_>>(), ["sounds/vo/testhero/test_kill_01"]);
    let out = root.join("export/build/pak02_dir.vpk");
    let summary = pack::build(Some(&base_path), &mine, &out, |_| {}).unwrap();
    assert_eq!((summary.replaced, summary.copied), (1, 1));
    assert!(summary.not_in_base.is_empty());

    let r = VpkReader::open(&out).unwrap();
    assert_eq!(r.read("sounds/vo/testhero/test_laugh_01.vsnd_c").unwrap(), b"valve laugh");
    let vsnd = r.read("sounds/vo/testhero/test_kill_01.vsnd_c").unwrap();
    assert_eq!(&vsnd[16..20], b"REDI");

    // Корзина: файл уходит в .trash, статус сбрасывается.
    p.trash(&kill).unwrap();
    assert_eq!(p.status(&kill), Status::Empty);
    assert!(!p.export_mp3(&kill).exists());
    let trashed = std::fs::read_dir(root.join("export/.trash/sounds/vo/testhero")).unwrap().count();
    assert_eq!(trashed, 4, "mp3 и wav фразы + wav и json удалённого дубля");

    std::fs::remove_dir_all(root).unwrap();
}
