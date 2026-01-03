//! Build/test script runner implementation with auto-detection.

use crate::RpcResponse;
use crate::config::{DEFAULT_SCRIPT_TIMEOUT_MS, MAX_SCRIPT_STDERR_BYTES, MAX_SCRIPT_STDOUT_BYTES};
use crate::process_utils;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use tracing::{error, info, warn};

/// Supported build systems for auto-detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildSystem {
    Script, // build.ps1 / build.sh
    Cargo,
    Pnpm,
    Yarn,
    Npm,
    Make,
    Just,
    Go,
    Cmake,
}

impl BuildSystem {
    /// Parse from string parameter.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "script" => Some(Self::Script),
            "cargo" | "rust" => Some(Self::Cargo),
            "pnpm" => Some(Self::Pnpm),
            "yarn" => Some(Self::Yarn),
            "npm" => Some(Self::Npm),
            "make" => Some(Self::Make),
            "just" => Some(Self::Just),
            "go" | "golang" => Some(Self::Go),
            "cmake" => Some(Self::Cmake),
            _ => None,
        }
    }

    /// Get display name for logging.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Script => "script",
            Self::Cargo => "cargo",
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
            Self::Npm => "npm",
            Self::Make => "make",
            Self::Just => "just",
            Self::Go => "go",
            Self::Cmake => "cmake",
        }
    }
}

/// Information about a detected Cargo workspace member.
#[derive(Debug)]
struct CargoWorkspaceInfo {
    /// The workspace root directory.
    workspace_root: std::path::PathBuf,
    /// The package name if we're in a subcrate.
    package_name: Option<String>,
}

/// Detect if we're in a Cargo workspace and get relevant info.
fn detect_cargo_workspace(work_dir: &Path) -> Option<CargoWorkspaceInfo> {
    let cargo_toml = work_dir.join("Cargo.toml");
    if !cargo_toml.exists() {
        return None;
    }

    // Read the Cargo.toml to check if it's a workspace root or member
    let content = std::fs::read_to_string(&cargo_toml).ok()?;

    // Check if this is a workspace root
    if content.contains("[workspace]") {
        return Some(CargoWorkspaceInfo {
            workspace_root: work_dir.to_path_buf(),
            package_name: None,
        });
    }

    // Check if there's a package name (we're in a subcrate)
    let package_name = content
        .lines()
        .find(|line| line.trim().starts_with("name"))
        .and_then(|line| {
            line.split('=')
                .nth(1)
                .map(|s| s.trim().trim_matches('"').to_string())
        });

    // Look for parent workspace
    let mut parent = work_dir.parent();
    while let Some(p) = parent {
        let parent_cargo = p.join("Cargo.toml");
        if parent_cargo.exists() {
            if let Ok(parent_content) = std::fs::read_to_string(&parent_cargo) {
                if parent_content.contains("[workspace]") {
                    return Some(CargoWorkspaceInfo {
                        workspace_root: p.to_path_buf(),
                        package_name,
                    });
                }
            }
        }
        parent = p.parent();
    }

    // Standalone Cargo project
    Some(CargoWorkspaceInfo {
        workspace_root: work_dir.to_path_buf(),
        package_name: None,
    })
}

/// Check if package.json has a build script.
fn package_json_has_build_script(work_dir: &Path) -> bool {
    let pkg_json = work_dir.join("package.json");
    if let Ok(content) = std::fs::read_to_string(pkg_json) {
        // Simple check - look for "build" in scripts
        content.contains("\"build\"")
    } else {
        false
    }
}

/// Detect the build system to use for a directory.
/// Returns (BuildSystem, effective_work_dir, extra_args).
pub fn detect_build_system(
    work_dir: &Path,
    action: &str, // "build" or "test"
) -> Option<(BuildSystem, std::path::PathBuf, Vec<String>)> {
    let is_windows = cfg!(target_os = "windows");

    // 1. Check for explicit script override
    let script_name = if is_windows {
        format!("{action}.ps1")
    } else {
        format!("{action}.sh")
    };
    if work_dir.join(&script_name).exists() {
        return Some((BuildSystem::Script, work_dir.to_path_buf(), vec![]));
    }

    // 2. Check for Cargo.toml (Rust)
    if work_dir.join("Cargo.toml").exists() {
        if let Some(ws_info) = detect_cargo_workspace(work_dir) {
            let mut args = vec![action.to_string()];
            if let Some(pkg) = ws_info.package_name {
                args.push("-p".to_string());
                args.push(pkg);
            }
            return Some((BuildSystem::Cargo, ws_info.workspace_root, args));
        }
    }

    // 3. Check for package.json (JS/TS)
    if work_dir.join("package.json").exists() && package_json_has_build_script(work_dir) {
        // Detect package manager
        if work_dir.join("pnpm-lock.yaml").exists() {
            return Some((
                BuildSystem::Pnpm,
                work_dir.to_path_buf(),
                vec!["run".to_string(), action.to_string()],
            ));
        }
        if work_dir.join("yarn.lock").exists() {
            return Some((
                BuildSystem::Yarn,
                work_dir.to_path_buf(),
                vec![action.to_string()],
            ));
        }
        return Some((
            BuildSystem::Npm,
            work_dir.to_path_buf(),
            vec!["run".to_string(), action.to_string()],
        ));
    }

    // 4. Check for Makefile
    if work_dir.join("Makefile").exists() || work_dir.join("makefile").exists() {
        return Some((BuildSystem::Make, work_dir.to_path_buf(), vec![]));
    }

    // 5. Check for justfile
    if work_dir.join("justfile").exists() || work_dir.join("Justfile").exists() {
        return Some((
            BuildSystem::Just,
            work_dir.to_path_buf(),
            vec![action.to_string()],
        ));
    }

    // 6. Check for go.mod (Go)
    if work_dir.join("go.mod").exists() {
        let args = if action == "build" {
            vec!["build".to_string(), "./...".to_string()]
        } else {
            vec!["test".to_string(), "./...".to_string()]
        };
        return Some((BuildSystem::Go, work_dir.to_path_buf(), args));
    }

    // 7. Check for CMakeLists.txt
    if work_dir.join("CMakeLists.txt").exists() {
        // CMake typically uses a build directory
        let build_dir = work_dir.join("build");
        if build_dir.exists() {
            return Some((
                BuildSystem::Cmake,
                work_dir.to_path_buf(),
                vec!["--build".to_string(), "build".to_string()],
            ));
        }
        // No build dir yet - would need cmake configure first
        return Some((
            BuildSystem::Cmake,
            work_dir.to_path_buf(),
            vec!["--build".to_string(), ".".to_string()],
        ));
    }

    None
}

/// Get the command to run for a build system.
fn get_build_command(system: BuildSystem) -> &'static str {
    match system {
        BuildSystem::Script => unreachable!("Script handled separately"),
        BuildSystem::Cargo => "cargo",
        BuildSystem::Pnpm => "pnpm",
        BuildSystem::Yarn => "yarn",
        BuildSystem::Npm => "npm",
        BuildSystem::Make => "make",
        BuildSystem::Just => "just",
        BuildSystem::Go => "go",
        BuildSystem::Cmake => "cmake",
    }
}

/// Configuration for a script-based tool.
pub struct ScriptConfig {
    /// Base script name without extension (e.g., "build", "test").
    pub script_base: &'static str,
    /// Label for logging/error messages (e.g., "Build", "Test").
    pub tool_label: &'static str,
}

#[derive(Deserialize)]
struct ScriptRequest {
    #[serde(default)]
    working_dir: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    build_system: Option<String>,
}

/// Generic script runner for build/test style tools.
/// Auto-detects build system or uses explicit override.
pub async fn run_script_tool(
    id: Option<Value>,
    args: Value,
    config: ScriptConfig,
) -> RpcResponse<'static> {
    let req = match RpcResponse::parse::<ScriptRequest>(id.clone(), args) {
        Ok(r) => r,
        Err(resp) => return resp,
    };

    let work_dir = req.working_dir.as_deref().unwrap_or(".");
    let work_path = Path::new(work_dir);
    let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_SCRIPT_TIMEOUT_MS);

    // Check for explicit build_system override
    let forced_system = req
        .build_system
        .as_ref()
        .and_then(|s| BuildSystem::from_str(s));

    if let Some(bs) = &req.build_system {
        if forced_system.is_none() {
            return RpcResponse::err(
                id,
                format!(
                    "Unknown build_system '{}'. Valid options: cargo, npm, pnpm, yarn, make, just, go, cmake, script",
                    bs
                ),
            );
        }
    }

    // Detect or use forced build system
    let (system, effective_dir, args) = if let Some(system) = forced_system {
        // User explicitly requested a build system
        let args = match system {
            BuildSystem::Script => vec![],
            BuildSystem::Cargo => vec![config.script_base.to_string()],
            BuildSystem::Pnpm => vec!["run".to_string(), config.script_base.to_string()],
            BuildSystem::Yarn => vec![config.script_base.to_string()],
            BuildSystem::Npm => vec!["run".to_string(), config.script_base.to_string()],
            BuildSystem::Make => vec![],
            BuildSystem::Just => vec![config.script_base.to_string()],
            BuildSystem::Go => {
                if config.script_base == "build" {
                    vec!["build".to_string(), "./...".to_string()]
                } else {
                    vec!["test".to_string(), "./...".to_string()]
                }
            }
            BuildSystem::Cmake => vec!["--build".to_string(), ".".to_string()],
        };
        (system, work_path.to_path_buf(), args)
    } else {
        // Auto-detect
        match detect_build_system(work_path, config.script_base) {
            Some(detected) => detected,
            None => {
                let is_windows = cfg!(target_os = "windows");
                let script_ext = if is_windows { ".ps1" } else { ".sh" };
                return RpcResponse::err(
                    id,
                    format!(
                        "No build system detected in {}. Looked for: {}{}, Cargo.toml, package.json, Makefile, justfile, go.mod, CMakeLists.txt. Remediation: pass working_dir to the project root or set build_system explicitly.",
                        Path::new(work_dir)
                            .canonicalize()
                            .unwrap_or_else(|_| work_dir.into())
                            .display(),
                        config.script_base,
                        script_ext,
                    ),
                );
            }
        }
    };

    // Handle script specially
    if system == BuildSystem::Script {
        let is_windows = cfg!(target_os = "windows");
        let script_name = if is_windows {
            format!("{}.ps1", config.script_base)
        } else {
            format!("{}.sh", config.script_base)
        };
        let script_path = effective_dir.join(&script_name);

        info!(
            "{} tool: running {} in {}",
            config.tool_label,
            script_name,
            effective_dir.display()
        );

        let result = match process_utils::run_shell_script(
            &script_path,
            effective_dir.to_str().unwrap_or("."),
            timeout_ms,
            MAX_SCRIPT_STDOUT_BYTES,
            MAX_SCRIPT_STDERR_BYTES,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                error!("{} tool: {}", config.tool_label, e);
                let mut msg = format!("failed to run {}: {}", script_name, e);
                let lower = e.to_ascii_lowercase();
                if lower.contains("failed to spawn") {
                    msg.push_str(
                        ". Remediation: ensure the script runner is installed and on PATH (PowerShell on Windows, bash/sh on Unix), and that the script file exists.",
                    );
                }
                return RpcResponse::err(id, msg);
            }
        };

        if !result.success {
            error!(
                "{} tool: {} failed (exit_code={:?})",
                config.tool_label, script_name, result.exit_code
            );
        }

        let mut extra = HashMap::new();
        extra.insert("build_system", json!("script"));
        extra.insert("script", json!(script_name));
        extra.insert("working_dir", json!(effective_dir.to_string_lossy()));
        let payload = process_utils::build_process_result_response(&result, Some(extra));
        return RpcResponse::ok_json_content(id, payload, !result.success);
    }

    // Run the detected build command
    let cmd = get_build_command(system);
    info!(
        "{} tool: running {} {:?} in {}",
        config.tool_label,
        cmd,
        args,
        effective_dir.display()
    );

    let result = match process_utils::run_command(
        cmd,
        &args,
        effective_dir.to_str().unwrap_or("."),
        timeout_ms,
        MAX_SCRIPT_STDOUT_BYTES,
        MAX_SCRIPT_STDERR_BYTES,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            error!("{} tool: {}", config.tool_label, e);
            let mut msg = format!("failed to run {} {:?}: {}", cmd, args, e);
            let lower = e.to_ascii_lowercase();
            if lower.contains("failed to spawn") {
                msg.push_str(
                    ". Remediation: ensure the command is installed and on PATH for the host running the MCP server.",
                );
            }
            return RpcResponse::err(id, msg);
        }
    };

    if !result.success {
        warn!(
            "{} tool: {} failed (exit_code={:?})",
            config.tool_label, cmd, result.exit_code
        );
    }

    let mut extra = HashMap::new();
    extra.insert("build_system", json!(system.name()));
    extra.insert("command", json!(cmd));
    extra.insert("args", json!(args));
    extra.insert("working_dir", json!(effective_dir.to_string_lossy()));
    let payload = process_utils::build_process_result_response(&result, Some(extra));
    RpcResponse::ok_json_content(id, payload, !result.success)
}
