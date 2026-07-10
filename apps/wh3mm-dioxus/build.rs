use std::env;

const APP_ICON_PATH: &str = "assets/modmanager.ico";

fn main() {
    println!("cargo:rerun-if-changed={APP_ICON_PATH}");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    winresource::WindowsResource::new()
        .set_icon(APP_ICON_PATH)
        .compile()
        .expect("failed to embed the WH3 Mod Manager Windows icon");
}
