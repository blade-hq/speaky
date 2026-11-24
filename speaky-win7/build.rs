fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/speaky-win7.rc");
        println!("cargo:rerun-if-changed=assets/speaky-win7.manifest");
        println!("cargo:rerun-if-changed=../speaky-gpui/assets/speaky.ico");
        embed_resource::compile("assets/speaky-win7.rc", embed_resource::NONE)
            .manifest_required()
            .expect("could not embed Speaky resources");
    }
}
