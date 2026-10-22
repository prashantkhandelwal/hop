fn main() {
    println!("cargo:rerun-if-changed=assets/hop.ico");
    #[cfg(windows)]
    winresource::WindowsResource::new()
        .set_icon("assets/hop.ico")
        .compile()
        .expect("Cannot embed the Hop executable icon");
}
