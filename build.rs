//! Иконка в exe для Windows: Проводник и ярлыки берут её из ресурсов файла,
//! а не из окна. На других системах иконку кладёт упаковка (см. release.yml).

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("не собрался ресурс с иконкой");
    }
}
