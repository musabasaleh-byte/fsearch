// Embeds the program icon when building with the MSVC toolchain (CI and normal Windows builds).
// The GNU cross-compile from Linux passes assets/fsearch.rc to the linker itself (see build-installer.sh).
fn main() {
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        embed_resource::compile("assets/fsearch.rc", embed_resource::NONE);
    }
    println!("cargo:rerun-if-changed=assets/fsearch.rc");
}
