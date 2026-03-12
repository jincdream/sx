use nix::mount::{mount, umount2, MntFlags, MsFlags};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::info;

pub fn mount_overlay(
    merged_dir: &Path,
    lower_dirs: &[PathBuf],
    upper_dir: &Path,
    work_dir: &Path,
) -> anyhow::Result<()> {
    fs::create_dir_all(upper_dir)?;
    fs::create_dir_all(work_dir)?;
    fs::create_dir_all(merged_dir)?;

    info!("Mounting overlayfs at {:?}", merged_dir);

    let base_data = format!(
        "upperdir={},workdir={}",
        upper_dir.to_str().unwrap(),
        work_dir.to_str().unwrap()
    );

    let lower_part = if !lower_dirs.is_empty() {
        let lower_str = lower_dirs
            .iter()
            .rev()
            .map(|p| p.to_str().unwrap())
            .collect::<Vec<_>>()
            .join(":");
        format!("lowerdir={}", lower_str)
    } else {
        let dummy_lower = work_dir.parent().unwrap().join("empty_lower");
        fs::create_dir_all(&dummy_lower)?;
        format!("lowerdir={}", dummy_lower.to_str().unwrap())
    };

    let data_with_xattr = format!("{},userxattr,{}", lower_part, base_data);
    let data_without_xattr = format!("{},{}", lower_part, base_data);

    let result = mount(
        Some("overlay"),
        merged_dir.as_os_str(),
        Some("overlay"),
        MsFlags::empty(),
        Some(data_with_xattr.as_str()),
    );

    match result {
        Ok(()) => Ok(()),
        Err(_) => {
            mount(
                Some("overlay"),
                merged_dir.as_os_str(),
                Some("overlay"),
                MsFlags::empty(),
                Some(data_without_xattr.as_str()),
            )?;
            Ok(())
        }
    }
}

pub fn unmount_overlay(merged_dir: &Path) -> anyhow::Result<()> {
    umount2(merged_dir.as_os_str(), MntFlags::MNT_DETACH)
        .map_err(|e| anyhow::anyhow!("nix::umount2 failed: {}", e))?;
    Ok(())
}
