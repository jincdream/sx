use tracing::info;

pub fn ensure_bridge() -> anyhow::Result<()> {
    info!("[macOS] Network bridge setup skipped (sandboxes use host networking)");
    Ok(())
}

#[allow(dead_code)]
pub fn setup_sandbox_net(
    _child_pid: i32,
    _sandbox_index: u8,
    _merged_dir: Option<&str>,
) -> anyhow::Result<()> {
    info!("[macOS] Network setup skipped (sandboxes use host networking)");
    Ok(())
}

#[allow(dead_code)]
pub fn teardown_sandbox_net(_sandbox_index: u8) {
    // No-op on macOS
}
