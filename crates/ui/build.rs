//! Embeds the AgnoPeriph Windows application icon into the executable resource table.

fn main() {
    println!("cargo:rerun-if-changed=assets/logo.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/logo.ico");
        if let Err(err) = res.compile() {
            println!("cargo:warning=Could not embed Windows icon: {err}");
        }
    }
}
