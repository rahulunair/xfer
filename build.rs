use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-env-changed=LEVEL_ZERO_INCLUDE");

    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .allowlist_function("ze.*")
        .allowlist_type("ze_.*")
        .allowlist_var("ZE_.*")
        .default_enum_style(bindgen::EnumVariation::Consts)
        .prepend_enum_name(false)
        .layout_tests(false);

    if let Ok(include_dir) = env::var("LEVEL_ZERO_INCLUDE") {
        builder = builder.clang_arg(format!("-I{include_dir}"));
    }

    if let Ok(output) = Command::new("pkg-config")
        .args(["--cflags", "level-zero"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for flag in stdout.split_whitespace() {
                builder = builder.clang_arg(flag);
            }
        }
    }

    let bindings = builder
        .generate()
        .expect("failed to generate Level Zero bindings from level_zero/ze_api.h");

    let out_path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    bindings
        .write_to_file(out_path.join("level_zero_bindings.rs"))
        .expect("failed to write generated Level Zero bindings");

    println!("cargo:rustc-link-lib=dylib=ze_loader");
}
