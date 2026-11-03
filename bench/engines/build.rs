// Links libvterm for the `libvterm` feature: the static archive from LIBVTERM_DIR (default: Homebrew's).
fn main() {
    println!("cargo:rerun-if-env-changed=LIBVTERM_DIR");
    if std::env::var_os("CARGO_FEATURE_LIBVTERM").is_some() {
        let dir =
            std::env::var("LIBVTERM_DIR").unwrap_or_else(|_| "/opt/homebrew/opt/libvterm".into());
        println!("cargo:rustc-link-search=native={dir}/lib");
        println!("cargo:rustc-link-lib=static=vterm");
    }
}
