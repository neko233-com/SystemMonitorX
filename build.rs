fn main() {
    #[cfg(windows)]
    {
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
