use std::process::Command;
use tracing::info;

use crate::sandbox::{ResourceLimits, SandboxProfile};

pub fn run_sandbox(
    sandbox_id: &str,
    merged_dir: &str,
    cmd: &[&str],
    _limits: Option<&ResourceLimits>,
    workdir: Option<&str>,
    _profile: SandboxProfile,
) -> anyhow::Result<()> {
    info!(
        "[macOS] Starting sandbox {} (no isolation — development mode)",
        sandbox_id
    );
    info!("[macOS] merged_dir = {}", merged_dir);

    if cmd.is_empty() {
        info!("[macOS] No command specified, running /bin/sh (sandbox-exec)");
        let profile_content = format!(
            "(version 1)\n\
            (allow default)\n\
            (deny file-write*)\n\
            (allow file-write* (subpath \"{}\"))\n\
            (allow file-write* (subpath \"/dev\"))\n\
            (allow file-write* (regex #\"^/private/var/folders/.*\"))\n",
            merged_dir
        );
        let profile_path = format!("{}/../profile-{}-sh.sb", merged_dir, sandbox_id);
        std::fs::write(&profile_path, profile_content)?;

        let status = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("/bin/sh")
            .current_dir(merged_dir)
            .status()?;
        if !status.success() {
            anyhow::bail!("Sandbox exited with code {:?}", status.code());
        }
        return Ok(());
    }

    // For "tail -f /dev/null" style keep-alive, spawn a long-running child
    // and save its PID so exec can target it later.
    let is_keepalive =
        cmd.len() >= 3 && cmd[0] == "tail" && cmd[1] == "-f" && cmd[2] == "/dev/null";

    if is_keepalive {
        // On macOS we use a simple sleep loop. The sandbox merged_dir is just
        // a regular directory, so exec will run commands directly inside it.
        info!("[macOS] Keep-alive mode: spawning background sleep process");

        let child = Command::new("sleep")
            .arg("999999")
            .current_dir(merged_dir)
            .spawn()?;

        let child_pid = child.id() as i32;
        info!("[macOS] Background process PID: {}", child_pid);

        // Save PID to metadata so exec can find the sandbox
        if let Ok(mut metadata) = crate::metadata::load_metadata() {
            if let Some(sandbox) = metadata.sandboxes.get_mut(sandbox_id) {
                sandbox.pid = Some(child_pid);
                sandbox.ip = Some("127.0.0.1".to_string());
                let _ = crate::metadata::save_metadata(&metadata);
            }
        }

        // Don't wait — return immediately so the API can respond
        // The child will be killed when the sandbox is destroyed
        std::mem::forget(child);
        return Ok(());
    }

    // Normal command execution: run the command with merged_dir as root-like env
    let effective_cmd = if cmd[0].starts_with('/') {
        // Absolute path: try to run it from merged_dir
        let merged_path = format!("{}{}", merged_dir, cmd[0]);
        if std::path::Path::new(&merged_path).exists() {
            merged_path
        } else {
            // Fall back to host command
            cmd[0].to_string()
        }
    } else {
        cmd[0].to_string()
    };

    let actual_workdir = if let Some(wd) = workdir {
        if wd.starts_with('/') {
            format!("{}{}", merged_dir, wd)
        } else {
            format!("{}/{}", merged_dir, wd)
        }
    } else {
        merged_dir.to_string()
    };

    // Ensure workdir exists
    let _ = std::fs::create_dir_all(&actual_workdir);

    info!(
        "[macOS] Running (sandbox-exec): {} {:?} in {}",
        effective_cmd,
        &cmd[1..],
        actual_workdir
    );

    let profile_content = format!(
        "(version 1)\n\
        (allow default)\n\
        (deny file-write*)\n\
        (allow file-write* (subpath \"{}\"))\n\
        (allow file-write* (subpath \"/dev\"))\n\
        (allow file-write* (regex #\"^/private/var/folders/.*\"))\n",
        merged_dir
    );
    let profile_path = format!("{}/../profile-{}.sb", merged_dir, sandbox_id);
    std::fs::write(&profile_path, profile_content)?;

    // Discover Python site-packages inside the image so host python
    // can import packages that were installed during `docker build`.
    let python_path = std::fs::read_dir(format!("{}/usr/local/lib", merged_dir))
        .ok()
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    n.starts_with("python") && e.path().join("site-packages").is_dir()
                })
                .map(|e| format!("{}/site-packages", e.path().to_string_lossy()))
                .collect::<Vec<_>>()
                .join(":")
        })
        .unwrap_or_default();

    let host_path = std::env::var("PATH")
        .unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".to_string());
    let mut run = Command::new("sandbox-exec");
    run.arg("-f")
        .arg(&profile_path)
        .arg(&effective_cmd)
        .args(&cmd[1..])
        .current_dir(&actual_workdir)
        .env(
            "PATH",
            format!(
                "{}:{}/usr/local/sbin:{}/usr/local/bin:{}/usr/sbin:{}/usr/bin:{}/sbin:{}/bin",
                host_path, merged_dir, merged_dir, merged_dir, merged_dir, merged_dir, merged_dir
            ),
        )
        .env("HOME", format!("{}/root", merged_dir))
        .env("TMPDIR", format!("{}/tmp", merged_dir))
        .env(
            "NODE_PATH",
            format!("{}/usr/local/lib/node_modules", merged_dir),
        );

    if !python_path.is_empty() {
        run.env("PYTHONPATH", &python_path);
    }

    let status = run.status()?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        if code != 0 {
            anyhow::bail!("Sandbox exited with code {}", code);
        }
    }

    Ok(())
}
