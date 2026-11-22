fn main() {
    windows_resources("Mega Mix+ Mod Manager (command line)");
}

/// Icon and version info in the .exe (Windows targets only).
fn windows_resources(description: &str) {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/icon.ico")
        .set("FileDescription", description)
        .set("ProductName", "Mega Mix+ Mod Manager");
    if let Err(e) = res.compile() {
        println!("cargo:warning=couldn't add the Windows icon: {e}");
    }
}
