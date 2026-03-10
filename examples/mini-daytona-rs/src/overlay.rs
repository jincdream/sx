#[cfg(target_os = "linux")]
use nix::mount::{mount, umount, MsFlags};
use std::fs;
use std::path::PathBuf;

#[derive(Debug)]
pub struct OverlayMount {
    pub lower_dirs: Vec<PathBuf>,
    pub upper_dir: PathBuf,
    pub work_dir: PathBuf,
    pub merged_dir: PathBuf,
}

impl OverlayMount {
    pub fn new(
        lower_dirs: Vec<PathBuf>,
        upper_dir: PathBuf,
        work_dir: PathBuf,
        merged_dir: PathBuf,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            lower_dirs,
            upper_dir,
            work_dir,
            merged_dir,
        })
    }

    pub fn mount(&self) -> anyhow::Result<()> {
        crate::os::sys::mount_overlay(
            &self.merged_dir,
            &self.lower_dirs,
            &self.upper_dir,
            &self.work_dir
        )
    }

    pub fn unmount(&self) -> anyhow::Result<()> {
        crate::os::sys::unmount_overlay(&self.merged_dir)
    }

    #[allow(dead_code)]
    pub fn cleanup(&self) -> anyhow::Result<()> {
        if self.merged_dir.exists() {
            let _ = fs::remove_dir_all(&self.merged_dir);
        }
        if self.upper_dir.exists() {
            let _ = fs::remove_dir_all(&self.upper_dir);
        }
        if self.work_dir.exists() {
            let _ = fs::remove_dir_all(&self.work_dir);
        }
        Ok(())
    }
}

