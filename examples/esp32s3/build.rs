fn main() {
    // esp-hal's linker script; it must come last.
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}
