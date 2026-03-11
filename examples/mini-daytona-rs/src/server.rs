use axum::{
    extract::{Path, Query, State, DefaultBodyLimit},
    routing::{get, post, delete},
    Json, Router,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing::{info, error};
use std::sync::Arc;

use crate::build::build;
use crate::build::cache::{list_build_artifacts, prune_build_artifacts, BuildCachePruneMode, BuildCacheScope};
use crate::build::parser::{parse_dockerfile, Instruction};
use crate::metadata::{load_metadata, register_snapshot, save_metadata, SandboxMetadata};
use crate::overlay::OverlayMount;
use crate::sandbox::{run_sandbox, ResourceLimits, SandboxProfile};
use crate::snapshot::{create_archive, get_sandboxes_dir};
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {}

#[derive(Serialize)]
pub struct ApiResponse<T> {
    success: bool,
    data: Option<T>,
    error: Option<String>,
}

#[derive(Deserialize)]
pub struct BuildRequest {
    dockerfile: String,
    context: String,
}

#[derive(Serialize)]
pub struct BuildResponse {
    snapshot_path: String,
    snapshot_id: String,
}

#[derive(Deserialize)]
pub struct StartRequest {
    snapshot: String,
    /// Optional resource limits for the sandbox
    #[serde(default)]
    resources: Option<ResourceLimits>,
}

#[derive(Serialize)]
pub struct StartResponse {
    sandbox_id: String,
}

#[derive(Deserialize)]
pub struct SnapshotRequest {
    sandbox_id: String,
    output: String,
}

#[derive(Deserialize)]
pub struct ExecRequest {
    cmd: Vec<String>,
}

#[derive(Serialize)]
pub struct ExecResponse {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

#[derive(Serialize)]
pub struct ServerInfoResponse {
    pub os: &'static str,
    pub degraded_mode: bool,
    pub supports_image_exec: bool,
}

#[derive(Deserialize)]
pub struct BuildCachePruneRequest {
    #[serde(default)]
    dockerfile_md5: Option<String>,
    #[serde(default)]
    cache_key: Option<String>,
    #[serde(default)]
    clear_all: bool,
}

#[derive(Serialize)]
pub struct BuildCacheEntryResponse {
    cache_key: String,
    dockerfile_md5: String,
    context_hash: String,
    snapshot_path: String,
    created_at: String,
    last_used_at: String,
}

#[derive(Deserialize)]
pub struct FileWriteRequest {
    path: String,
    content: String,
}

#[derive(Deserialize)]
pub struct FileDeleteRequest {
    path: String,
}

#[derive(Deserialize)]
pub struct FileUploadRequest {
    /// Target path inside the sandbox
    path: String,
    /// Base64-encoded file content
    data: String,
}

#[derive(Deserialize)]
pub struct FileDownloadQuery {
    /// File path inside the sandbox
    path: String,
}

#[derive(Deserialize)]
pub struct FileReadQuery {
    path: String,
}

pub async fn run_server() -> anyhow::Result<()> {
    // Initialize the network bridge for sandbox isolation
    crate::netns::ensure_bridge()?;

    let app = Router::new()
        .route("/api/info", get(handle_info))
        .route("/api/build", post(handle_build))
        .route("/api/build-cache", get(handle_build_cache_list))
        .route("/api/build-cache/prune", post(handle_build_cache_prune))
        .route("/api/start", post(handle_start))
        .route("/api/snapshot", post(handle_snapshot))
        .route("/api/list", get(handle_list))
        .route("/api/sandbox/{id}", delete(handle_destroy))
        .route("/api/sandbox/{id}/info", get(handle_sandbox_info))
        .route("/api/sandbox/{id}/exec", post(handle_exec))
        .route("/api/sandbox/{id}/file", get(handle_file_read))
        .route("/api/sandbox/{id}/file", post(handle_file_write))
        .route("/api/sandbox/{id}/file", delete(handle_file_delete))
        .route("/api/sandbox/{id}/upload", post(handle_file_upload))
        .route("/api/sandbox/{id}/download", get(handle_file_download))
        .route("/api/sandbox/{id}/suspend", post(handle_suspend))
        .route("/api/sandbox/{id}/resume", post(handle_resume))
        .with_state(Arc::new(AppState {}))
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024)); // 50 MiB body limit

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    info!("Starting API server on {}", addr);
    
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    
    Ok(())
}

/// Resolve a user-supplied relative path inside the sandbox merged directory.
/// Returns the canonicalized PathBuf only if:
///   1. No `..` components in the raw path (cheap pre-filter).
///   2. After canonicalize, the real path still lives under `merged_dir`.
///   3. The resolved path is not a symlink that points outside `merged_dir`.
///
/// This defends against TOCTOU, `..` traversal AND symlink-based escapes.
fn resolve_sandbox_path(
    merged_dir: &std::path::Path,
    user_path: &str,
) -> anyhow::Result<std::path::PathBuf> {
    let stripped = user_path.trim_start_matches('/');

    // 1. Quick reject obvious traversal
    if stripped.split('/').any(|c| c == "..") {
        anyhow::bail!("Path contains '..' traversal component");
    }

    let joined = merged_dir.join(stripped);

    // 2. Canonicalize what we can — the file (or even ancestor dirs) may not exist yet.
    let (canonical, tail) = if joined.exists() {
        (std::fs::canonicalize(&joined)?, None)
    } else {
        // Walk up the path until we find an existing ancestor to canonicalize.
        let mut ancestor = joined.clone();
        let mut suffix_parts: Vec<std::ffi::OsString> = Vec::new();
        loop {
            if let Some(parent) = ancestor.parent() {
                suffix_parts.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| anyhow::anyhow!("Missing file name component"))?
                        .to_os_string(),
                );
                ancestor = parent.to_path_buf();
                if ancestor.exists() {
                    break;
                }
            } else {
                anyhow::bail!("No existing ancestor directory found");
            }
        }
        let mut canon = std::fs::canonicalize(&ancestor)?;
        for part in suffix_parts.into_iter().rev() {
            canon = canon.join(part);
        }
        (canon, Some(()))
    };

    // 3. Canonicalize the merged_dir itself for a reliable prefix check
    let canon_root = std::fs::canonicalize(merged_dir)?;

    if !canonical.starts_with(&canon_root) {
        anyhow::bail!(
            "Resolved path escapes sandbox root (possible symlink attack)"
        );
    }

    // 4. If the path already exists and is a symlink, verify its target
    if tail.is_none() {
        let meta = std::fs::symlink_metadata(&canonical)?;
        if meta.file_type().is_symlink() {
            let link_target = std::fs::read_link(&canonical)?;
            let abs_target = if link_target.is_relative() {
                std::fs::canonicalize(canonical.parent().unwrap().join(&link_target))?
            } else {
                std::fs::canonicalize(&link_target)?
            };
            if !abs_target.starts_with(&canon_root) {
                anyhow::bail!(
                    "Symlink target escapes sandbox root"
                );
            }
        }
    }

    Ok(canonical)
}

async fn handle_info() -> Json<ApiResponse<ServerInfoResponse>> {
    let data = crate::os::sys::get_server_info();

    Json(ApiResponse {
        success: true,
        data: Some(data),
        error: None,
    })
}

async fn handle_build(
    State(_state): State<Arc<AppState>>,
    Json(payload): Json<BuildRequest>,
) -> Json<ApiResponse<BuildResponse>> {
    info!("API Build requested: {} context: {}", payload.dockerfile, payload.context);
    
    let result: anyhow::Result<BuildResponse> = tokio::task::spawn_blocking(move || {
        let dockerfile_path = PathBuf::from(payload.dockerfile);
        let context_path = PathBuf::from(payload.context);

        let snapshot_path = build(&dockerfile_path, &context_path)?;

        let (entrypoint, cmd, env) = extract_snapshot_config(&dockerfile_path)?;

        let mut metadata = load_metadata()?;
        let snapshot_id = register_snapshot(&mut metadata, snapshot_path.clone(), entrypoint, cmd, env);
        save_metadata(&metadata)?;

        Ok(BuildResponse {
            snapshot_path: snapshot_path.to_string_lossy().to_string(),
            snapshot_id,
        })
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

async fn handle_build_cache_list() -> Json<ApiResponse<Vec<BuildCacheEntryResponse>>> {
    let result = tokio::task::spawn_blocking(|| {
        let entries = list_build_artifacts()?
            .into_iter()
            .map(|artifact| BuildCacheEntryResponse {
                cache_key: artifact.cache_key,
                dockerfile_md5: artifact.dockerfile_md5,
                context_hash: artifact.context_hash,
                snapshot_path: artifact.snapshot_path.to_string_lossy().to_string(),
                created_at: artifact.created_at,
                last_used_at: artifact.last_used_at,
            })
            .collect();
        Ok::<_, anyhow::Error>(entries)
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

async fn handle_build_cache_prune(
    Json(payload): Json<BuildCachePruneRequest>,
) -> Json<ApiResponse<Vec<BuildCacheEntryResponse>>> {
    let result = tokio::task::spawn_blocking(move || {
        let mode = if payload.clear_all {
            BuildCachePruneMode::ClearAll
        } else if let Some(cache_key) = payload.cache_key {
            BuildCachePruneMode::Scope(BuildCacheScope::CacheKey(cache_key))
        } else if let Some(dockerfile_md5) = payload.dockerfile_md5 {
            BuildCachePruneMode::Scope(BuildCacheScope::DockerfileMd5(dockerfile_md5))
        } else {
            BuildCachePruneMode::RemoveMissingOnly
        };

        let entries = prune_build_artifacts(mode)?
            .into_iter()
            .map(|artifact| BuildCacheEntryResponse {
                cache_key: artifact.cache_key,
                dockerfile_md5: artifact.dockerfile_md5,
                context_hash: artifact.context_hash,
                snapshot_path: artifact.snapshot_path.to_string_lossy().to_string(),
                created_at: artifact.created_at,
                last_used_at: artifact.last_used_at,
            })
            .collect();
        Ok::<_, anyhow::Error>(entries)
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

async fn handle_start(
    State(_state): State<Arc<AppState>>,
    Json(payload): Json<StartRequest>,
) -> Json<ApiResponse<StartResponse>> {
    info!("API Start requested from snapshot: {}", payload.snapshot);
    
    let result: anyhow::Result<StartResponse> = tokio::task::spawn_blocking(move || {
        let snapshot_path = PathBuf::from(payload.snapshot);
        let resource_limits = payload.resources;
        let sandbox_id = Uuid::new_v4().to_string();
        let sandbox_dir = get_sandboxes_dir()?.join(&sandbox_id);

        let metadata = load_metadata()?;
        let snapshot_id = metadata
            .snapshots
            .values()
            .filter(|snapshot| snapshot.path == snapshot_path)
            .max_by(|left, right| left.created_at.cmp(&right.created_at))
            .map(|snapshot| snapshot.id.clone())
            .unwrap_or_default();
        
        let upper_dir = sandbox_dir.join("upper");
        let work_dir = sandbox_dir.join("work");
        let merged_dir = sandbox_dir.join("merged");
        
        std::fs::create_dir_all(&upper_dir)?;
        
        let overlay = OverlayMount::new(
            vec![snapshot_path],
            upper_dir,
            work_dir,
            merged_dir.clone(),
        )?;
        overlay.mount()?;
        
        let mut metadata = load_metadata()?;
        metadata.sandboxes.insert(
            sandbox_id.clone(),
            SandboxMetadata {
                id: sandbox_id.clone(),
            snapshot_id,
                created_at: Utc::now().to_rfc3339(),
                dir: sandbox_dir.clone(),
                pid: None,
                ip: None,
            },
        );
        save_metadata(&metadata)?;
        
        let local_sandbox_id = sandbox_id.clone();
        
        // Spawn the blocking sandbox process in another thread so we can return the ID
        std::thread::spawn(move || {
            info!("Starting sandbox execution: {}", local_sandbox_id);
            // Use an infinite sleep so the primary container process doesn't exit immediately
            let sid = local_sandbox_id.clone();
            if let Err(e) = run_sandbox(&sid, merged_dir.to_str().unwrap(), &["tail", "-f", "/dev/null"], resource_limits.as_ref(), None, SandboxProfile::Runtime) {
                error!("Sandbox {} failed: {}", local_sandbox_id, e);
            }
            // We do not unmount here, we leave it to handle_destroy so the user can interact via API.
        });

        Ok(StartResponse { sandbox_id })
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

#[derive(Serialize)]
pub struct ListResponse {
    snapshots: Vec<crate::metadata::SnapshotMetadata>,
    sandboxes: Vec<crate::metadata::SandboxMetadata>,
}

async fn handle_list() -> Json<ApiResponse<ListResponse>> {
    let result = tokio::task::spawn_blocking(|| {
        let metadata = load_metadata()?;
        Ok::<ListResponse, anyhow::Error>(ListResponse {
            snapshots: metadata.snapshots.into_values().collect(),
            sandboxes: metadata.sandboxes.into_values().collect(),
        })
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

async fn handle_snapshot(
    State(_state): State<Arc<AppState>>,
    Json(payload): Json<SnapshotRequest>,
) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&payload.sandbox_id) {
            let merged_dir = sandbox.dir.join("merged");
            let output = PathBuf::from(&payload.output);
            create_archive(&merged_dir, &output)?;
            Ok(payload.output)
        } else {
            anyhow::bail!("Sandbox {} not found", payload.sandbox_id);
        }
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

async fn handle_destroy(
    Path(id): Path<String>,
) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        let mut metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.remove(&id) {
            let merged_dir = sandbox.dir.join("merged");

            crate::os::sys::destroy_sandbox_os(&sandbox, &merged_dir);

            std::fs::remove_dir_all(&sandbox.dir)?;
            save_metadata(&metadata)?;
            Ok(format!("Destroyed sandbox {}", id))
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

#[derive(Serialize)]
pub struct SandboxInfoResponse {
    pub id: String,
    pub ip: Option<String>,
}

async fn handle_sandbox_info(
    Path(id): Path<String>,
) -> Json<ApiResponse<SandboxInfoResponse>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            Ok(SandboxInfoResponse {
                id: sandbox.id.clone(),
                ip: sandbox.ip.clone(),
            })
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

async fn handle_exec(
    Path(id): Path<String>,
    Json(payload): Json<ExecRequest>,
) -> Json<ApiResponse<ExecResponse>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            if payload.cmd.is_empty() {
                anyhow::bail!("Command cannot be empty");
            }

            let snapshot_env = metadata
                .snapshots
                .get(&sandbox.snapshot_id)
                .and_then(|snapshot| snapshot.env.clone())
                .unwrap_or_default();

            let output = crate::os::sys::exec_sandbox(sandbox, &payload.cmd, &snapshot_env)?;
            
            Ok(ExecResponse {
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                exit_code: output.status.code().unwrap_or(-1),
            })
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

fn extract_snapshot_config(
    dockerfile_path: &std::path::Path,
) -> anyhow::Result<(Option<Vec<String>>, Option<Vec<String>>, Option<Vec<String>>)> {
    let instructions = parse_dockerfile(dockerfile_path)?;
    let mut entrypoint = None;
    let mut cmd = None;
    let mut env = Vec::new();

    for instruction in instructions {
        match instruction {
            Instruction::Env { key, value } => env.push(format!("{}={}", key, value)),
            Instruction::Entrypoint(value) => entrypoint = Some(value),
            Instruction::Cmd(value) => cmd = Some(value),
            _ => {}
        }
    }

    let env = if env.is_empty() { None } else { Some(env) };
    Ok((entrypoint, cmd, env))
}

async fn handle_file_read(
    Path(id): Path<String>,
    Query(query): Query<FileReadQuery>,
) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            let merged_dir = sandbox.dir.join("merged");
            let target = resolve_sandbox_path(&merged_dir, &query.path)?;
            let content = std::fs::read_to_string(target)?;
            Ok(content)
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

async fn handle_file_write(
    Path(id): Path<String>,
    Json(payload): Json<FileWriteRequest>,
) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            let merged_dir = sandbox.dir.join("merged");
            let target = resolve_sandbox_path(&merged_dir, &payload.path)?;
            
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            
            std::fs::write(&target, payload.content)?;
            Ok(format!("File {} written successfully", payload.path))
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

async fn handle_file_delete(
    Path(id): Path<String>,
    Json(payload): Json<FileDeleteRequest>,
) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            let merged_dir = sandbox.dir.join("merged");
            let target = resolve_sandbox_path(&merged_dir, &payload.path)?;
            
            if target.is_dir() {
                std::fs::remove_dir_all(&target)?;
            } else {
                std::fs::remove_file(&target)?;
            }
            
            Ok(format!("File {} deleted successfully", payload.path))
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

/// Upload a binary file to the sandbox (base64-encoded).
async fn handle_file_upload(
    Path(id): Path<String>,
    Json(payload): Json<FileUploadRequest>,
) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        use base64::Engine;

        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            let merged_dir = sandbox.dir.join("merged");
            let target = resolve_sandbox_path(&merged_dir, &payload.path)?;

            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&payload.data)
                .map_err(|e| anyhow::anyhow!("Invalid base64 data: {}", e))?;

            std::fs::write(&target, &bytes)?;
            Ok(format!("File {} uploaded successfully ({} bytes)", payload.path, bytes.len()))
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

/// Download a binary file from the sandbox (returned as base64-encoded).
async fn handle_file_download(
    Path(id): Path<String>,
    Query(query): Query<FileDownloadQuery>,
) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        use base64::Engine;

        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            let merged_dir = sandbox.dir.join("merged");
            let target = resolve_sandbox_path(&merged_dir, &query.path)?;
            let bytes = std::fs::read(&target)?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            Ok(encoded)
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

async fn handle_suspend(
    Path(id): Path<String>,
) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            let merged_dir = sandbox.dir.join("merged");
            crate::os::sys::suspend_sandbox_os(sandbox, &merged_dir)?;
            Ok(format!("Suspended sandbox {}", id))
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}

async fn handle_resume(
    Path(id): Path<String>,
) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            let merged_dir = sandbox.dir.join("merged");
            crate::os::sys::resume_sandbox_os(sandbox, &merged_dir)?;
            Ok(format!("Resumed sandbox {}", id))
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    }).await.unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse { success: true, data: Some(data), error: None }),
        Err(e) => Json(ApiResponse { success: false, data: None, error: Some(e.to_string()) }),
    }
}