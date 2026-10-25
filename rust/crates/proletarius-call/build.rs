use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(switch_evs_available)");
    println!("cargo:rerun-if-env-changed=SWITCH_BUILD_DIR");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_ARCH");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_NATIVE_CODEC_FAULT_TESTS");
    println!("cargo:rerun-if-env-changed=PROFILE");
    let fault_tests = env::var_os("CARGO_FEATURE_NATIVE_CODEC_FAULT_TESTS").is_some();
    if fault_tests {
        assert_eq!(
            env::var("PROFILE").as_deref(),
            Ok("debug"),
            "native codec fault hooks are forbidden in release builds"
        );
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos")
        || env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("aarch64")
    {
        assert!(!fault_tests, "native codec fault tests require macOS arm64");
        return;
    }

    let build_dir = env::var_os("SWITCH_BUILD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"))
                .join("../../../..")
                .join("build")
        });
    let evs_dir = if fault_tests {
        build_dir.join("native-codec-allocation/after/evs")
    } else {
        build_dir.join("evs")
    };
    let archive = evs_dir.join("libswitch_evs.a");
    println!("cargo:rerun-if-changed={}", archive.display());
    if !archive.is_file() {
        assert!(
            !fault_tests,
            "native fault-test archive missing: run scripts/test-native-codec-allocation.py"
        );
        println!(
            "cargo:warning=EVS codec archive is unavailable at {}; build it with scripts/build-evs.py",
            archive.display()
        );
        return;
    }

    println!("cargo:rustc-cfg=switch_evs_available");
    println!("cargo:rustc-link-search=native={}", evs_dir.display());
    println!("cargo:rustc-link-lib=static=switch_evs");
    println!("cargo:rustc-link-lib=m");
}
