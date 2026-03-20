use crate::metadata::SandboxMetadata;
use crate::overlay::OverlayMount;
use crate::server::{SandboxRuntimeStatsResponse, ServerInfoResponse};
use std::path::Path;
use std::process::{Command, Output};

pub fn get_server_info() -> ServerInfoResponse {
    ServerInfoResponse {
        os: "linux",
        degraded_mode: false,
        supports_image_exec: true,
    }
}

pub fn get_sandbox_metrics(sandbox: &SandboxMetadata) -> SandboxRuntimeStatsResponse {
    let mut stats = SandboxRuntimeStatsResponse::default();
    let cgroup_path = std::path::PathBuf::from("/sys/fs/cgroup/moulin").join(&sandbox.id);

    stats.memory_current_bytes = read_u64_file(&cgroup_path.join("memory.current"));
    stats.memory_peak_bytes = read_u64_file(&cgroup_path.join("memory.peak"));
    stats.memory_limit_bytes = read_limit_file(&cgroup_path.join("memory.max"));
    stats.pids_current = read_u64_file(&cgroup_path.join("pids.current"));
    stats.pids_limit = read_limit_file(&cgroup_path.join("pids.max"));

    if let Some(cpu_max) = read_trimmed(&cgroup_path.join("cpu.max")) {
        let mut parts = cpu_max.split_whitespace();
        let quota = parts.next();
        let period = parts.next();
        stats.cpu_quota = quota.and_then(parse_limit_value);
        stats.cpu_period = period.and_then(|value| value.parse::<u64>().ok());
    }

    if let Ok(cpu_stat) = std::fs::read_to_string(cgroup_path.join("cpu.stat")) {
        for line in cpu_stat.lines() {
            if let Some(value) = line.strip_prefix("usage_usec ") {
                stats.cpu_usage_usec = value.trim().parse::<u64>().ok();
                break;
            }
        }
    }

    if let Some(pid) = sandbox.pid {
        stats.process_resident_bytes = read_proc_rss_bytes(pid);
    }

    stats.oom_kill_count = read_oom_kill_count_from_events(&cgroup_path);

    stats
}

pub fn read_oom_kill_count(sandbox_id: &str) -> Option<u64> {
    let cgroup_path = std::path::PathBuf::from("/sys/fs/cgroup/moulin").join(sandbox_id);
    read_oom_kill_count_from_events(&cgroup_path)
}

fn read_oom_kill_count_from_events(cgroup_path: &Path) -> Option<u64> {
    let events = std::fs::read_to_string(cgroup_path.join("memory.events")).ok()?;
    for line in events.lines() {
        if let Some(value) = line.strip_prefix("oom_kill ") {
            return value.trim().parse::<u64>().ok();
        }
    }
    None
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
}

fn read_u64_file(path: &Path) -> Option<u64> {
    read_trimmed(path).and_then(|value| value.parse::<u64>().ok())
}

fn parse_limit_value(raw: &str) -> Option<u64> {
    if raw == "max" {
        None
    } else {
        raw.parse::<u64>().ok()
    }
}

fn read_limit_file(path: &Path) -> Option<u64> {
    read_trimmed(path).and_then(|value| parse_limit_value(&value))
}

fn read_proc_rss_bytes(pid: i32) -> Option<u64> {
    let status_path = format!("/proc/{}/status", pid);
    let status = std::fs::read_to_string(status_path).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb = rest.split_whitespace().next()?.parse::<u64>().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

pub fn destroy_sandbox_os(sandbox: &SandboxMetadata, merged_dir: &Path) {
    let overlay = OverlayMount::new(
        vec![],
        sandbox.dir.join("upper"),
        sandbox.dir.join("work"),
        merged_dir.to_path_buf(),
    )
    .ok();
    if let Some(mnt) = overlay {
        let _ = mnt.unmount();
    }
}

pub fn exec_sandbox(
    sandbox: &SandboxMetadata,
    cmd: &[String],
    env_vars: &[String],
) -> anyhow::Result<Output> {
    let merged_dir = sandbox.dir.join("merged");

    if let Some(pid) = sandbox.pid {
        let mut command = Command::new("nsenter");
        command
            .arg("-a") // enter all namespaces (mount, uts, ipc, net, pid, user, cgroup)
            .arg("-t")
            .arg(pid.to_string())
            .env_clear()
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .env("HOME", "/root")
            .env("TMPDIR", "/tmp")
            .env("TERM", "xterm");

        for entry in env_vars {
            if let Some((key, value)) = entry.split_once('=') {
                command.env(key, value);
            }
        }

        command
            .args(cmd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn()?;
        // Place the nsenter process (and its future children) into the sandbox
        // cgroup so that CPU / memory / pids accounting is captured correctly.
        let cgroup_procs = format!("/sys/fs/cgroup/moulin/{}/cgroup.procs", sandbox.id);
        let _ = std::fs::write(&cgroup_procs, child.id().to_string());
        let output = child.wait_with_output()?;
        Ok(output)
    } else {
        let output = Command::new("chroot").arg(&merged_dir).args(cmd).output()?;
        Ok(output)
    }
}

pub async fn exec_sandbox_stream(
    sandbox: &SandboxMetadata,
    cmd: &[String],
    env_vars: &[String],
) -> anyhow::Result<tokio::process::Child> {
    let merged_dir = sandbox.dir.join("merged");

    if let Some(pid) = sandbox.pid {
        let mut command = tokio::process::Command::new("nsenter");
        command
            .arg("-a") // enter all namespaces (mount, uts, ipc, net, pid, user, cgroup)
            .arg("-t")
            .arg(pid.to_string())
            .env_clear()
            .env(
                "PATH",
                "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
            )
            .env("HOME", "/root")
            .env("TMPDIR", "/tmp")
            .env("TERM", "xterm")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        for entry in env_vars {
            if let Some((key, value)) = entry.split_once('=') {
                command.env(key, value);
            }
        }

        command.args(cmd);
        let child = command.spawn()?;
        // Place the nsenter process into the sandbox cgroup for accurate
        // CPU / memory / pids accounting.
        if let Some(child_pid) = child.id() {
            let cgroup_procs = format!("/sys/fs/cgroup/moulin/{}/cgroup.procs", sandbox.id);
            let _ = std::fs::write(&cgroup_procs, child_pid.to_string());
        }
        Ok(child)
    } else {
        // Fallback (e.g. metadata before the fix)
        let mut command = tokio::process::Command::new("chroot");
        command
            .arg(&merged_dir)
            .args(cmd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        Ok(command.spawn()?)
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
