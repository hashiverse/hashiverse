fn main() {
    #[cfg(target_os = "unknown")]
    {
        println!("target_os is 'unknown'");
    }
    
    #[cfg(target_os = "wasi")]
    {
        println!("target_os is 'wasi'");
    }
    
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        println!("WASM32 + unknown");
    }
    
    #[cfg(all(target_arch = "wasm32", target_os = "wasi"))]
    {
        println!("WASM32 + wasi");
    }
}
