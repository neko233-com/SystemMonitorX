fn main() {
    // rust-i18n reads catalogs in its macro; Cargo must watch those inputs
    // explicitly so JSON-only edits reach incremental release builds.
    println!("cargo:rerun-if-changed=locales");
    println!("cargo:rerun-if-changed=assets/app.ico");
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/update-helper.manifest");
        let manifest = std::path::Path::new("assets/update-helper.manifest")
            .canonicalize()
            .expect("helper manifest");
        println!("cargo:rustc-link-arg-bin=SystemMonitorXUpdate=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=SystemMonitorXUpdate=/MANIFESTINPUT:{}",
            manifest.display()
        );
        let mut resource = winresource::WindowsResource::new();
        resource
            .set("ProductName", "SystemMonitorX")
            .set("FileDescription", "SystemMonitorX — Windows System Monitor")
            .set("CompanyName", "neko233-com");
        // GPUI embeds the asInvoker, Windows 10/11 and PerMonitorV2 manifest.
        // A second RT_MANIFEST/id 1 here would cause CVT1100 at final link.
        if std::path::Path::new("assets/app.ico").exists() {
            resource.set_icon("assets/app.ico");
        }
        resource
            .compile()
            .expect("compile Windows application resources");
    }
}
