#![allow(rustdoc::missing_crate_level_docs)] // it's an example

#[cfg(any(target_os = "linux", target_os = "android"))]
mod app;

#[cfg(any(target_os = "linux", target_os = "android"))]
fn main() -> std::io::Result<()> {
    app::run()
}

// Do not check `app` on unsupported platforms when check "--all-features" is used in CI.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn main() {
    #![expect(clippy::print_stdout)]
    println!("This example only supports Linux.");
}
