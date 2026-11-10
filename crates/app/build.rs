//! Compiles the Slint UI and, for Windows targets, embeds the application
//! icon, version information and manifest into the executable.

fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("compile the Slint UI");

    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=../../assets/icon/autoclicker.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_resources();
    }
}

fn embed_windows_resources() {
    // Single version source: the Cargo package version.
    let version = env!("CARGO_PKG_VERSION");
    let manifest = std::fs::read_to_string("app.manifest")
        .expect("read app.manifest")
        .replace("{{VERSION}}", &format!("{version}.0"));
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let manifest_path = out.join("app.manifest");
    std::fs::write(&manifest_path, manifest).expect("write manifest");

    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/icon/autoclicker.ico")
        .set_manifest_file(manifest_path.to_str().expect("UTF-8 path"))
        .set("ProductName", "Autoclicker")
        .set("FileDescription", "Autoclicker")
        .set("InternalName", "autoclicker")
        .set("OriginalFilename", "autoclicker.exe")
        .set(
            "LegalCopyright",
            "Copyright (c) 2026 nightmare. MIT License.",
        )
        .set("ProductVersion", version)
        .set("FileVersion", version);
    res.compile()
        .expect("embed Windows resources (icon, version info, manifest)");
}
