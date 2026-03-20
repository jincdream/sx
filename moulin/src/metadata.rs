use crate::sandbox::ResourceLimits;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMetadata {
    pub id: String,
    pub path: PathBuf,
    pub created_at: String,
    pub entrypoint: Option<Vec<String>>,
    pub cmd: Option<Vec<String>>,
    pub env: Option<Vec<String>>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMetadata {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub created_at: String,
}

/// Describes a volume mount inside a sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountConfig {
    /// Volume ID to mount
    pub volume_id: String,
    /// Absolute path inside the container, e.g. "/data"
    pub mount_path: String,
    /// Mount the volume read-only (default: false)
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxMetadata {
    pub id: String,
    pub snapshot_id: String,
    pub created_at: String,
    pub dir: PathBuf,
    /// PID of the sandbox init process (for nsenter-based exec)
    #[serde(default)]
    pub pid: Option<i32>,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub resources: ResourceLimits,
    /// Volumes mounted into this sandbox
    #[serde(default)]
    pub mounts: Vec<MountConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildArtifactMetadata {
    pub cache_key: String,
    pub dockerfile_md5: String,
    pub context_hash: String,
    pub snapshot_path: PathBuf,
    pub created_at: String,
    pub last_used_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Metadata {
    pub snapshots: HashMap<String, SnapshotMetadata>,
    pub sandboxes: HashMap<String, SandboxMetadata>,
    #[serde(default)]
    pub build_artifacts: HashMap<String, BuildArtifactMetadata>,
    #[serde(default)]
    pub volumes: HashMap<String, VolumeMetadata>,
}

pub fn get_volumes_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME")?;
    let dir = PathBuf::from(home).join(".moulin/volumes");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn get_metadata_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME")?;
    let dir = PathBuf::from(home).join(".moulin/metadata");
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

pub fn register_snapshot(
    metadata: &mut Metadata,
    path: PathBuf,
    entrypoint: Option<Vec<String>>,
    cmd: Option<Vec<String>>,
    env: Option<Vec<String>>,
    name: Option<String>,
    description: Option<String>,
) -> String {
    let snapshot_id = Uuid::new_v4().to_string();
    metadata.snapshots.insert(
        snapshot_id.clone(),
        SnapshotMetadata {
            id: snapshot_id.clone(),
            path,
            created_at: Utc::now().to_rfc3339(),
            entrypoint,
            cmd,
            env,
            name,
            description,
        },
    );
    snapshot_id
}

pub fn touch_build_artifact(
    metadata: &mut Metadata,
    cache_key: String,
    dockerfile_md5: String,
    context_hash: String,
    snapshot_path: PathBuf,
) {
    let now = Utc::now().to_rfc3339();
    metadata
        .build_artifacts
        .entry(cache_key.clone())
        .and_modify(|entry| {
            entry.snapshot_path = snapshot_path.clone();
            entry.context_hash = context_hash.clone();
            entry.last_used_at = now.clone();
        })
        .or_insert(BuildArtifactMetadata {
            cache_key,
            dockerfile_md5,
            context_hash,
            snapshot_path,
            created_at: now.clone(),
            last_used_at: now,
        });
}
