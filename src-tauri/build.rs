fn main() {
    let target = std::env::var("TARGET").unwrap();
    // ponytail: only the two targets we ship; add more folders from the cactus release as needed.
    let dir = match target.as_str() {
        "aarch64-apple-darwin" => "macos-arm64",
        "aarch64-linux-android" => "android-arm64",
        t => panic!("no libcactus.a vendored for {t}"),
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../vendor");
    println!("cargo:rustc-link-search=native={}", root.join(dir).display());
    println!("cargo:rustc-link-lib=static=cactus");
    if target.contains("apple") {
        println!("cargo:rustc-link-lib=c++");
    } else {
        // Plain -l so the NDK linker resolves these from its own sysroot.
        println!("cargo:rustc-link-lib=c++_static");
        println!("cargo:rustc-link-lib=c++abi");
    }
    println!("cargo:rerun-if-changed=../vendor");
    tauri_build::build()
}
