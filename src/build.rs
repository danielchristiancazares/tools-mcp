fn main() {
    // If APP_VERSION is set in the environment at build time, expose it to the binary.
    if let Ok(version) = std::env::var("APP_VERSION") {
        println!("cargo:rustc-env=APP_VERSION={}", version);
    }
}
