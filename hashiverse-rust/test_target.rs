fn main() {
    println!("target_os: {}", env!("CARGO_CFG_TARGET_OS"));
    println!("target_arch: {}", env!("CARGO_CFG_TARGET_ARCH"));
}
