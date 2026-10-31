fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        // Linux release archives carry GTK4 Layer Shell here; other GTK libraries stay system-provided.
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/lib");
    }
}
