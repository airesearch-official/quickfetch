fn main() {
    tauri_build::build();
    // Tauri's application manifest does not cover Rust's unit-test executable.
    // Workers reference dialog APIs, which require Common Controls version 6.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'");
        // The application binary already embeds Tauri's manifest resource.
        println!("cargo:rustc-link-arg-bins=/MANIFEST:NO");
    }
}
