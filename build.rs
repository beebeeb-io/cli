//! Windows gives a process's main thread a 1 MB stack by default (Linux: 8 MB). The async `main` future and clap's
//! command tree overflow that in a debug build (task 2431), so the MSVC binary asks the linker for 8 MB. This is a link
//! setting only: `main`, exit codes, Ctrl-C handling and stdout/stderr are untouched, and other targets get no change.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    }
}
