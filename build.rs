fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/Icon.ico");
        
        if std::env::var("NUM_JOBS").is_ok() && cfg!(target_os = "linux") {
            res.set_toolkit_path("/usr/bin");
            res.set_windres_path("x86_64-w64-mingw32-windres");
            res.set_ar_path("x86_64-w64-mingw32-ar");
        }

        res.compile().expect("Failed to compile Windows resources");
    }
}