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
        fs::create_dir_all(&self.upper_dir)?;
        fs::create_dir_all(&self.work_dir)?;
        fs::create_dir_all(&self.merged_dir)?;

        let mut data = format!(
            "upperdir={},workdir={}",
            self.upper_dir.to_str().unwrap(),
            self.work_dir.to_str().unwrap()
        );

        if !self.lower_dirs.is_empty() {
            let lower_str = self
                .lower_dirs
                .iter()
                .map(|p| p.to_str().unwrap())
                .collect::<Vec<_>>()
                .join(":");
            data = format!("lowerdir={},{}", lower_str, data);
        } else {
             // To mount an overlay without lowerdir, you usually need a lowerdir. But there's a workaround: using the upperdir itself or simply skip lowerdir in some kernels, but typically it's required for standard overlayfs usage or at least multiple lowerdirs. 
             // We can just create an empty lower stack for safety.
             let dummy_lower = self.work_dir.parent().unwrap().join("empty_lower");
             fs::create_dir_all(&dummy_lower)?;
             data = format!("lowerdir={},{}", dummy_lower.to_str().unwrap(), data);
        }

        mount(
            Some("overlay"),
            self.merged_dir.as_os_str(),
            Some("overlay"),
            MsFlags::empty(),
            Some(data.as_str()),
        )?;

        Ok(())
    }

    pub fn unmount(&self) -> anyhow::Result<()> {
        umount(self.merged_dir.as_os_str())?;
        Ok(())
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
