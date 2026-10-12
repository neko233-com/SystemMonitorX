# Documentation screenshots

These English and Chinese pictures show the actual native Windows GPUI interface with anonymous illustrative data. They explain the product; they are not live telemetry, benchmark or hardware-coverage evidence. All eight images were visually reviewed. PNG text/EXIF metadata is absent.

Reproduce on Windows with the bundled VC runtime beside the executable:

```powershell
cargo build --release --locked --features ui-test --bin SystemMonitorX
.\SystemMonitorX.exe --docs-screenshots-dir C:\Temp\SystemMonitorX-docs
```

The opt-in renderer isolates its settings in that output directory. It never changes the normal application preference file. Only this process is inspected; screenshot-visible process, module paths, system and endpoint samples are illustrations. The screenshots and renderer are development assets and are not shipped in the MSI/portable payload.

简体中文：这些图片由实际 Windows 原生界面渲染，使用匿名示例数据介绍产品，不是实时遥测、性能或硬件覆盖证据。八张图片均已人工查看，没有 PNG 文本/EXIF 元数据。专用渲染入口隔离设置，只读取本进程，截图可见的数据是演示样例；截图和渲染工具不增加安装包载荷。
