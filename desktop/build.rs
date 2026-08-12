fn main() {
    println!("cargo:rerun-if-changed=windows.rc");
    println!("cargo:rerun-if-changed=ui/src-tauri/icons/icon.ico");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_resource::compile("windows.rc", embed_resource::NONE)
            .manifest_required()
            .expect("failed to embed the CruiseMesh Windows resources");
    }
}
