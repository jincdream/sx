#![allow(unused)]
/// Security profile that controls the isolation level of a sandbox.
/// Build sandboxes need more privileges (e.g. apt-get, npm install).
/// Runtime sandboxes are locked down for defense-in-depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SandboxProfile {
    /// Used during `docker build`-style operations. More permissive:
    /// - Retains capabilities (setuid/setgroups for apt)
    /// - Wider seccomp whitelist
    /// - no_new_privs NOT set
    Build,
    /// Used when running user workloads (Chromium, Node, Python, etc.).
    /// Locked down:
    /// - Drops all capabilities
    /// - Sets PR_SET_NO_NEW_PRIVS
    /// - Narrower seccomp profile (runtime denylist applied on top)
    /// - Extended /proc and /sys masking
    Runtime,
}

impl Default for SandboxProfile {
    fn default() -> Self {
        SandboxProfile::Runtime
    }
}

/// Configurable resource limits for a sandbox (cgroups v2).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ResourceLimits {
    /// Memory limit in bytes (default: 1 GiB)
    #[serde(default)]
    pub memory_bytes: Option<u64>,
    /// CPU quota in microseconds per period (default: 100000 = 1 core)
    #[serde(default)]
    pub cpu_quota: Option<u64>,
    /// CPU period in microseconds (default: 100000)
    #[serde(default)]
    pub cpu_period: Option<u64>,
    /// Maximum number of PIDs (default: 512)
    #[serde(default)]
    pub pids_max: Option<u64>,
    /// Disk space limit in bytes (default: 2 GiB, informational — not enforced via cgroups)
    #[serde(default)]
    pub disk_bytes: Option<u64>,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            memory_bytes: Some(1_073_741_824), // 1 GiB
            cpu_quota: Some(100_000),          // 1 core
            cpu_period: Some(100_000),
            pids_max: Some(512),
            disk_bytes: Some(2_147_483_648), // 2 GiB
        }
    }
}

// Re-export the platform-specific run_sandbox at the module level
pub use crate::os::sys::run_sandbox;
