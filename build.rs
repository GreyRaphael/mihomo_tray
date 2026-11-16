use embed_manifest::{
    embed_manifest,
    manifest::{ActiveCodePage, DpiAwareness, SupportedOS},
    new_manifest,
};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest = new_manifest("GreyRaphael.MihomoTray")
        .active_code_page(ActiveCodePage::Utf8)
        .dpi_awareness(DpiAwareness::PerMonitorV2)
        .supported_os(SupportedOS::Windows10..=SupportedOS::Windows10);
    if let Err(err) = embed_manifest(manifest) {
        println!("cargo:warning=failed to embed Windows manifest: {err}");
    }
}
