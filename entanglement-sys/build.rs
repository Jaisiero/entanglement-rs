use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ent_root = manifest_dir.join("..").join("deps").join("Entanglement");
    let src_dir = ent_root.join("src");
    let include_dir = ent_root.join("include");

    // Collect all .cpp files
    let cpp_files: Vec<PathBuf> = std::fs::read_dir(&src_dir)
        .expect("Failed to read deps/Entanglement/src/")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "cpp"))
        .collect();

    // Compile C++ sources
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++20")
        .include(&src_dir)
        .include(&include_dir);

    for file in &cpp_files {
        build.file(file);
    }

    if cfg!(target_os = "windows") {
        println!("cargo:rustc-link-lib=ws2_32");
    }

    let mut has_uring = false;

    if cfg!(target_os = "linux") {
        build.define("_GNU_SOURCE", None);
        // io_uring batched GSO sends
        if std::process::Command::new("pkg-config")
            .args(["--exists", "liburing"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            has_uring = true;
            build.define("ENTANGLEMENT_HAS_URING", None);
            println!("cargo:rustc-link-lib=uring");
        }
    }

    build.compile("entanglement");

    // Generate bindings with bindgen
    let header = include_dir.join("entanglement.h");
    let mut bindgen_builder = bindgen::Builder::default()
        .header(header.to_str().unwrap())
        .clang_arg(format!("-I{}", include_dir.display()))
        .clang_arg(format!("-I{}", src_dir.display()))
        .allowlist_type("ent_.*")
        .allowlist_function("ent_.*")
        .allowlist_var("ENT_.*");

    if has_uring {
        bindgen_builder = bindgen_builder.clang_arg("-DENTANGLEMENT_HAS_URING");
    }

    let bindings = bindgen_builder
        .generate()
        .expect("Failed to generate bindings");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_dir.join("bindings.rs"))
        .expect("Failed to write bindings");

    println!("cargo:rerun-if-changed={}", header.display());
    for file in &cpp_files {
        println!("cargo:rerun-if-changed={}", file.display());
    }
}
