fn main() {
    // Increase stack size to 32 MB to handle large VHDL hierarchies
    #[cfg(target_os = "windows")]
    println!("cargo:rustc-link-arg=/STACK:33554432");
}
