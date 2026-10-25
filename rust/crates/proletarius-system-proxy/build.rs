fn main() {
    println!("cargo:rerun-if-changed=src/cfnetwork_proxy.c");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let mut build = cc::Build::new();
        build
            .file("src/cfnetwork_proxy.c")
            .warnings(true)
            .flag_if_supported("-Werror");
        build.compile("switch_cfnetwork_proxy");
        println!("cargo:rustc-link-lib=framework=CFNetwork");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
    }
}
