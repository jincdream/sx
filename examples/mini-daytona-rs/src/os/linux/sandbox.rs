use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::sched::{clone, CloneFlags};
use nix::sys::signal::Signal;
use nix::sys::stat::Mode;
use nix::sys::wait::waitpid;
use nix::unistd::{chdir, execvp, pivot_root, sethostname};
use std::ffi::CString;
use std::fs;
use tracing::{info, warn, error};

use crate::os::sys::netns;
use crate::sandbox::{ResourceLimits, SandboxProfile};

const STACK_SIZE: usize = 1024 * 1024;

pub fn run_sandbox(sandbox_id: &str, merged_dir: &str, cmd: &[&str], limits: Option<&ResourceLimits>, workdir: Option<&str>, profile: SandboxProfile) -> anyhow::Result<()> {
    let mut stack = vec![0u8; STACK_SIZE];

    let flags = CloneFlags::CLONE_NEWNS
        | CloneFlags::CLONE_NEWPID
        | CloneFlags::CLONE_NEWUTS
        | CloneFlags::CLONE_NEWIPC
        | CloneFlags::CLONE_NEWNET
        | CloneFlags::CLONE_NEWUSER;

    let merged_dir_c = CString::new(merged_dir)?;
    let cmd_c: Vec<CString> = cmd.iter().map(|s| CString::new(*s).unwrap()).collect();
    let workdir_c = workdir.map(|w| CString::new(w).unwrap());

    // Create a pipe for parent→child signaling.
    // Child blocks on read_fd until parent finishes user-ns + network setup, then writes a byte.
    let mut pipe_fds = [0i32; 2];
    if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
        anyhow::bail!("Failed to create pipe");
    }
    let read_fd = pipe_fds[0];
    let write_fd = pipe_fds[1];

    let child_pid = unsafe {
        clone(
            Box::new(|| {
                // Close write end in child
                libc::close(write_fd);

                // Block until parent signals readiness (replaces 500ms sleep)
                let mut buf = [0u8; 1];
                libc::read(read_fd, buf.as_mut_ptr() as *mut libc::c_void, 1);
                libc::close(read_fd);

                if let Err(e) = child(merged_dir_c.as_c_str(), &cmd_c, workdir_c.as_deref(), profile) {
                    eprintln!("Child error: {}", e);
                    std::process::exit(1);
                }
                0
            }),
            &mut stack,
            flags,
            Some(Signal::SIGCHLD as i32),
        )?
    };

    // Close read end in parent
    unsafe { libc::close(read_fd) };

    // Parent: setup user namespace ID mappings FIRST (child is blocked waiting)
    let pid_val = child_pid.as_raw();
    if let Err(e) = setup_user_ns(pid_val) {
        warn!("Failed to setup user namespace for pid {}: {}", pid_val, e);
    }

    // Parent: setup cgroups limits
    let effective_limits = limits.cloned().unwrap_or_default();
    if let Err(e) = setup_cgroups(sandbox_id, pid_val, &effective_limits) {
        warn!("Failed to setup cgroups for {}: {}", sandbox_id, e);
    }

    // Save PID to metadata so `exec` can nsenter
    if let Ok(mut metadata) = crate::metadata::load_metadata() {
        if let Some(sandbox) = metadata.sandboxes.get_mut(sandbox_id) {
            sandbox.pid = Some(pid_val);
            let _ = crate::metadata::save_metadata(&metadata);
        }
    }

    // Parent: allocate IP slot and set up veth bridge networking for the child
    let net_index = crate::netns::allocate_index()?;
    if let Err(e) = netns::setup_sandbox_net(pid_val, net_index, Some(merged_dir)) {
        warn!("Failed to setup sandbox network: {}", e);
        // Child will still run but without network
    }

    // Signal child that setup is complete — child unblocks immediately
    unsafe {
        let byte: u8 = 1;
        libc::write(write_fd, &byte as *const u8 as *const libc::c_void, 1);
        libc::close(write_fd);
    };

    // Wait for child to exit
    let status = waitpid(child_pid, None)?;

    // Cleanup network and cgroups
    netns::teardown_sandbox_net(net_index);
    let _ = cleanup_cgroups(sandbox_id);

    match status {
        nix::sys::wait::WaitStatus::Exited(_, code) if code != 0 => {
            anyhow::bail!("Sandbox exited with code {}", code)
        }
        nix::sys::wait::WaitStatus::Signaled(_, sig, _) => {
            anyhow::bail!("Sandbox killed by signal {:?}", sig)
        }
        _ => {}
    }

    Ok(())
}

/// Write uid_map / gid_map so the child has a valid user inside its user namespace.
/// Maps sandbox uid/gid 0 → host uid/gid 0.
/// Even though the numeric ID is the same, the user-namespace boundary means
/// capabilities gained inside the namespace do NOT extend to the parent (init)
/// user namespace — this is the key security property.
fn setup_user_ns(pid: i32) -> anyhow::Result<()> {
    let proc = format!("/proc/{}", pid);

    // Allow setgroups inside the namespace (requires CAP_SETGID in parent ns)
    fs::write(format!("{}/setgroups", proc), "allow")?;

    // Map a full range of UIDs/GIDs so tools like apt can drop privileges
    // (e.g. _apt user uid 42, nobody gid 65534)
    fs::write(format!("{}/uid_map", proc), "0 0 65536\n")?;
    fs::write(format!("{}/gid_map", proc), "0 0 65536\n")?;

    info!("User namespace ID mappings written for pid {}", pid);
    Ok(())
}

fn setup_cgroups(sandbox_id: &str, pid: i32, limits: &ResourceLimits) -> anyhow::Result<()> {
    let cg_root = "/sys/fs/cgroup";
    let cg_base = "/sys/fs/cgroup/mini-daytona";
    let cg_path = format!("{}/{}", cg_base, sandbox_id);

    // cgroup v2 delegation chain: must enable controllers at each level
    // /sys/fs/cgroup/cgroup.subtree_control → +memory +pids +cpu
    //   └─ /sys/fs/cgroup/mini-daytona/cgroup.subtree_control → +memory +pids +cpu
    //       └─ /sys/fs/cgroup/mini-daytona/<sandbox-id>/memory.max  ← writable

    // Evacuate processes from root cgroup to init cgroup to allow subtree delegation
    let root_procs_path = format!("{}/cgroup.procs", cg_root);
    if let Ok(procs) = fs::read_to_string(&root_procs_path) {
        if !procs.trim().is_empty() {
            let init_cg = format!("{}/init", cg_root);
            if fs::create_dir_all(&init_cg).is_ok() {
                for pid in procs.split_whitespace() {
                    let _ = fs::write(format!("{}/cgroup.procs", init_cg), pid);
                }
            }
        }
    }

    // Step 1: Enable controllers at the ROOT cgroup level
    if let Err(e) = fs::write(
        format!("{}/cgroup.subtree_control", cg_root),
        "+memory +pids +cpu",
    ) {
        warn!("Failed to enable root cgroup subtree controllers: {}", e);
    }

    // Step 2: Ensure mini-daytona cgroup directory exists
    if !fs::metadata(cg_base).is_ok() {
        fs::create_dir_all(cg_base)?;
    }

    // Step 3: Enable controllers at the mini-daytona level
    if let Err(e) = fs::write(
        format!("{}/cgroup.subtree_control", cg_base),
        "+memory +pids +cpu",
    ) {
        warn!("Failed to enable mini-daytona cgroup subtree controllers: {}", e);
    }

    // Step 4: Create the cgroup for this sandbox
    fs::create_dir_all(&cg_path)?;

    // Set memory limit
    let mem = limits.memory_bytes.unwrap_or(1_073_741_824);
    if let Err(e) = fs::write(format!("{}/memory.max", cg_path), mem.to_string()) {
        warn!("Failed to set memory limit: {}", e);
    }

    // Set pids limit
    let pids = limits.pids_max.unwrap_or(512);
    if let Err(e) = fs::write(format!("{}/pids.max", cg_path), pids.to_string()) {
        warn!("Failed to set pids limit: {}", e);
    }

    // Set cpu limit: quota period
    let quota = limits.cpu_quota.unwrap_or(100_000);
    let period = limits.cpu_period.unwrap_or(100_000);
    if let Err(e) = fs::write(format!("{}/cpu.max", cg_path), format!("{} {}", quota, period)) {
        warn!("Failed to set cpu limit: {}", e);
    }

    // Write PID to cgroup.procs
    fs::write(format!("{}/cgroup.procs", cg_path), pid.to_string())?;

    info!("Cgroups configured for {}: memory={}B, cpu={}/{}, pids={}", sandbox_id, mem, quota, period, pids);
    Ok(())
}

fn cleanup_cgroups(sandbox_id: &str) -> anyhow::Result<()> {
    let cg_path = format!("/sys/fs/cgroup/mini-daytona/{}", sandbox_id);
    if fs::metadata(&cg_path).is_ok() {
        fs::remove_dir(&cg_path)?;
    }
    Ok(())
}

fn child(merged_dir: &std::ffi::CStr, cmd: &[CString], workdir: Option<&std::ffi::CStr>, profile: SandboxProfile) -> anyhow::Result<()> {
    sethostname("mini-daytona")?;

    // Make all mounts slave so changes don't propagate to the host,
    // but incoming propagation is still allowed (compatible with user namespaces).
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_SLAVE | MsFlags::MS_REC,
        None::<&str>,
    ).map_err(|e| anyhow::anyhow!("MS_SLAVE mount failed: {}", e))?;

    let new_root = merged_dir.to_str()?;

    // Bind-mount merged_dir onto itself — pivot_root requires new_root to be a mount point
    mount(
        Some(new_root),
        new_root,
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        None::<&str>,
    ).map_err(|e| anyhow::anyhow!("bind mount {} failed: {}", new_root, e))?;

    // Setup /dev with essential device nodes by bind-mounting from host
    setup_dev(new_root)?;

    // Bind-mount host /proc into new root BEFORE pivot_root (while host /proc is still accessible)
    {
        let proc_dir = format!("{}/proc", new_root);
        let _ = fs::create_dir_all(&proc_dir);
        if let Err(e) = mount(
            Some("/proc"),
            proc_dir.as_str(),
            None::<&str>,
            MsFlags::MS_BIND | MsFlags::MS_REC,
            None::<&str>,
        ) {
            warn!("Could not bind-mount /proc into new root: {}", e);
        }
    }

    // Create a directory inside the new root to serve as the put_old mount point
    let put_old = format!("{}/.pivot_old", new_root);
    let _ = nix::unistd::mkdir(
        put_old.as_str(),
        Mode::S_IRWXU,
    );

    // pivot_root: swap root filesystem
    pivot_root(new_root, put_old.as_str())
        .map_err(|e| anyhow::anyhow!("pivot_root failed: {}", e))?;

    chdir("/")?;

    // Unmount the old root (now at /.pivot_old) and remove the directory
    umount2("/.pivot_old", MntFlags::MNT_DETACH)
        .map_err(|e| anyhow::anyhow!("umount old root failed: {}", e))?;
    let _ = fs::remove_dir("/.pivot_old");

    // Apply /proc security masks (profile-specific).
    mask_proc(profile)?;

    // Mount /sys read-only for additional isolation in runtime mode.
    if profile == SandboxProfile::Runtime {
        mask_sys();
    }

    // Create cgroup namespace AFTER /proc is mounted (runc-style).
    // This hides host cgroup paths from the sandbox while keeping /proc functional.
    if let Err(e) = nix::sched::unshare(CloneFlags::CLONE_NEWCGROUP) {
        warn!("Failed to create cgroup namespace: {}", e);
    }

    // Profile-specific hardening
    match profile {
        SandboxProfile::Build => {
            // Build mode: keep capabilities (apt needs setuid/setgroups),
            // do NOT set no_new_privs. Apply base seccomp whitelist only.
            if let Err(e) = setup_seccomp(profile) {
                error!("Failed to setup seccomp filter: {}", e);
            }
        }
        SandboxProfile::Runtime => {
            // Runtime mode: lock down hard.
            // 1. Drop ALL capabilities — runtime workloads don't need them.
            if let Err(e) = drop_capabilities() {
                warn!("Failed to drop capabilities: {}", e);
            }
            // 2. Set PR_SET_NO_NEW_PRIVS — prevent privilege escalation via
            //    setuid binaries. Must be set BEFORE seccomp for the filter to
            //    be truly immutable (no process can add_rule after this).
            let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
            if ret != 0 {
                warn!("Failed to set PR_SET_NO_NEW_PRIVS: errno {}", nix::errno::errno());
            }
            // 3. Apply runtime seccomp profile (base whitelist minus runtime denylist).
            if let Err(e) = setup_seccomp(profile) {
                error!("Failed to setup seccomp filter: {}", e);
            }
        }
    }

    // Change to workdir if specified (Dockerfile WORKDIR support)
    if let Some(wd) = workdir {
        chdir(wd)?;
    }

    if !cmd.is_empty() {
        execvp(&cmd[0], cmd)?;
    } else {
        let sh = CString::new("/bin/sh")?;
        execvp(&sh, std::slice::from_ref(&sh))?;
    }

    Ok(())
}

/// Mask sensitive /proc paths. In runtime mode, applies the full OCI-standard mask set.
fn mask_proc(profile: SandboxProfile) -> anyhow::Result<()> {
    let _ = fs::write("/.empty_file", "");

    // Base mask paths (always applied, both build and runtime)
    let base_mask: &[&str] = &[
        "/proc/kcore",
        "/proc/sched_debug",
        "/proc/sysrq-trigger",
        "/proc/timer_list",
        "/proc/timer_stats",
    ];

    // Extended mask paths for runtime (OCI-standard, Docker defaults)
    let runtime_mask: &[&str] = &[
        "/proc/acpi",
        "/proc/keys",
        "/proc/latency_stats",
        "/proc/scsi",
    ];

    let paths: Vec<&str> = match profile {
        SandboxProfile::Build => base_mask.to_vec(),
        SandboxProfile::Runtime => {
            let mut v = base_mask.to_vec();
            v.extend_from_slice(runtime_mask);
            v
        }
    };

    for path in &paths {
        if fs::metadata(path).is_ok() {
            let _ = mount(
                Some("/.empty_file"),
                *path,
                None::<&str>,
                MsFlags::MS_BIND,
                None::<&str>,
            );
        }
    }

    // Make /proc/sys read-only (both profiles — safe, needed for security)
    if fs::metadata("/proc/sys").is_ok() {
        let _ = mount(
            Some("/proc/sys"),
            "/proc/sys",
            None::<&str>,
            MsFlags::MS_BIND | MsFlags::MS_REC,
            None::<&str>,
        );
        let _ = mount(
            Some("/proc/sys"),
            "/proc/sys",
            None::<&str>,
            MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY | MsFlags::MS_REC,
            None::<&str>,
        );
    }

    // In runtime mode, also make /proc/bus and /proc/fs read-only
    if profile == SandboxProfile::Runtime {
        for dir in &["/proc/bus", "/proc/fs", "/proc/irq"] {
            if fs::metadata(dir).is_ok() {
                let _ = mount(
                    Some(*dir),
                    *dir,
                    None::<&str>,
                    MsFlags::MS_BIND | MsFlags::MS_REC,
                    None::<&str>,
                );
                let _ = mount(
                    Some(*dir),
                    *dir,
                    None::<&str>,
                    MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY | MsFlags::MS_REC,
                    None::<&str>,
                );
            }
        }
    }

    Ok(())
}

/// Make /sys read-only in runtime mode. Build mode may need sysfs write access
/// for certain package installation steps.
fn mask_sys() {
    if fs::metadata("/sys").is_ok() {
        let _ = mount(
            Some("/sys"),
            "/sys",
            None::<&str>,
            MsFlags::MS_BIND | MsFlags::MS_REC,
            None::<&str>,
        );
        let _ = mount(
            Some("/sys"),
            "/sys",
            None::<&str>,
            MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY | MsFlags::MS_REC,
            None::<&str>,
        );
    }
}

/// Setup essential /dev devices inside the new root by bind-mounting from host.
/// Must be called BEFORE pivot_root (while host /dev is still accessible).
fn setup_dev(new_root: &str) -> anyhow::Result<()> {
    let dev_dir = format!("{}/dev", new_root);
    fs::create_dir_all(&dev_dir)?;

    // Mount a tmpfs on /dev so we have a writable filesystem for devices
    mount(
        Some("tmpfs"),
        dev_dir.as_str(),
        Some("tmpfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_STRICTATIME,
        Some("mode=755,size=65536k"),
    ).map_err(|e| anyhow::anyhow!("Failed to mount tmpfs on {}: {}", dev_dir, e))?;

    // Essential device nodes to bind-mount from host
    let devices = [
        ("null", 0o666),
        ("zero", 0o666),
        ("full", 0o666),
        ("random", 0o666),
        ("urandom", 0o666),
        ("tty", 0o666),
    ];

    for (name, _mode) in &devices {
        let host_path = format!("/dev/{}", name);
        let target_path = format!("{}/{}", dev_dir, name);

        // Create an empty file as mount target
        if let Err(e) = fs::write(&target_path, "") {
            warn!("Could not create {} for bind mount: {}", target_path, e);
            continue;
        }

        // Bind-mount the host device into the new root
        if let Err(e) = mount(
            Some(host_path.as_str()),
            target_path.as_str(),
            None::<&str>,
            MsFlags::MS_BIND,
            None::<&str>,
        ) {
            warn!("Could not bind-mount {} -> {}: {}", host_path, target_path, e);
        }
    }

    // Create /dev/pts directory
    let pts_dir = format!("{}/pts", dev_dir);
    let _ = fs::create_dir_all(&pts_dir);

    // Create /dev/shm as tmpfs (needed by Chromium, databases, etc.)
    let shm_dir = format!("{}/shm", dev_dir);
    let _ = fs::create_dir_all(&shm_dir);
    if let Err(e) = mount(
        Some("shm"),
        shm_dir.as_str(),
        Some("tmpfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        Some("mode=1777,size=65536k"),
    ) {
        warn!("Could not mount tmpfs on /dev/shm: {}", e);
    }

    // Create symlinks: /dev/stdin, /dev/stdout, /dev/stderr, /dev/fd
    let _ = std::os::unix::fs::symlink("/proc/self/fd", format!("{}/fd", dev_dir));
    let _ = std::os::unix::fs::symlink("/proc/self/fd/0", format!("{}/stdin", dev_dir));
    let _ = std::os::unix::fs::symlink("/proc/self/fd/1", format!("{}/stdout", dev_dir));
    let _ = std::os::unix::fs::symlink("/proc/self/fd/2", format!("{}/stderr", dev_dir));

    info!("Essential /dev devices configured in {}", dev_dir);
    Ok(())
}

#[allow(dead_code)]
fn drop_capabilities() -> anyhow::Result<()> {
    caps::clear(None, caps::CapSet::Bounding)?;
    caps::clear(None, caps::CapSet::Effective)?;
    caps::clear(None, caps::CapSet::Inheritable)?;
    caps::clear(None, caps::CapSet::Permitted)?;
    Ok(())
}

fn setup_seccomp(profile: SandboxProfile) -> anyhow::Result<()> {
    use libseccomp::{ScmpAction, ScmpFilterContext, ScmpSyscall};
    use crate::os::linux::seccomp_whitelist::SECCOMP_WHITELIST;
    use crate::os::linux::seccomp_whitelist::SECCOMP_RUNTIME_DENYLIST;

    // Default action is ERRNO(EPERM) to turn on whitelist mode
    let mut ctx = ScmpFilterContext::new_filter(ScmpAction::Errno(libc::EPERM))?;

    // Build a set of denied syscalls for runtime profile
    let runtime_deny: std::collections::HashSet<&str> = if profile == SandboxProfile::Runtime {
        SECCOMP_RUNTIME_DENYLIST.iter().copied().collect()
    } else {
        std::collections::HashSet::new()
    };

    for syscall_name in SECCOMP_WHITELIST {
        // In runtime mode, skip syscalls on the denylist
        if runtime_deny.contains(syscall_name) {
            continue;
        }
        if let Ok(syscall) = ScmpSyscall::from_name(syscall_name) {
            // Ignore errors for syscalls that don't exist on this arch
            let _ = ctx.add_rule(ScmpAction::Allow, syscall);
        }
    }

    ctx.load()?;
    info!("Seccomp filter loaded ({:?} profile)", profile);
    Ok(())
}
