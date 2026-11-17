fn main() {
    println!("cargo:rerun-if-changed=assets/rustsearch.ico");
    println!("cargo:rerun-if-changed=assets/windows.rc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/windows.rc", embed_resource::NONE)
            .manifest_required()
            .expect("embed RustSearch application icon and version metadata");
    }
}
