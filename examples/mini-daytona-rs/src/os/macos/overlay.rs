use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

pub fn mount_overlay(merged_dir: &Path, lower_dirs: &[PathBuf], upper_dir: &Path, _work_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(upper_dir)?;
    fs::create_dir_all(_work_dir)?;
    fs::create_dir_all(merged_dir)?;

    info!("[macOS] Simulating overlay mount via directory copy");

    // Copy lower dirs in order (first = bottom, last = top)
    for lower in lower_dirs {
        if lower.exists() {
            copy_dir_contents(lower, merged_dir, "lower dir")?;
            // Ensure merged tree stays writable so subsequent layers can overwrite files
            let _ = make_tree_user_writable(merged_dir);
        }
    }

    // Copy upper dir on top (highest priority, overrides lower layers)
    if upper_dir.exists() {
        let entries: Vec<_> = fs::read_dir(upper_dir)?
            .filter_map(|e| e.ok())
            .collect();
        if !entries.is_empty() {
            copy_dir_contents(upper_dir, merged_dir, "upper dir")?;
        }
    }

    // The simulated merged tree should behave like a writable container rootfs.
    make_tree_user_writable(merged_dir)?;

    info!("[macOS] Overlay simulation complete: {:?}", merged_dir);
    Ok(())
}

pub fn unmount_overlay(merged_dir: &Path) -> anyhow::Result<()> {
    info!("[macOS] Simulating overlay unmount for merged_dir {:?}", merged_dir);
    // Real code removes the active content so there is "nothing" mounted.
    // the parent cleanup will delete everything else anyway
    if merged_dir.exists() {
        // we can leave this here since destroy does remove_dir_all
    }
    Ok(())
}

fn copy_dir_contents(src: &Path, dst: &Path, label: &str) -> anyhow::Result<()> {
    let status = std::process::Command::new("rsync")
        .arg("-a")           // archive mode (preserves symlinks, permissions, etc.)
        .arg("--chmod=u+rwX") // ensure owner can write, so later layers can overwrite
        .arg("--ignore-errors")
        .arg(format!("{}/", src.to_string_lossy())) // trailing slash = contents only
        .arg(format!("{}/", dst.to_string_lossy()))
        .status();

    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => {
            tracing::warn!("[macOS] rsync had non-zero exit for {} {:?} (some special files may have been skipped)", label, src);
            Ok(())
        }
        Err(_) => {
            tracing::warn!("[macOS] rsync not found, falling back to cp -a for {} {:?}", label, src);
            let status = std::process::Command::new("cp")
                .arg("-a")
                .arg(format!("{}/.", src.to_string_lossy()))
                .arg(dst)
                .status()
                .map_err(|e| anyhow::anyhow!("Failed to execute cp -a for {} {:?}: {}", label, src, e))?;

            if !status.success() {
                tracing::warn!("[macOS] cp -a had non-zero exit for {} {:?} (likely some special files/symlinks failed to copy)", label, src);
            }
            Ok(())
        }
    }
}

fn make_tree_user_writable(path: &Path) -> anyhow::Result<()> {
    let status = std::process::Command::new("chmod")
        .arg("-R")
        .arg("u+rwX")
        .arg(path)
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to chmod merged tree {:?}: {}", path, e))?;

    if !status.success() {
        anyhow::bail!("chmod -R u+rwX failed for {:?}", path);
    }

    Ok(())
}
