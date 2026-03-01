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

pub fn get_data_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME")?;
    let dir = PathBuf::from(home).join(".mini-daytona");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn get_snapshots_dir() -> anyhow::Result<PathBuf> {
    let dir = get_data_dir()?.join("snapshots");
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

pub fn get_sandboxes_dir() -> anyhow::Result<PathBuf> {
    let dir = get_data_dir()?.join("sandboxes");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}
