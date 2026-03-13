use std::process::Command;
use tracing::{debug, info};

const BRIDGE_NAME: &str = "daytona0";
const SUBNET: &str = "10.200.0";
const BRIDGE_IP: &str = "10.200.0.1/24";
const BRIDGE_GATEWAY: &str = "10.200.0.1";

fn should_ignore_netns_error(stderr: &str) -> bool {
    let stderr = stderr.trim();
    stderr.contains("File exists") || stderr.contains("RTNETLINK answers: File exists")
}

fn run_cmd(program: &str, args: &[&str]) -> anyhow::Result<()> {
    debug!("netns: {} {}", program, args.join(" "));
    let output = Command::new(program).args(args).output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        debug!("netns cmd stderr: {}", stderr);
        anyhow::bail!("{} {} failed: {}", program, args.join(" "), stderr.trim());
    }
    Ok(())
}

fn run_cmd_allow_exists(program: &str, args: &[&str]) -> anyhow::Result<()> {
    debug!("netns (allow exists): {} {}", program, args.join(" "));
    let output = Command::new(program).args(args).output()?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    debug!("netns cmd stderr: {}", stderr);
    if should_ignore_netns_error(&stderr) {
        return Ok(());
    }

    anyhow::bail!("{} {} failed: {}", program, args.join(" "), stderr.trim());
}

fn run_cmd_ignore(program: &str, args: &[&str]) {
    debug!("netns (ignore errors): {} {}", program, args.join(" "));
    let _ = Command::new(program).args(args).output();
}

/// Ensure the host-side bridge `daytona0` exists with IP forwarding and NAT.
/// This is idempotent and safe to call multiple times.
pub fn ensure_bridge() -> anyhow::Result<()> {
    info!("Ensuring network bridge {} is configured", BRIDGE_NAME);

    // 1. Create bridge. Only ignore the "already exists" case.
    run_cmd_allow_exists("ip", &["link", "add", BRIDGE_NAME, "type", "bridge"]).map_err(
        |err| {
            anyhow::anyhow!(
                "failed to create bridge {}: {}. If running inside Docker or a cloud container, start the container with --privileged or at least CAP_NET_ADMIN, CAP_SYS_ADMIN, writable /proc/sys, and access to iptables",
                BRIDGE_NAME,
                err
            )
        },
    )?;

    // 2. Assign IP address. Only ignore the "already exists" case.
    run_cmd_allow_exists("ip", &["addr", "add", BRIDGE_IP, "dev", BRIDGE_NAME])?;

    // 3. Bring bridge up
    run_cmd("ip", &["link", "set", BRIDGE_NAME, "up"])?;

    // 4. Enable IP forwarding
    std::fs::write("/proc/sys/net/ipv4/ip_forward", "1")?;
    info!("IP forwarding enabled");

    // 5. Add iptables MASQUERADE rule (ignore if already exists)
    let masq_check = Command::new("iptables")
        .args([
            "-t",
            "nat",
            "-C",
            "POSTROUTING",
            "-s",
            &format!("{}.0/24", SUBNET),
            "-j",
            "MASQUERADE",
        ])
        .output()?;

    if !masq_check.status.success() {
        run_cmd(
            "iptables",
            &[
                "-t",
                "nat",
                "-A",
                "POSTROUTING",
                "-s",
                &format!("{}.0/24", SUBNET),
                "-j",
                "MASQUERADE",
            ],
        )?;
        info!("iptables MASQUERADE rule added for {}.0/24", SUBNET);
    }

    // 6. Allow forwarding for the bridge subnet
    let fwd_check = Command::new("iptables")
        .args([
            "-C",
            "FORWARD",
            "-s",
            &format!("{}.0/24", SUBNET),
            "-j",
            "ACCEPT",
        ])
        .output()?;

    if !fwd_check.status.success() {
        run_cmd(
            "iptables",
            &[
                "-A",
                "FORWARD",
                "-s",
                &format!("{}.0/24", SUBNET),
                "-j",
                "ACCEPT",
            ],
        )?;
        run_cmd(
            "iptables",
            &[
                "-A",
                "FORWARD",
                "-d",
                &format!("{}.0/24", SUBNET),
                "-j",
                "ACCEPT",
            ],
        )?;
        info!("iptables FORWARD rules added");
    }

    info!("Bridge {} ready at {}", BRIDGE_NAME, BRIDGE_IP);
    Ok(())
}

/// Set up networking for a sandbox after clone().
pub fn setup_sandbox_net(
    child_pid: i32,
    sandbox_index: u8,
    merged_dir: Option<&str>,
) -> anyhow::Result<()> {
    let idx = sandbox_index.to_string();
    let veth_host = format!("veth-h-{}", idx);
    let veth_sandbox = format!("veth-s-{}", idx);
    let sandbox_ip = format!("{}.{}/24", SUBNET, idx);
    let pid_str = child_pid.to_string();

    info!(
        "Setting up network for sandbox (pid={}, ip={}.{})",
        child_pid, SUBNET, sandbox_index
    );

    // 1. Create veth pair
    run_cmd(
        "ip",
        &[
            "link",
            "add",
            &veth_host,
            "type",
            "veth",
            "peer",
            "name",
            &veth_sandbox,
        ],
    )?;

    // 2. Attach host-side to bridge and bring up
    run_cmd("ip", &["link", "set", &veth_host, "master", BRIDGE_NAME])?;
    run_cmd("ip", &["link", "set", &veth_host, "up"])?;

    // 3. Move sandbox-side into child's network namespace
    run_cmd("ip", &["link", "set", &veth_sandbox, "netns", &pid_str])?;

    // 4. Configure networking inside the child namespace via nsenter
    run_cmd(
        "nsenter",
        &["-t", &pid_str, "-n", "ip", "link", "set", "lo", "up"],
    )?;

    run_cmd(
        "nsenter",
        &[
            "-t",
            &pid_str,
            "-n",
            "ip",
            "link",
            "set",
            &veth_sandbox,
            "name",
            "eth0",
        ],
    )?;

    run_cmd(
        "nsenter",
        &[
            "-t",
            &pid_str,
            "-n",
            "ip",
            "addr",
            "add",
            &sandbox_ip,
            "dev",
            "eth0",
        ],
    )?;

    run_cmd(
        "nsenter",
        &["-t", &pid_str, "-n", "ip", "link", "set", "eth0", "up"],
    )?;

    run_cmd(
        "nsenter",
        &[
            "-t",
            &pid_str,
            "-n",
            "ip",
            "route",
            "add",
            "default",
            "via",
            BRIDGE_GATEWAY,
        ],
    )?;

    // 5. Copy resolv.conf into sandbox rootfs for DNS resolution
    if let Some(rootfs) = merged_dir {
        let etc_dir = std::path::Path::new(rootfs).join("etc");
        let _ = std::fs::create_dir_all(&etc_dir);
        let _ = std::fs::copy("/etc/resolv.conf", etc_dir.join("resolv.conf"));
        debug!("Copied resolv.conf into sandbox rootfs");
    }

    info!("Network configured for sandbox pid={}", child_pid);
    Ok(())
}

/// Tear down networking for a sandbox.
pub fn teardown_sandbox_net(sandbox_index: u8) {
    let veth_host = format!("veth-h-{}", sandbox_index);
    info!("Tearing down network for sandbox index={}", sandbox_index);
    run_cmd_ignore("ip", &["link", "del", &veth_host]);
    crate::netns::release_index(sandbox_index);
}
