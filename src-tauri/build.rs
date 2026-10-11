include!("../crates/dex-core/build_commit.rs");

fn main() {
    emit_commit();
    // S5-A: a `desktop` / `mobile` cfg for the whole crate. tauri-build itself only gates its own
    // code; the split (dex-core, desktop plugins) is ours, so publish the flags here.
    let mobile = matches!(
        std::env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("android") | Ok("ios")
    );
    println!("cargo:rustc-check-cfg=cfg(mobile, desktop)");
    println!("cargo:rustc-cfg={}", if mobile { "mobile" } else { "desktop" });
    tauri_build::build()
}
