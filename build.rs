use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    // Packaged apps load the FFmpeg libraries bundled next to the program; builds from
    // the repo also find them where scripts/build-ffmpeg.sh put them.
    match os.as_str() {
        "macos" => println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks"),
        "linux" => println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/lib"),
        _ => {}
    }
    if let Ok(dir) = env::var("FFMPEG_DIR")
        && os != "windows"
    {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}/lib");
    }
}
