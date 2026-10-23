fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/gpui-notes.ico");

    // Embed the app icon as resource ID 1, which gpui loads for the window
    // and taskbar. Release builds for Windows run on a Windows host.
    #[cfg(windows)]
    winresource::WindowsResource::new()
        .set_icon("assets/gpui-notes.ico")
        .compile()
        .expect("embed the Windows icon resource");
}
