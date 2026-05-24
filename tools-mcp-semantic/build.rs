//! Build-time helper for the ORT linking story.
//!
//! When `ORT_LIB_LOCATION` is set (i.e. the user is linking against their own ONNX Runtime
//! build rather than pyke's prebuilt download), copies the relevant ORT DLLs from that
//! directory into the cargo target `deps/` directory so the Windows loader resolves them from
//! the bench/test/example binary's application directory (step 1 of the DLL search order) —
//! ahead of any `onnxruntime.dll` shipped by the OS in `System32` (step 2). On this machine the
//! OS ships `C:\Windows\System32\onnxruntime.dll` (version 1.17.x, Windows ML stack) which is
//! too old for `ort 2.0.0-rc.12` and would otherwise hijack the process. PATH alone is not
//! sufficient because PATH is searched after System32.
//!
//! Behavior:
//!
//! * `ORT_LIB_LOCATION` unset → no-op. ort-sys downloads pyke's prebuilt; ort's `copy-dylibs`
//!   handles that path.
//! * `ORT_LIB_LOCATION` set, `gpu-cuda` off → copies `onnxruntime.dll` and
//!   `onnxruntime_providers_shared.dll`. Enough for the CPU bench to share an ORT build with
//!   the GPU bench so the comparison is on identical engine versions.
//! * `ORT_LIB_LOCATION` set, `gpu-cuda` on → additionally copies
//!   `onnxruntime_providers_cuda.dll`.
//!
//! Default `cargo build` is unaffected: `ORT_LIB_LOCATION` is not set in normal production
//! builds and the function returns early.

fn main() {
    println!("cargo:rerun-if-env-changed=ORT_LIB_LOCATION");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_GPU_CUDA");

    let Some(lib_dir) = std::env::var_os("ORT_LIB_LOCATION") else {
        return;
    };

    let src_dir = std::path::PathBuf::from(lib_dir);
    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    // OUT_DIR is target/<profile>/build/<crate>-<hash>/out — walk up 3 to reach target/<profile>,
    // then into deps/.
    let Some(target_deps) = std::path::PathBuf::from(&out_dir)
        .ancestors()
        .nth(3)
        .map(|p| p.join("deps"))
    else {
        return;
    };
    if !target_deps.exists() {
        // The deps dir is created by cargo before linking bench/test binaries. If it doesn't
        // exist yet, create it so the copy still succeeds for fresh builds.
        let _ = std::fs::create_dir_all(&target_deps);
    }

    // Files needed by every ORT-linked bench/test binary.
    let mut files: Vec<&str> = vec!["onnxruntime.dll", "onnxruntime_providers_shared.dll"];
    // Only relevant when CUDA is wired in. `onnxruntime_providers_cuda.dll` is ~80 MB so we
    // skip it on CPU builds to keep the deps dir from bloating unnecessarily.
    if std::env::var_os("CARGO_FEATURE_GPU_CUDA").is_some() {
        files.push("onnxruntime_providers_cuda.dll");
    }

    for name in files {
        let src = src_dir.join(name);
        if !src.exists() {
            continue;
        }
        let dst = target_deps.join(name);
        // Best-effort copy; emit a cargo warning on failure but do not fail the build.
        if let Err(err) = std::fs::copy(&src, &dst) {
            println!(
                "cargo:warning=failed to copy {} to {}: {err}",
                src.display(),
                dst.display()
            );
        }
    }
}
