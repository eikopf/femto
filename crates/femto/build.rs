use std::{env, path::PathBuf, process::Command};
fn output(command: &mut Command) -> String {
    let result = command.output().expect("native TLS build command");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).expect("UTF-8 command output")
}
fn main() {
    println!("cargo::rerun-if-env-changed=CARGO_FEATURE_NATIVE_MBEDTLS_TESTS");
    if env::var_os("CARGO_FEATURE_NATIVE_MBEDTLS_TESTS").is_none() {
        return;
    }
    let version = output(Command::new("pkg-config").args(["--modversion", "mbedtls"]));
    assert_eq!(
        version.trim(),
        "3.6.7",
        "Use scripts/check-native-tls with pinned Nix dependencies"
    );
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let source = root.join("tests/native-tls/bridge.c");
    println!("cargo::rerun-if-changed={}", source.display());
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let flags = output(Command::new("pkg-config").args(["--cflags", "mbedtls"]));
    output(
        Command::new("cc")
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-c"])
            .args(flags.split_whitespace())
            .arg(&source)
            .arg("-o")
            .arg(out.join("bridge.o")),
    );
    output(
        Command::new("ar")
            .arg("crs")
            .arg(out.join("libfemto_native_tls.a"))
            .arg(out.join("bridge.o")),
    );
    println!("cargo::rustc-link-search=native={}", out.display());
    println!("cargo::rustc-link-lib=static=femto_native_tls");
    for flag in
        output(Command::new("pkg-config").args(["--libs", "mbedtls", "mbedx509", "mbedcrypto"]))
            .split_whitespace()
    {
        if let Some(path) = flag.strip_prefix("-L") {
            println!("cargo::rustc-link-search=native={path}");
        } else if let Some(lib) = flag.strip_prefix("-l") {
            println!("cargo::rustc-link-lib={lib}");
        } else {
            panic!("unexpected native TLS linker flag");
        }
    }
}
