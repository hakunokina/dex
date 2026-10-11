include!("../dex-core/build_commit.rs");

fn main() {
    emit_commit();
    // #427: MSVC stamps the link time and a fresh PDB id into every link, so a relink that changed
    // nothing gave other bytes; reproducible, the shell tells the same build by its content.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bin=dexd=/Brepro");
    }
}
