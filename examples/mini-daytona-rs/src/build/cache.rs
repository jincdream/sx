use crate::metadata::{load_metadata, save_metadata, touch_build_artifact, BuildArtifactMetadata};
use crate::snapshot::{get_build_artifacts_dir, hardlink_copy};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::info;
use uuid::Uuid;

pub struct BuildArtifact {
    pub cache_key: String,
    pub dockerfile_md5: String,
    pub context_hash: String,
    pub snapshot_path: PathBuf,
}

pub enum BuildCacheScope {
    DockerfileMd5(String),
    CacheKey(String),
}

pub enum BuildCachePruneMode {
    RemoveMissingOnly,
    Scope(BuildCacheScope),
    ClearAll,
}

pub fn compute_dockerfile_md5(dockerfile_path: &Path) -> Result<String> {
    let content = fs::read(dockerfile_path)?;
    Ok(format!("{:x}", md5::compute(content)))
}

pub fn compute_context_hash(context_dir: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hash_directory(context_dir, context_dir, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn compute_build_cache_key(dockerfile_md5: &str, context_hash: &str) -> String {
    if let Some(ext) = crate::os::sys::get_cache_key_ext() {
        format!(
            "{:x}",
            md5::compute(format!("{}:{}:{}", dockerfile_md5, context_hash, ext))
        )
    } else {
        format!(
            "{:x}",
            md5::compute(format!("{}:{}", dockerfile_md5, context_hash))
        )
    }
}

pub fn resolve_cached_build_artifact(
    dockerfile_path: &Path,
    context_dir: &Path,
) -> Result<Option<BuildArtifact>> {
    let dockerfile_md5 = compute_dockerfile_md5(dockerfile_path)?;
    let context_hash = compute_context_hash(context_dir)?;
    let cache_key = compute_build_cache_key(&dockerfile_md5, &context_hash);
    let snapshot_path = build_artifact_path(&cache_key)?;

    if !snapshot_path.exists() {
        prune_stale_build_artifact(&cache_key)?;
        return Ok(None);
    }

    info!(
        "Using global build artifact cache for key={} dockerfile_md5={} at {:?}",
        cache_key, dockerfile_md5, snapshot_path
    );
    persist_build_artifact_metadata(
        cache_key.clone(),
        dockerfile_md5.clone(),
        context_hash.clone(),
        snapshot_path.clone(),
    )?;

    Ok(Some(BuildArtifact {
        cache_key,
        dockerfile_md5,
        context_hash,
        snapshot_path,
    }))
}

pub fn publish_build_artifact(
    dockerfile_md5: &str,
    context_hash: &str,
    rootfs_path: &Path,
) -> Result<BuildArtifact> {
    let cache_key = compute_build_cache_key(dockerfile_md5, context_hash);
    let snapshot_path = build_artifact_path(&cache_key)?;
    if snapshot_path.exists() {
        persist_build_artifact_metadata(
            cache_key.clone(),
            dockerfile_md5.to_string(),
            context_hash.to_string(),
            snapshot_path.clone(),
        )?;
        return Ok(BuildArtifact {
            cache_key,
            dockerfile_md5: dockerfile_md5.to_string(),
            context_hash: context_hash.to_string(),
            snapshot_path,
        });
    }

    let artifacts_dir = get_build_artifacts_dir()?;
    let staging_dir = artifacts_dir.join(format!("{}.tmp-{}", cache_key, Uuid::new_v4()));
    hardlink_copy(rootfs_path, &staging_dir)?;

    match fs::rename(&staging_dir, &snapshot_path) {
        Ok(()) => {}
        Err(_) if snapshot_path.exists() => {
            let _ = fs::remove_dir_all(&staging_dir);
        }
        Err(err) => {
            let _ = fs::remove_dir_all(&staging_dir);
            return Err(err.into());
        }
    }

    persist_build_artifact_metadata(
        cache_key.clone(),
        dockerfile_md5.to_string(),
        context_hash.to_string(),
        snapshot_path.clone(),
    )?;

    Ok(BuildArtifact {
        cache_key,
        dockerfile_md5: dockerfile_md5.to_string(),
        context_hash: context_hash.to_string(),
        snapshot_path,
    })
}

pub fn list_build_artifacts() -> Result<Vec<BuildArtifactMetadata>> {
    let mut artifacts: Vec<BuildArtifactMetadata> =
        load_metadata()?.build_artifacts.into_values().collect();
    artifacts.sort_by(|left, right| left.cache_key.cmp(&right.cache_key));
    Ok(artifacts)
}

pub fn prune_build_artifacts(mode: BuildCachePruneMode) -> Result<Vec<BuildArtifactMetadata>> {
    let mut metadata = load_metadata()?;
    let mut removed = Vec::new();

    match mode {
        BuildCachePruneMode::RemoveMissingOnly => {
            metadata.build_artifacts.retain(|_, artifact| {
                let exists = artifact.snapshot_path.exists();
                if !exists {
                    removed.push(artifact.clone());
                }
                exists
            });
        }
        BuildCachePruneMode::Scope(scope) => {
            let keys_to_remove: Vec<String> = metadata
                .build_artifacts
                .iter()
                .filter(|(cache_key, artifact)| matches_scope(cache_key, artifact, &scope))
                .map(|(cache_key, _)| cache_key.clone())
                .collect();

            for cache_key in keys_to_remove {
                if let Some(artifact) = metadata.build_artifacts.remove(&cache_key) {
                    let _ = fs::remove_dir_all(&artifact.snapshot_path);
                    removed.push(artifact);
                }
            }
        }
        BuildCachePruneMode::ClearAll => {
            for (_, artifact) in metadata.build_artifacts.drain() {
                let _ = fs::remove_dir_all(&artifact.snapshot_path);
                removed.push(artifact);
            }
        }
    }

    save_metadata(&metadata)?;
    Ok(removed)
}

fn build_artifact_path(cache_key: &str) -> Result<PathBuf> {
    Ok(get_build_artifacts_dir()?.join(cache_key))
}

fn persist_build_artifact_metadata(
    cache_key: String,
    dockerfile_md5: String,
    context_hash: String,
    snapshot_path: PathBuf,
) -> Result<()> {
    let mut metadata = load_metadata()?;
    touch_build_artifact(
        &mut metadata,
        cache_key,
        dockerfile_md5,
        context_hash,
        snapshot_path,
    );
    save_metadata(&metadata)
}

fn prune_stale_build_artifact(cache_key: &str) -> Result<()> {
    let mut metadata = load_metadata()?;
    if metadata.build_artifacts.remove(cache_key).is_some() {
        save_metadata(&metadata)?;
    }
    Ok(())
}

fn hash_directory(root: &Path, dir: &Path, hasher: &mut Sha256) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(root)?;
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            hasher.update(b"dir:");
            hasher.update(relative.to_string_lossy().as_bytes());
            hash_directory(root, &path, hasher)?;
            continue;
        }

        if file_type.is_symlink() {
            hasher.update(b"symlink:");
            hasher.update(relative.to_string_lossy().as_bytes());
            hasher.update(fs::read_link(&path)?.to_string_lossy().as_bytes());
            continue;
        }

        hasher.update(b"file:");
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update(entry.metadata()?.len().to_le_bytes());
        hasher.update(fs::read(&path)?);
    }

    Ok(())
}

fn matches_scope(
    cache_key: &str,
    artifact: &BuildArtifactMetadata,
    scope: &BuildCacheScope,
) -> bool {
    match scope {
        BuildCacheScope::DockerfileMd5(value) => artifact.dockerfile_md5 == *value,
        BuildCacheScope::CacheKey(value) => cache_key == value,
    }
}
