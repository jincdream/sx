use std::path::Path;
use std::process::{Command, Output};
use tracing::info;
use crate::metadata::SandboxMetadata;
use crate::server::ServerInfoResponse;

pub fn get_server_info() -> ServerInfoResponse {
    ServerInfoResponse {
        os: "macos",
        degraded_mode: true,
        supports_image_exec: true,
    }
}

pub fn destroy_sandbox_os(_sandbox: &SandboxMetadata, _merged_dir: &Path) {
    // On macOS there's no real mount; skip the expensive merged→upper copy
    // since we're about to delete everything anyway.
}

pub fn exec_sandbox(sandbox: &SandboxMetadata, cmd: &[String], env_vars: &[String]) -> anyhow::Result<Output> {
    let merged_dir = sandbox.dir.join("merged");

    // On macOS there are no namespaces. Run supported host tools
    // while rewriting absolute sandbox paths into merged_dir paths.
    let effective_cmd = resolve_macos_command(&merged_dir, &cmd[0]);
    let translated_args: Vec<String> = cmd[1..]
        .iter()
        .map(|arg| translate_macos_path_arg(&merged_dir, arg))
        .collect();

    let merged_dir_str = merged_dir.to_string_lossy().to_string();

    // Discover Python site-packages inside the image so host python
    // can import packages that were installed during `docker build`.
    let python_path = std::fs::read_dir(format!("{}/usr/local/lib", merged_dir_str))
        .ok()
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let name = e.file_name();
                    let n = name.to_string_lossy();
                    n.starts_with("python") && e.path().join("site-packages").is_dir()
                })
                .map(|e| format!("{}/site-packages", e.path().to_string_lossy()))
                .collect::<Vec<_>>()
                .join(":")
        })
        .unwrap_or_default();

    // For Python commands, inject a sitecustomize.py that transparently
    // redirects absolute file paths (e.g. /home/daytona/...) into the
    // merged_dir. Without a real chroot on macOS, scripts that use
    // hard-coded absolute paths would otherwise fail.
    let is_python = effective_cmd.contains("python");
    let redirect_dir = if is_python {
        let dir = format!("{}/tmp/_sandbox_pathfix", merged_dir_str);
        let _ = std::fs::create_dir_all(&dir);
        let sc = format!(r#"import os as _os, builtins as _bi
_M = _os.environ.get('_SANDBOX_MERGED_DIR', '')
if _M:
    _orig = _bi.open
    def _ropen(f, *a, **k):
        if isinstance(f, str) and f.startswith('/') and not f.startswith(_M):
            alt = _os.path.join(_M, f.lstrip('/'))
            if _os.path.exists(alt):
                f = alt
        return _orig(f, *a, **k)
    _bi.open = _ropen
    import io as _io
    _iorig = _io.open
    def _rioopen(f, *a, **k):
        if isinstance(f, str) and f.startswith('/') and not f.startswith(_M):
            alt = _os.path.join(_M, f.lstrip('/'))
            if _os.path.exists(alt):
                f = alt
        return _iorig(f, *a, **k)
    _io.open = _rioopen
"#);
        let _ = std::fs::write(format!("{}/sitecustomize.py", dir), sc);
        Some(dir)
    } else {
        None
    };

    let mut command = Command::new(&effective_cmd);
    // Use the process's actual PATH (which includes e.g. conda) so
    // the same python3 that pip used during build is available here.
    let host_path = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_string());
    command.args(&translated_args)
        .current_dir(&merged_dir)
        // Host paths FIRST so native macOS binaries (python3, node, etc.)
        // are resolved before the Linux ELF binaries inside the image.
        .env("PATH", format!("{}:{}/usr/local/sbin:{}/usr/local/bin:{}/usr/sbin:{}/usr/bin:{}/sbin:{}/bin",
            host_path, merged_dir_str, merged_dir_str, merged_dir_str, merged_dir_str, merged_dir_str, merged_dir_str))
        .env("HOME", format!("{}/root", merged_dir_str))
        .env("TMPDIR", format!("{}/tmp", merged_dir_str))
        .env("NODE_PATH", format!("{0}/usr/local/lib/node_modules:{0}/home/daytona/workspace/node_modules", merged_dir_str))
        .env("PUPPETEER_CACHE_DIR", format!("{}/.cache/puppeteer", std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string())))
        .env("TERM", "xterm");

    for entry in env_vars {
        if let Some((key, value)) = entry.split_once('=') {
            command.env(key, value);
        }
    }

    if is_python {
        command.env("_SANDBOX_MERGED_DIR", &merged_dir_str);
    }

    if !python_path.is_empty() {
        let final_python_path = if let Some(ref rd) = redirect_dir {
            format!("{}:{}", rd, python_path)
        } else {
            python_path
        };
        command.env("PYTHONPATH", &final_python_path);
    } else if let Some(ref rd) = redirect_dir {
        command.env("PYTHONPATH", rd);
    }

    Ok(command.output()?)
}

fn translate_macos_path_arg(merged_dir: &Path, value: &str) -> String {
    if !value.starts_with('/') {
        return value.to_string();
    }

    let translated = merged_dir.join(value.trim_start_matches('/'));
    if translated.exists() {
        translated.to_string_lossy().to_string()
    } else {
        value.to_string()
    }
}

/// Scan {merged_dir}/usr/local/lib/python*/site-packages/ for .cpython-NNN-darwin.so
/// files and return the version string like "3.12" or "3.13".
fn detect_cpython_version_from_so(merged_dir: &Path) -> Option<String> {
    fn extract_cpython_ver(fname: &str) -> Option<String> {
        if fname.contains(".cpython-") && fname.ends_with("-darwin.so") {
            let ver_str = fname.split(".cpython-").nth(1)?;
            let digits = ver_str.split('-').next()?;
            if digits.len() >= 3 {
                return Some(format!("{}.{}", &digits[0..1], &digits[1..]));
            }
        }
        None
    }
    let lib_dir = merged_dir.join("usr/local/lib");
    for entry in std::fs::read_dir(&lib_dir).ok()?.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("python") && entry.path().join("site-packages").is_dir() {
            let sp = entry.path().join("site-packages");
            // Search up to 3 levels deep (e.g. numpy/_core/_multiarray_umath.cpython-312-darwin.so)
            for d1 in std::fs::read_dir(&sp).ok()?.filter_map(|e| e.ok()) {
                if !d1.path().is_dir() { continue; }
                for d2 in std::fs::read_dir(d1.path()).ok()?.filter_map(|e| e.ok()) {
                    let fname = d2.file_name().to_string_lossy().to_string();
                    if let Some(v) = extract_cpython_ver(&fname) { return Some(v); }
                    if d2.path().is_dir() {
                        for d3 in std::fs::read_dir(d2.path()).ok()?.filter_map(|e| e.ok()) {
                            let fname = d3.file_name().to_string_lossy().to_string();
                            if let Some(v) = extract_cpython_ver(&fname) { return Some(v); }
                        }
                    }
                }
            }
        }
    }
    None
}

fn resolve_macos_command(merged_dir: &Path, command: &str) -> String {
    let file_name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);

    match file_name {
        "python" | "python3" => {
            // Find the Python version matching installed C-extensions in site-packages.
            // Packages built by host pip compile .so for a specific cpython version
            // (e.g. .cpython-312-darwin.so). We must exec with that same version.
            if let Some(ver) = detect_cpython_version_from_so(merged_dir) {
                let versioned = format!("python{}", ver);
                if let Ok(output) = Command::new("which")
                    .arg(&versioned)
                    .output()
                {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path.is_empty() && Path::new(&path).exists() {
                        info!("[macOS] Resolved {} → {} (matching cpython-{} C-extensions)", file_name, path, ver);
                        return path;
                    }
                }
            }
            "python3".to_string()
        },
        "pip" | "pip3" => "pip3".to_string(),
        "node" | "npm" | "npx" => file_name.to_string(),
        "bash" | "sh" | "env" => file_name.to_string(),
        "ls" | "cat" | "grep" | "head" | "tail" | "wc" | "sort" | "uniq"
        | "find" | "mkdir" | "rm" | "cp" | "mv" | "touch" | "chmod"
        | "echo" | "printf" | "tee" | "sed" | "awk" | "curl" | "wget" => file_name.to_string(),
        _ => translate_macos_path_arg(merged_dir, command),
    }
}
