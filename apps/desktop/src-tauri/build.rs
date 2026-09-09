fn main() {
    // `WindowsAttributes::new_without_app_manifest` suppresses the manifest
    // `tauri-build` would compile into `resource.lib`, because
    // `embed_common_controls_manifest` below embeds an equivalent one across
    // more targets. A binary holds a single manifest resource, so the two
    // together are a `CVT1100: duplicate resource` link failure rather than a
    // merge. Everything else `tauri-build` puts in the resource file — icon,
    // version strings — is untouched.
    let attributes = tauri_build::Attributes::new()
        .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());

    // Mirrors what `tauri_build::build()` does with the error, which keeps the
    // build script free of the panicking calls the workspace lints forbid.
    if let Err(error) = tauri_build::try_build(attributes) {
        println!("{error:#}");
        std::process::exit(1);
    }

    embed_common_controls_manifest();
}

/// Gives every Windows target the Common Controls v6 dependency, not just the
/// app binary.
///
/// `tauri-plugin-dialog` links `rfd` with its `common-controls-v6` feature, so
/// anything linking this crate statically imports `TaskDialogIndirect` from
/// `ComCtl32.dll`. That export exists only in v6, and Windows loads v5 unless
/// an application manifest asks for v6. `tauri-build` embeds such a manifest
/// through `embed-resource`, which emits `rustc-link-arg-bins` — binaries
/// only. The test executables therefore load v5, cannot resolve the import,
/// and die at load time with STATUS_ENTRYPOINT_NOT_FOUND before a single test
/// runs.
///
/// This uses the unscoped `rustc-link-arg` because `rustc-link-arg-tests` does
/// not reach a library's own unit-test executable — only `tests/` integration
/// targets (verified against cargo 1.96, which is the pinned toolchain).
fn embed_common_controls_manifest() {
    let targets_msvc = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !targets_msvc {
        return;
    }

    // `env!` rather than `std::env::var`: cargo sets this when it compiles the
    // build script, so the path is resolved without a runtime unwrap.
    let crate_dir = env!("CARGO_MANIFEST_DIR");
    let manifest = std::path::Path::new(crate_dir).join("windows-common-controls-v6.manifest");

    println!("cargo::rerun-if-changed={}", manifest.display());
    println!("cargo::rustc-link-arg=/MANIFEST:EMBED");
    println!(
        "cargo::rustc-link-arg=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
