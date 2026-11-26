//! Embeds the AgnoPeriph Windows application icon into the executable resource table, and
//! records the release build version the binary was compiled as.

fn main() {
    println!("cargo:rerun-if-changed=assets/logo.ico");
    println!("cargo:rerun-if-env-changed=APP_VERSION");
    let version =
        std::env::var("APP_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_owned());
    println!("cargo:rustc-env=AGNO_APP_VERSION={version}");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/logo.ico");
        if let Err(err) = res.compile() {
            println!("cargo:warning=Could not embed Windows icon: {err}");
        }
    }
}
