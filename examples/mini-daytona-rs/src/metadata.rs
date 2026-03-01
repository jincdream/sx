use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMetadata {
    pub id: String,
    pub path: PathBuf,
    pub created_at: String,
    pub entrypoint: Option<Vec<String>>,
    pub cmd: Option<Vec<String>>,
    pub env: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxMetadata {
    pub id: String,
    pub snapshot_id: String,
    pub created_at: String,
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Metadata {
    pub snapshots: HashMap<String, SnapshotMetadata>,
    pub sandboxes: HashMap<String, SandboxMetadata>,
}

pub fn get_metadata_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME")?;
    let dir = PathBuf::from(home).join(".mini-daytona/metadata");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn metadata_path() -> anyhow::Result<PathBuf> {
    Ok(get_metadata_dir()?.join("state.json"))
}

pub fn load_metadata() -> anyhow::Result<Metadata> {
    let path = metadata_path()?;
    if !path.exists() {
        return Ok(Metadata::default());
    }
    let content = fs::read_to_string(path)?;
    let metadata: Metadata = serde_json::from_str(&content)?;
    Ok(metadata)
}

pub fn save_metadata(metadata: &Metadata) -> anyhow::Result<()> {
    let path = metadata_path()?;
    let content = serde_json::to_string_pretty(metadata)?;
    fs::write(path, content)?;
    Ok(())
}
