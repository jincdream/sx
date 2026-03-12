use crate::sandbox::{run_sandbox, SandboxProfile};
use std::path::Path;
use uuid::Uuid;

pub fn get_cache_key_ext() -> Option<String> {
    None
}

pub fn build_instruction(
    cmd_str: &str,
    merged_dir: &Path,
    workdir: &str,
    _env: &[String],
) -> anyhow::Result<()> {
    let build_sandbox_id = format!("build-{}", Uuid::new_v4());
    run_sandbox(
        &build_sandbox_id,
        merged_dir.to_str().unwrap(),
        &["/bin/sh", "-c", cmd_str],
        None,
        Some(workdir),
        SandboxProfile::Build,
    )
}
