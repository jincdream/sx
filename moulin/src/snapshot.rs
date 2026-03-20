use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use tar::{Archive, Builder};

pub fn create_archive(source_dir: &Path, output_path: &Path) -> anyhow::Result<()> {
    let tar_gz = File::create(output_path)?;
    let enc = GzEncoder::new(tar_gz, Compression::default());
    let mut tar = Builder::new(enc);
    tar.follow_symlinks(false);
    tar.append_dir_all(".", source_dir)?;
    tar.finish()?;
    Ok(())
}

pub fn extract_archive(archive_path: &Path, dest_dir: &Path) -> anyhow::Result<()> {
    let tar_gz = File::open(archive_path)?;
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);
    archive.unpack(dest_dir)?;
    Ok(())
}

/// Recursively hardlink-copy a directory tree (equivalent to `cp -al`).
/// Files are hardlinked (zero-copy, instant), directories and symlinks are recreated.
/// Falls back to `fs::copy` transparently if source and destination are on different
/// filesystems (cross-device link).
pub fn hardlink_copy(src: &Path, dst: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dst)?;
    // Try hardlink first; if we get EXDEV, fall back to copy mode for rest of tree.
    hardlink_copy_inner(src, dst, true)
}

fn hardlink_copy_inner(src: &Path, dst: &Path, try_hardlink: bool) -> anyhow::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = entry.file_type()?;

        if ft.is_dir() {
            fs::create_dir_all(&dst_path)?;
            // Preserve directory permissions
            let meta = fs::metadata(&src_path)?;
            let _ = fs::set_permissions(&dst_path, meta.permissions());
            hardlink_copy_inner(&src_path, &dst_path, try_hardlink)?;
        } else if ft.is_symlink() {
            let target = fs::read_link(&src_path)?;
            // Destination symlink may already exist if hardlink partially succeeded
            let _ = std::os::unix::fs::symlink(&target, &dst_path);
        } else if try_hardlink {
            // Try hardlink first (zero-copy, instant)
            match fs::hard_link(&src_path, &dst_path) {
                Ok(()) => {}
                Err(e) if e.raw_os_error() == Some(18 /* EXDEV */) => {
                    // Cross-device: fall back to copy for this file and all remaining
                    fs::copy(&src_path, &dst_path)?;
                    // Re-scan remaining entries in copy-only mode
                    return hardlink_copy_inner_remaining(src, dst, &entry);
                }
                Err(e) => return Err(e.into()),
            }
        } else {
            // Copy mode (used when hardlinks are not possible)
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// After detecting cross-device, finish the rest of the current directory in copy mode,
/// then recurse in copy mode for all subdirectories not yet visited.
fn hardlink_copy_inner_remaining(
    src: &Path,
    dst: &Path,
    _already_done: &std::fs::DirEntry,
) -> anyhow::Result<()> {
    // Just re-walk the whole directory in copy-only mode.
    // Already-copied files will be skipped via the error-ignore on symlink creation
    // and overwrite on fs::copy. This is simpler and still fast.
    hardlink_copy_inner(src, dst, false)
}

pub fn get_data_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME")?;
    let dir = PathBuf::from(home).join(".moulin");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn get_bases_dir() -> anyhow::Result<PathBuf> {
    let dir = get_data_dir()?.join("bases");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn get_cache_dir() -> anyhow::Result<PathBuf> {
    let dir = get_data_dir()?.join("cache");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn get_build_artifacts_dir() -> anyhow::Result<PathBuf> {
    let dir = get_data_dir()?.join("build-artifacts");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn get_snapshots_dir() -> anyhow::Result<PathBuf> {
    let dir = get_data_dir()?.join("snapshots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn get_sandboxes_dir() -> anyhow::Result<PathBuf> {
    let dir = get_data_dir()?.join("sandboxes");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}
