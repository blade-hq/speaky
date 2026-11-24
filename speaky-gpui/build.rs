fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/windows-dialogs.rc");
        println!("cargo:rerun-if-changed=assets/speaky.ico");
        embed_resource::compile("assets/windows-dialogs.rc", embed_resource::NONE)
            .manifest_required()
            .expect("could not embed Windows resources");
    }
}
