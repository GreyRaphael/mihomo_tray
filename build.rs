const WINDOWS_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <assemblyIdentity type="win32" name="GreyRaphael.MihomoTray" version="0.3.4.0" processorArchitecture="*" />
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*" />
    </dependentAssembly>
  </dependency>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}" />
    </application>
  </compatibility>
  <asmv3:application>
    <asmv3:windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
    </asmv3:windowsSettings>
  </asmv3:application>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let has_windres = cfg!(windows)
        || std::process::Command::new("x86_64-w64-mingw32-windres")
            .arg("--version")
            .output()
            .is_ok();
    if target_env != "msvc" && !has_windres {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/app.ico");
    res.set_manifest(WINDOWS_MANIFEST);
    if let Err(err) = res.compile() {
        if cfg!(windows) {
            panic!("failed to compile Windows resources: {err}");
        }
        println!("cargo:warning=failed to compile Windows resources: {err}");
    }
}
