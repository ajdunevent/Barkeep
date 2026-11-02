fn main() {
    glib_build_tools::compile_resources(
        &["resources"],
        "resources/cardigan.gresource.xml",
        "cardigan.gresource",
    );
}
