fn main() {
    println!("cargo:rerun-if-changed=packaging/windows/zfs-send-explore.rc");
    println!("cargo:rerun-if-changed=packaging/windows/zfs-send-explore-windows.exe.manifest");

    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows") {
        let result = embed_resource::compile_for(
            "packaging/windows/zfs-send-explore.rc",
            ["zfs-send-explore-windows"],
            embed_resource::NONE,
        );
        if let Err(error) = result.manifest_required() {
            panic!("could not compile the Windows application resources: {error}");
        }
    }
}
