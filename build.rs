fn main() {
    const ICON: &str = "assets/calibrator.ico";

    println!("cargo:rerun-if-changed={ICON}");
    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() != Some(std::ffi::OsStr::new("windows")) {
        return;
    }

    winresource::WindowsResource::new()
        .set_icon(ICON)
        .compile()
        .expect("failed to compile Windows application resources");
}
