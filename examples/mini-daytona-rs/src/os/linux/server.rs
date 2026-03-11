use std::path::Path;
use std::process::{Command, Output};
use crate::metadata::SandboxMetadata;
use crate::server::ServerInfoResponse;
use crate::overlay::OverlayMount;

pub fn get_server_info() -> ServerInfoResponse {
    ServerInfoResponse {
        os: "linux",
        degraded_mode: false,
        supports_image_exec: true,
    }
}

pub fn destroy_sandbox_os(sandbox: &SandboxMetadata, merged_dir: &Path) {
    let overlay = OverlayMount::new(
        vec![],
        sandbox.dir.join("upper"),
        sandbox.dir.join("work"),
        merged_dir.to_path_buf(),
    ).ok();
    if let Some(mnt) = overlay {
        let _ = mnt.unmount();
    }
}

pub fn exec_sandbox(sandbox: &SandboxMetadata, cmd: &[String], env_vars: &[String]) -> anyhow::Result<Output> {
    let merged_dir = sandbox.dir.join("merged");

    if let Some(pid) = sandbox.pid {
        let mut command = Command::new("nsenter");
        command
            .arg("-a") // enter all namespaces (mount, uts, ipc, net, pid, user, cgroup)
            .arg("-t")
            .arg(pid.to_string())
            .env_clear()
            .env("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
            .env("HOME", "/root")
            .env("TMPDIR", "/tmp")
            .env("TERM", "xterm");

        for entry in env_vars {
            if let Some((key, value)) = entry.split_once('=') {
                command.env(key, value);
            }
        }

        let output = command.args(cmd).output()?;
        Ok(output)
    } else {
        // Fallback (e.g. metadata before the fix)
        let output = Command::new("chroot")
            .arg(&merged_dir)
            .args(cmd)
            .output()?;
        Ok(output)
    }
}

pub fn suspend_sandbox_os(sandbox: &SandboxMetadata, _merged_dir: &Path) -> anyhow::Result<()> {
    if let Some(pid) = sandbox.pid {
        unsafe {
            let _ = libc::kill(pid, libc::SIGSTOP);
        }
    }
    Ok(())
}

pub fn resume_sandbox_os(sandbox: &SandboxMetadata, _merged_dir: &Path) -> anyhow::Result<()> {
    if let Some(pid) = sandbox.pid {
        unsafe {
            let _ = libc::kill(pid, libc::SIGCONT);
        }
    }
    Ok(())
}
