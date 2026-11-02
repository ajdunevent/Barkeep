//! Resolves cardigan.plugin.in's two placeholder paths
//! (@plugins_dir@/@plugin_prefs_dir@) into a real cardigan.plugin, via
//! the same `pkg-config --variable=... phosh-plugins` queries confirmed
//! directly on a real FLX1S (see cardigan.plugin.in's own doc comment
//! for the full reasoning and confirmed values).
//!
//! Writes its output to a fixed path in this crate's own source
//! directory (cardigan.plugin, alongside cardigan.plugin.in) rather
//! than Cargo's usual $OUT_DIR -- deliberately, since $OUT_DIR is a
//! build-specific, unpredictable hashed path
//! (target/<profile>/build/<crate>-<hash>/out/), and cargo-deb's own
//! `assets` list needs a fixed, known path to reference directly (it
//! isn't a target/release/-prefixed Cargo-built artifact in the usual
//! sense cargo-deb auto-resolves, just a plain file this build script
//! happens to produce as a side effect). cardigan.plugin is therefore a
//! generated file living inside the repo's own directory structure --
//! worth adding to .gitignore, not something to commit as source.
//!
//! Deliberately does NOT hard-fail (panic / non-zero exit, which would
//! fail the whole `cargo build`) when pkg-config or the phosh-plugins
//! module itself aren't available -- only emits a `cargo:warning=...`
//! and writes cardigan.plugin with the raw, unresolved @...@
//! placeholders still in it. Reasoning: phosh-dev (which provides the
//! phosh-plugins.pc pkg-config file queried below) is a real,
//! confirmed-necessary dependency for actually *packaging* this crate,
//! but has no reason to be a hard requirement for ordinary `cargo
//! build`/`cargo check` iteration -- e.g. on a development machine
//! that isn't the target device at all, or isn't running Phosh, which
//! shouldn't be blocked from compiling and type-checking this crate's
//! own Rust code just because one unrelated metadata file couldn't be
//! fully resolved. The unresolved file is fine for that case, since
//! nothing else in this crate reads cardigan.plugin at all -- it's
//! Phosh's own runtime, not this crate's own compiled code, that
//! parses it. Only an actual packaging step run on/for the real target
//! device (where phosh-dev is a confirmed, real Build-Depends) needs
//! the fully-resolved version.

use std::path::Path;

fn pkg_config_variable(var: &str) -> Option<String> {
    let output = std::process::Command::new("pkg-config")
        .arg(format!("--variable={var}"))
        .arg("phosh-plugins")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("Cargo always sets this");
    let template_path = Path::new(&manifest_dir).join("cardigan.plugin.in");
    let output_path = Path::new(&manifest_dir).join("cardigan.plugin");

    let template = std::fs::read_to_string(&template_path)
        .unwrap_or_else(|e| panic!("couldn't read {}: {e}", template_path.display()));

    let plugins_dir = pkg_config_variable("lockscreen_plugins_dir");
    let plugin_prefs_dir = pkg_config_variable("lockscreen_prefs_dir");

    let resolved = match (&plugins_dir, &plugin_prefs_dir) {
        (Some(p), Some(pp)) => template.replace("@plugins_dir@", p).replace("@plugin_prefs_dir@", pp),
        _ => {
            println!(
                "cargo:warning=cardigan-lockscreen: couldn't resolve @plugins_dir@/@plugin_prefs_dir@ via \
                 `pkg-config phosh-plugins` (is phosh-dev installed?) -- writing cardigan.plugin with the raw, \
                 unresolved placeholders still in it. This is fine for ordinary `cargo build`/`cargo check` \
                 iteration, but the resulting file isn't valid for actual packaging/installation until this \
                 build script runs somewhere phosh-dev is available."
            );
            template
        }
    };

    std::fs::write(&output_path, resolved)
        .unwrap_or_else(|e| panic!("couldn't write {}: {e}", output_path.display()));
}
