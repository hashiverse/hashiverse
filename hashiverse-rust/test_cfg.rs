fn main() {
    #[cfg(target_os = "unknown")]
    {
        println!("target_os is 'unknown'");
    }
    
    #[cfg(target_os = "wasi")]
    {
        println!("target_os is 'wasi'");
    }
}
