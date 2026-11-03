fn main() {
    // Increase stack size to 32 MB to handle large hierarchies
    #[cfg(target_os = "windows")]
    println!("cargo:rustc-link-arg=/STACK:33554432");
}
