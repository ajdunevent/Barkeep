fn main() {
    glib_build_tools::compile_resources(
        &["resources"],
        "resources/barkeep.gresource.xml",
        "barkeep.gresource",
    );
}
