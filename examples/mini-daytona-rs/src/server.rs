use crate::build::build;
use crate::build::cache::{
    list_build_artifacts, prune_build_artifacts, BuildCachePruneMode, BuildCacheScope,
};
use crate::build::parser::{parse_dockerfile, Instruction};
use crate::metadata::{
    get_volumes_dir, load_metadata, register_snapshot, save_metadata, MountConfig, SandboxMetadata,
    VolumeMetadata,
};
use crate::overlay::OverlayMount;
use crate::sandbox::{run_sandbox, BindMount, ResourceLimits, SandboxProfile};
use crate::snapshot::{create_archive, get_sandboxes_dir, get_snapshots_dir, hardlink_copy};
use axum::response::sse::{Event, Sse};
use axum::response::{Html, IntoResponse};
use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    routing::{delete, get, post},
    Json, Router,
};
use chrono::Utc;
use futures_util::stream::Stream;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tracing::{error, info};
use uuid::Uuid;

const ADMIN_HTML: &str = include_str!("../web/admin.html");
const PROJECT_ROOT: &str = env!("CARGO_MANIFEST_DIR");

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
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
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
    /// Optional volume mounts
    #[serde(default)]
    mounts: Vec<MountConfig>,
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
    #[serde(default)]
    stream: bool,
}

// Internal type enum for axum response combining either JSON or SSE stream
pub enum ExecApiResult {
    Json(Json<ApiResponse<ExecResponse>>),
    #[allow(clippy::type_complexity)]
    Sse(Sse<std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>>),
}

impl IntoResponse for ExecApiResult {
    fn into_response(self) -> axum::response::Response {
        match self {
            ExecApiResult::Json(json) => json.into_response(),
            ExecApiResult::Sse(sse) => sse.into_response(),
        }
    }
}

#[derive(Serialize)]
pub struct ExecResponse {
    stdout: String,
    stderr: String,
    exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    signal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oom_killed: Option<bool>,
}

#[derive(Serialize)]
pub struct ServerInfoResponse {
    pub os: &'static str,
    pub degraded_mode: bool,
    pub supports_image_exec: bool,
}

#[derive(Serialize, Default, Clone)]
pub struct SandboxRuntimeStatsResponse {
    pub memory_current_bytes: Option<u64>,
    pub memory_peak_bytes: Option<u64>,
    pub process_resident_bytes: Option<u64>,
    pub cpu_usage_usec: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub pids_current: Option<u64>,
    pub memory_limit_bytes: Option<u64>,
    pub cpu_quota: Option<u64>,
    pub cpu_period: Option<u64>,
    pub pids_limit: Option<u64>,
    pub oom_kill_count: Option<u64>,
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

#[derive(Deserialize)]
pub struct VolumeCreateRequest {
    name: String,
}

#[derive(Serialize)]
pub struct VolumeResponse {
    id: String,
    name: String,
    path: String,
    created_at: String,
}

#[derive(Serialize)]
pub struct ImageDefinitionResponse {
    name: String,
    dockerfile_path: String,
    context_path: String,
    dockerfile_content: String,
}

#[derive(Deserialize)]
pub struct ImageDockerfileUpdateRequest {
    content: String,
}

#[derive(Serialize)]
pub struct SnapshotDeleteResponse {
    id: String,
    path: String,
}

#[derive(Deserialize)]
pub struct SandboxSnapshotCreateRequest {
    sandbox_id: String,
}

#[derive(Deserialize)]
pub struct E2eRunRequest {
    #[serde(default = "default_client_only")]
    client_only: bool,
    #[serde(default)]
    test: Option<String>,
}

#[derive(Serialize)]
pub struct E2eRunResponse {
    command: Vec<String>,
    exit_code: i32,
    stdout: String,
    stderr: String,
    duration_ms: u64,
}

fn default_client_only() -> bool {
    true
}

pub async fn run_server() -> anyhow::Result<()> {
    // Initialize the network bridge for sandbox isolation
    crate::netns::ensure_bridge()?;

    let app = Router::new()
        .route("/", get(handle_admin_page))
        .route("/admin", get(handle_admin_page))
        .route("/api/info", get(handle_info))
        .route("/api/images", get(handle_image_list))
        .route("/api/images/{name}/dockerfile", post(handle_image_dockerfile_update))
        .route("/api/images/{name}/build", post(handle_image_build))
        .route("/api/build", post(handle_build))
        .route("/api/build-cache", get(handle_build_cache_list))
        .route("/api/build-cache/prune", post(handle_build_cache_prune))
        .route("/api/start", post(handle_start))
        .route("/api/snapshot", post(handle_snapshot))
        .route("/api/snapshots/from-sandbox", post(handle_snapshot_create_from_sandbox))
        .route("/api/snapshots/{id}", delete(handle_snapshot_delete))
        .route("/api/list", get(handle_list))
        .route("/api/e2e", post(handle_run_e2e))
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
        .route("/api/volumes", post(handle_volume_create))
        .route("/api/volumes", get(handle_volume_list))
        .route("/api/volumes/{id}", delete(handle_volume_delete))
        .route("/api/sandbox/{id}/proxy/{port}", get(handle_proxy_root))
        .route("/api/sandbox/{id}/proxy/{port}/{*rest}", get(handle_proxy))
        .route("/api/sandbox/{id}/url/{port}", get(handle_sandbox_url))
        .with_state(Arc::new(AppState {}))
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024)); // 50 MiB body limit

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    info!("Starting API server on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

fn images_root_dir() -> PathBuf {
    FsPath::new(PROJECT_ROOT).join("images")
}

fn resolve_image_dir(name: &str) -> anyhow::Result<PathBuf> {
    if name.is_empty() || name.contains('/') || name.contains("..") {
        anyhow::bail!("Invalid image name: {}", name);
    }

    let path = images_root_dir().join(name);
    if !path.is_dir() {
        anyhow::bail!("Image {} not found", name);
    }

    if !path.join("Dockerfile").is_file() {
        anyhow::bail!("Image {} is missing Dockerfile", name);
    }

    Ok(path)
}

fn build_snapshot_from_paths(
    dockerfile_path: PathBuf,
    context_path: PathBuf,
    name: Option<String>,
    description: Option<String>,
) -> anyhow::Result<BuildResponse> {
    let snapshot_path = build(&dockerfile_path, &context_path)?;
    let (entrypoint, cmd, env) = extract_snapshot_config(&dockerfile_path)?;

    let mut metadata = load_metadata()?;
    let snapshot_id = register_snapshot(&mut metadata, snapshot_path.clone(), entrypoint, cmd, env, name, description);
    save_metadata(&metadata)?;

    Ok(BuildResponse {
        snapshot_path: snapshot_path.to_string_lossy().to_string(),
        snapshot_id,
    })
}

async fn handle_admin_page() -> Html<&'static str> {
    Html(ADMIN_HTML)
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
        anyhow::bail!("Resolved path escapes sandbox root (possible symlink attack)");
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
                anyhow::bail!("Symlink target escapes sandbox root");
            }
        }
    }

    Ok(canonical)
}

/// Resolve a file path for a sandbox, checking volume mounts first.
/// If the path falls under a volume mount, resolve to the volume's host directory.
/// Otherwise, fall back to the merged overlay directory.
fn resolve_file_path(
    sandbox: &SandboxMetadata,
    metadata: &crate::metadata::Metadata,
    user_path: &str,
) -> anyhow::Result<std::path::PathBuf> {
    for mount in &sandbox.mounts {
        let mount_path = mount.mount_path.trim_end_matches('/');
        let user_trimmed = user_path.trim_end_matches('/');
        if user_trimmed == mount_path || user_trimmed.starts_with(&format!("{}/", mount_path)) {
            let volume = metadata
                .volumes
                .get(&mount.volume_id)
                .ok_or_else(|| anyhow::anyhow!("Volume {} not found", mount.volume_id))?;
            let relative = user_path
                .strip_prefix(mount_path)
                .unwrap_or("")
                .trim_start_matches('/');
            if relative.split('/').any(|c| c == "..") {
                anyhow::bail!("Path contains '..' traversal component");
            }
            let target = if relative.is_empty() {
                volume.path.clone()
            } else {
                volume.path.join(relative)
            };
            return Ok(target);
        }
    }
    let merged_dir = sandbox.dir.join("merged");
    resolve_sandbox_path(&merged_dir, user_path)
}

/// Check if a path is under a read-only volume mount.
fn is_readonly_mount(sandbox: &SandboxMetadata, user_path: &str) -> bool {
    for mount in &sandbox.mounts {
        let mount_path = mount.mount_path.trim_end_matches('/');
        let user_trimmed = user_path.trim_end_matches('/');
        if user_trimmed == mount_path || user_trimmed.starts_with(&format!("{}/", mount_path)) {
            return mount.readonly;
        }
    }
    false
}

async fn handle_info() -> Json<ApiResponse<ServerInfoResponse>> {
    let data = crate::os::sys::get_server_info();

    Json(ApiResponse {
        success: true,
        data: Some(data),
        error: None,
    })
}

async fn handle_image_list() -> Json<ApiResponse<Vec<ImageDefinitionResponse>>> {
    let result = tokio::task::spawn_blocking(|| {
        let mut images = Vec::new();

        for entry in std::fs::read_dir(images_root_dir())? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }

            let name = entry.file_name().to_string_lossy().to_string();
            let dir = entry.path();
            let dockerfile_path = dir.join("Dockerfile");
            if !dockerfile_path.is_file() {
                continue;
            }

            images.push(ImageDefinitionResponse {
                name,
                dockerfile_path: dockerfile_path.to_string_lossy().to_string(),
                context_path: dir.to_string_lossy().to_string(),
                dockerfile_content: std::fs::read_to_string(dockerfile_path)?,
            });
        }

        images.sort_by(|left, right| left.name.cmp(&right.name));
        Ok::<_, anyhow::Error>(images)
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_image_dockerfile_update(
    Path(name): Path<String>,
    Json(payload): Json<ImageDockerfileUpdateRequest>,
) -> Json<ApiResponse<ImageDefinitionResponse>> {
    let result = tokio::task::spawn_blocking(move || {
        let dir = resolve_image_dir(&name)?;
        let dockerfile_path = dir.join("Dockerfile");
        std::fs::write(&dockerfile_path, payload.content.clone())?;

        Ok::<_, anyhow::Error>(ImageDefinitionResponse {
            name,
            dockerfile_path: dockerfile_path.to_string_lossy().to_string(),
            context_path: dir.to_string_lossy().to_string(),
            dockerfile_content: payload.content,
        })
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_image_build(Path(name): Path<String>) -> Json<ApiResponse<BuildResponse>> {
    let result = tokio::task::spawn_blocking(move || {
        let dir = resolve_image_dir(&name)?;
        let dockerfile_path = dir.join("Dockerfile");
        build_snapshot_from_paths(dockerfile_path, dir, Some(name), None)
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_build(
    State(_state): State<Arc<AppState>>,
    Json(payload): Json<BuildRequest>,
) -> Json<ApiResponse<BuildResponse>> {
    info!(
        "API Build requested: {} context: {}",
        payload.dockerfile, payload.context
    );

    let result: anyhow::Result<BuildResponse> = tokio::task::spawn_blocking(move || {
        build_snapshot_from_paths(PathBuf::from(payload.dockerfile), PathBuf::from(payload.context), payload.name, payload.description)
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
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
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
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
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_start(
    State(_state): State<Arc<AppState>>,
    Json(payload): Json<StartRequest>,
) -> Json<ApiResponse<StartResponse>> {
    info!("API Start requested from snapshot: {}", payload.snapshot);

    let result: anyhow::Result<StartResponse> = tokio::task::spawn_blocking(move || {
        let snapshot_path = PathBuf::from(payload.snapshot);
        let resource_limits = payload.resources.unwrap_or_default();
        let mount_configs = payload.mounts;
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

        // Resolve and validate volume mounts
        let bind_mounts: Vec<BindMount> = mount_configs
            .iter()
            .map(|mc| {
                let volume = metadata
                    .volumes
                    .get(&mc.volume_id)
                    .ok_or_else(|| anyhow::anyhow!("Volume {} not found", mc.volume_id))?;
                if !mc.mount_path.starts_with('/') {
                    anyhow::bail!("mount_path must be absolute: {}", mc.mount_path);
                }
                if mc.mount_path.contains("..") {
                    anyhow::bail!("mount_path must not contain '..': {}", mc.mount_path);
                }
                Ok(BindMount {
                    host_path: volume.path.clone(),
                    container_path: mc.mount_path.clone(),
                    readonly: mc.readonly,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        let upper_dir = sandbox_dir.join("upper");
        let work_dir = sandbox_dir.join("work");
        let merged_dir = sandbox_dir.join("merged");

        std::fs::create_dir_all(&upper_dir)?;

        let overlay =
            OverlayMount::new(vec![snapshot_path], upper_dir, work_dir, merged_dir.clone())?;
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
                resources: resource_limits.clone(),
                mounts: mount_configs,
            },
        );
        save_metadata(&metadata)?;

        let local_sandbox_id = sandbox_id.clone();

        // Spawn the blocking sandbox process in another thread so we can return the ID
        std::thread::spawn(move || {
            info!("Starting sandbox execution: {}", local_sandbox_id);
            // Use an infinite sleep so the primary container process doesn't exit immediately
            let sid = local_sandbox_id.clone();
            if let Err(e) = run_sandbox(
                &sid,
                merged_dir.to_str().unwrap(),
                &["tail", "-f", "/dev/null"],
                Some(&resource_limits),
                None,
                SandboxProfile::Runtime,
                &bind_mounts,
            ) {
                error!("Sandbox {} failed: {}", local_sandbox_id, e);
            }
            // We do not unmount here, we leave it to handle_destroy so the user can interact via API.
        });

        Ok(StartResponse { sandbox_id })
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

#[derive(Serialize)]
pub struct ListResponse {
    snapshots: Vec<crate::metadata::SnapshotMetadata>,
    sandboxes: Vec<crate::metadata::SandboxMetadata>,
    volumes: Vec<crate::metadata::VolumeMetadata>,
}

async fn handle_list() -> Json<ApiResponse<ListResponse>> {
    let result = tokio::task::spawn_blocking(|| {
        let metadata = load_metadata()?;
        Ok::<ListResponse, anyhow::Error>(ListResponse {
            snapshots: metadata.snapshots.into_values().collect(),
            sandboxes: metadata.sandboxes.into_values().collect(),
            volumes: metadata.volumes.into_values().collect(),
        })
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
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
            let mut writable_metadata = load_metadata()?;
            register_snapshot(&mut writable_metadata, output.clone(), None, None, None, None, None);
            save_metadata(&writable_metadata)?;
            Ok(payload.output)
        } else {
            anyhow::bail!("Sandbox {} not found", payload.sandbox_id);
        }
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_snapshot_create_from_sandbox(
    Json(payload): Json<SandboxSnapshotCreateRequest>,
) -> Json<ApiResponse<BuildResponse>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        let sandbox = metadata
            .sandboxes
            .get(&payload.sandbox_id)
            .ok_or_else(|| anyhow::anyhow!("Sandbox {} not found", payload.sandbox_id))?
            .clone();

        let snapshot_id = Uuid::new_v4().to_string();
        let snapshot_path = get_snapshots_dir()?.join(&snapshot_id);
        hardlink_copy(&sandbox.dir.join("merged"), &snapshot_path)?;

        let mut writable_metadata = load_metadata()?;
        writable_metadata.snapshots.insert(
            snapshot_id.clone(),
            crate::metadata::SnapshotMetadata {
                id: snapshot_id.clone(),
                path: snapshot_path.clone(),
                created_at: Utc::now().to_rfc3339(),
                entrypoint: None,
                cmd: None,
                env: None,
                name: None,
                description: None,
            },
        );
        save_metadata(&writable_metadata)?;

        Ok::<_, anyhow::Error>(BuildResponse {
            snapshot_path: snapshot_path.to_string_lossy().to_string(),
            snapshot_id,
        })
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_snapshot_delete(Path(id): Path<String>) -> Json<ApiResponse<SnapshotDeleteResponse>> {
    let result = tokio::task::spawn_blocking(move || {
        let mut metadata = load_metadata()?;

        if metadata.sandboxes.values().any(|sandbox| sandbox.snapshot_id == id) {
            anyhow::bail!("Snapshot {} is in use by at least one sandbox", id);
        }

        let snapshot = metadata
            .snapshots
            .remove(&id)
            .ok_or_else(|| anyhow::anyhow!("Snapshot {} not found", id))?;

        if snapshot.path.exists() {
            if snapshot.path.is_dir() {
                std::fs::remove_dir_all(&snapshot.path)?;
            } else {
                std::fs::remove_file(&snapshot.path)?;
            }
        }

        save_metadata(&metadata)?;

        Ok::<_, anyhow::Error>(SnapshotDeleteResponse {
            id,
            path: snapshot.path.to_string_lossy().to_string(),
        })
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_destroy(Path(id): Path<String>) -> Json<ApiResponse<String>> {
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
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

#[derive(Serialize)]
pub struct SandboxInfoResponse {
    pub id: String,
    pub ip: Option<String>,
    pub pid: Option<i32>,
    pub resources: ResourceLimits,
    pub stats: SandboxRuntimeStatsResponse,
}

async fn handle_sandbox_info(Path(id): Path<String>) -> Json<ApiResponse<SandboxInfoResponse>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            let stats = crate::os::sys::get_sandbox_metrics(sandbox);
            Ok(SandboxInfoResponse {
                id: sandbox.id.clone(),
                ip: sandbox.ip.clone(),
                pid: sandbox.pid,
                resources: sandbox.resources.clone(),
                stats,
            })
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_exec(Path(id): Path<String>, Json(payload): Json<ExecRequest>) -> ExecApiResult {
    if payload.cmd.is_empty() {
        return ExecApiResult::Json(Json(ApiResponse {
            success: false,
            data: None,
            error: Some("Command cannot be empty".to_string()),
        }));
    }

    let metadata_res = tokio::task::spawn_blocking(load_metadata)
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("tokio spawn blocking failed")));
    let metadata = match metadata_res {
        Ok(m) => m,
        Err(e) => {
            return ExecApiResult::Json(Json(ApiResponse {
                success: false,
                data: None,
                error: Some(e.to_string()),
            }))
        }
    };

    let sandbox = match metadata.sandboxes.get(&id) {
        Some(s) => s.clone(),
        None => {
            return ExecApiResult::Json(Json(ApiResponse {
                success: false,
                data: None,
                error: Some(format!("Sandbox {} not found", id)),
            }))
        }
    };

    let snapshot_env = metadata
        .snapshots
        .get(&sandbox.snapshot_id)
        .and_then(|snapshot| snapshot.env.clone())
        .unwrap_or_default();

    if payload.stream {
        // SSE Streaming execution
        match crate::os::sys::exec_sandbox_stream(&sandbox, &payload.cmd, &snapshot_env).await {
            Ok(mut child) => {
                let stdout = child.stdout.take().expect("stdout should be piped");
                let stderr = child.stderr.take().expect("stderr should be piped");

                use tokio::io::{AsyncReadExt, BufReader};

                let (tx, rx) = tokio::sync::mpsc::channel(100);

                let tx_out = tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stdout);
                    let mut buf = [0; 4096];
                    loop {
                        match reader.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                let content = String::from_utf8_lossy(&buf[..n]).to_string();
                                if tx_out
                                    .send(Ok(Event::default().event("stdout").data(content)))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });

                let tx_err = tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stderr);
                    let mut buf = [0; 4096];
                    loop {
                        match reader.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                let content = String::from_utf8_lossy(&buf[..n]).to_string();
                                if tx_err
                                    .send(Ok(Event::default().event("stderr").data(content)))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });

                let tx_exit = tx.clone();
                tokio::spawn(async move {
                    match child.wait().await {
                        Ok(status) => {
                            let code = status.code().unwrap_or(-1i32);
                            let _ = tx_exit
                                .send(Ok(Event::default().event("exit").data(code.to_string())))
                                .await;
                        }
                        Err(_) => {
                            let _ = tx_exit
                                .send(Ok(Event::default().event("exit").data("-1")))
                                .await;
                        }
                    }
                });

                let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
                ExecApiResult::Sse(Sse::new(Box::pin(stream)
                    as std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>))
            }
            Err(e) => {
                let err_msg = e.to_string();
                ExecApiResult::Json(Json(ApiResponse::<ExecResponse> {
                    success: false,
                    data: None,
                    error: Some(err_msg),
                }))
            }
        }
    } else {
        // Synchronous / JSON blocking execution
        let result = tokio::task::spawn_blocking(move || {
            let oom_before = crate::os::sys::read_oom_kill_count(&sandbox.id);
            let output = crate::os::sys::exec_sandbox(&sandbox, &payload.cmd, &snapshot_env)?;

            let exit_code = output.status.code();
            let signal = if exit_code.is_none() {
                // Process was killed by a signal
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    output.status.signal().map(signal_name)
                }
                #[cfg(not(unix))]
                { None }
            } else {
                None
            };

            let oom_killed = if signal.as_deref() == Some("SIGKILL") {
                let oom_after = crate::os::sys::read_oom_kill_count(&sandbox.id);
                match (oom_before, oom_after) {
                    (Some(before), Some(after)) => Some(after > before),
                    _ => None,
                }
            } else {
                None
            };

            let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if oom_killed == Some(true) {
                stderr.push_str("\n[mini-daytona] Process was OOM-killed: memory usage exceeded the sandbox limit.");
            }

            Ok(ExecResponse {
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr,
                exit_code: exit_code.unwrap_or(-1i32),
                signal,
                oom_killed,
            })
        })
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

        match result {
            Ok(data) => ExecApiResult::Json(Json(ApiResponse {
                success: true,
                data: Some(data),
                error: None,
            })),
            Err(e) => ExecApiResult::Json(Json(ApiResponse::<ExecResponse> {
                success: false,
                data: None,
                error: Some(e.to_string()),
            })),
        }
    }
}

fn signal_name(sig: i32) -> String {
    match sig {
        1 => "SIGHUP".to_string(),
        2 => "SIGINT".to_string(),
        6 => "SIGABRT".to_string(),
        9 => "SIGKILL".to_string(),
        11 => "SIGSEGV".to_string(),
        13 => "SIGPIPE".to_string(),
        14 => "SIGALRM".to_string(),
        15 => "SIGTERM".to_string(),
        _ => format!("SIG{}", sig),
    }
}

type SnapshotConfig = (Option<Vec<String>>, Option<Vec<String>>, Option<Vec<String>>);

fn extract_snapshot_config(
    dockerfile_path: &std::path::Path,
) -> anyhow::Result<SnapshotConfig> {
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
            let target = resolve_file_path(sandbox, &metadata, &query.path)?;
            let content = std::fs::read_to_string(target)?;
            Ok(content)
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_file_write(
    Path(id): Path<String>,
    Json(payload): Json<FileWriteRequest>,
) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            if is_readonly_mount(sandbox, &payload.path) {
                anyhow::bail!("Cannot write to read-only volume mount");
            }
            let target = resolve_file_path(sandbox, &metadata, &payload.path)?;

            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }

            std::fs::write(&target, payload.content)?;
            Ok(format!("File {} written successfully", payload.path))
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_file_delete(
    Path(id): Path<String>,
    Json(payload): Json<FileDeleteRequest>,
) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            if is_readonly_mount(sandbox, &payload.path) {
                anyhow::bail!("Cannot delete from read-only volume mount");
            }
            let target = resolve_file_path(sandbox, &metadata, &payload.path)?;

            if target.is_dir() {
                std::fs::remove_dir_all(&target)?;
            } else {
                std::fs::remove_file(&target)?;
            }

            Ok(format!("File {} deleted successfully", payload.path))
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
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
            if is_readonly_mount(sandbox, &payload.path) {
                anyhow::bail!("Cannot upload to read-only volume mount");
            }
            let target = resolve_file_path(sandbox, &metadata, &payload.path)?;

            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&payload.data)
                .map_err(|e| anyhow::anyhow!("Invalid base64 data: {}", e))?;

            std::fs::write(&target, &bytes)?;
            Ok(format!(
                "File {} uploaded successfully ({} bytes)",
                payload.path,
                bytes.len()
            ))
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
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
            let target = resolve_file_path(sandbox, &metadata, &query.path)?;
            let bytes = std::fs::read(&target)?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
            Ok(encoded)
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_suspend(Path(id): Path<String>) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            let merged_dir = sandbox.dir.join("merged");
            crate::os::sys::suspend_sandbox_os(sandbox, &merged_dir)?;
            Ok(format!("Suspended sandbox {}", id))
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_resume(Path(id): Path<String>) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            let merged_dir = sandbox.dir.join("merged");
            crate::os::sys::resume_sandbox_os(sandbox, &merged_dir)?;
            Ok(format!("Resumed sandbox {}", id))
        } else {
            anyhow::bail!("Sandbox {} not found", id);
        }
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_sandbox_url(
    headers: axum::http::HeaderMap,
    Path((id, port)): Path<(String, u16)>,
) -> Json<ApiResponse<serde_json::Value>> {
    let metadata_res = tokio::task::spawn_blocking(load_metadata).await;
    let metadata = match metadata_res {
        Ok(Ok(m)) => m,
        _ => {
            return Json(ApiResponse {
                success: false,
                data: None,
                error: Some("Failed to load metadata".into()),
            })
        }
    };
    if !metadata.sandboxes.contains_key(&id) {
        return Json(ApiResponse {
            success: false,
            data: None,
            error: Some(format!("Sandbox {} not found", id)),
        });
    }
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:3000");
    let url = format!("http://{}/api/sandbox/{}/proxy/{}", host, id, port);
    Json(ApiResponse {
        success: true,
        data: Some(serde_json::json!({ "url": url })),
        error: None,
    })
}

async fn handle_volume_create(
    Json(payload): Json<VolumeCreateRequest>,
) -> Json<ApiResponse<VolumeResponse>> {
    let result = tokio::task::spawn_blocking(move || {
        let volume_id = Uuid::new_v4().to_string();
        let volume_dir = get_volumes_dir()?.join(&volume_id);
        std::fs::create_dir_all(&volume_dir)?;

        let now = Utc::now().to_rfc3339();
        let volume = VolumeMetadata {
            id: volume_id.clone(),
            name: payload.name.clone(),
            path: volume_dir.clone(),
            created_at: now.clone(),
        };

        let mut metadata = load_metadata()?;
        metadata.volumes.insert(volume_id.clone(), volume);
        save_metadata(&metadata)?;

        Ok(VolumeResponse {
            id: volume_id,
            name: payload.name,
            path: volume_dir.to_string_lossy().to_string(),
            created_at: now,
        })
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_volume_list() -> Json<ApiResponse<Vec<VolumeResponse>>> {
    let result = tokio::task::spawn_blocking(|| {
        let metadata = load_metadata()?;
        let volumes: Vec<VolumeResponse> = metadata
            .volumes
            .into_values()
            .map(|v| VolumeResponse {
                id: v.id,
                name: v.name,
                path: v.path.to_string_lossy().to_string(),
                created_at: v.created_at,
            })
            .collect();
        Ok::<_, anyhow::Error>(volumes)
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_volume_delete(Path(id): Path<String>) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        let mut metadata = load_metadata()?;

        // Refuse deletion if any running sandbox is using this volume
        for sandbox in metadata.sandboxes.values() {
            if sandbox.mounts.iter().any(|m| m.volume_id == id) {
                anyhow::bail!(
                    "Volume {} is in use by sandbox {}",
                    id,
                    sandbox.id
                );
            }
        }

        if let Some(volume) = metadata.volumes.remove(&id) {
            if volume.path.exists() {
                std::fs::remove_dir_all(&volume.path)?;
            }
            save_metadata(&metadata)?;
            Ok(format!("Deleted volume {}", id))
        } else {
            anyhow::bail!("Volume {} not found", id);
        }
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_run_e2e(Json(payload): Json<E2eRunRequest>) -> Json<ApiResponse<E2eRunResponse>> {
    let result = tokio::task::spawn_blocking(move || {
        let mut args = vec!["test/run_e2e.js".to_string()];
        if payload.client_only {
            args.push("--client".to_string());
        }
        if let Some(test) = payload.test.filter(|value| !value.trim().is_empty()) {
            args.push(format!("--test={}", test.trim()));
        }

        let command_preview = std::iter::once("node".to_string())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>();

        let started_at = std::time::Instant::now();
        let output = Command::new("node")
            .args(&args)
            .current_dir(PROJECT_ROOT)
            .output()?;

        Ok::<_, anyhow::Error>(E2eRunResponse {
            command: command_preview,
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            duration_ms: started_at.elapsed().as_millis() as u64,
        })
    })
    .await
    .unwrap_or_else(|e| Err(anyhow::anyhow!("Task panic: {}", e)));

    match result {
        Ok(data) => Json(ApiResponse {
            success: true,
            data: Some(data),
            error: None,
        }),
        Err(e) => Json(ApiResponse {
            success: false,
            data: None,
            error: Some(e.to_string()),
        }),
    }
}

async fn handle_proxy_root(Path((id, port)): Path<(String, u16)>) -> axum::response::Response {
    proxy_to_sandbox(&id, port, "").await
}

async fn handle_proxy(
    Path((id, port, rest)): Path<(String, u16, String)>,
) -> axum::response::Response {
    let path = rest.trim_start_matches('/');
    proxy_to_sandbox(&id, port, path).await
}

async fn proxy_to_sandbox(id: &str, port: u16, path: &str) -> axum::response::Response {
    // Load metadata to get the sandbox IP
    let metadata = match tokio::task::spawn_blocking(load_metadata).await {
        Ok(Ok(m)) => m,
        _ => {
            return axum::response::Response::builder()
                .status(500)
                .body(axum::body::Body::from("Failed to load metadata"))
                .unwrap();
        }
    };

    let sandbox = match metadata.sandboxes.get(id) {
        Some(s) => s.clone(),
        None => {
            return axum::response::Response::builder()
                .status(404)
                .body(axum::body::Body::from(format!("Sandbox {} not found", id)))
                .unwrap();
        }
    };

    let target_host = match &sandbox.ip {
        Some(ip) => ip.clone(),
        None => "127.0.0.1".to_string(),
    };

    let target_url = if path.is_empty() {
        format!("http://{}:{}/", target_host, port)
    } else {
        format!("http://{}:{}/{}", target_host, port, path)
    };
    info!("Proxy: forwarding to {}", target_url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap();

    match client.get(&target_url).send().await {
        Ok(resp) => {
            let status = axum::http::StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(axum::http::StatusCode::BAD_GATEWAY);
            let mut builder = axum::response::Response::builder().status(status);

            // Forward content-type header
            if let Some(ct) = resp.headers().get("content-type") {
                builder = builder.header("content-type", ct.as_bytes());
            }

            let body_bytes = resp.bytes().await.unwrap_or_default();
            builder.body(axum::body::Body::from(body_bytes)).unwrap()
        }
        Err(e) => axum::response::Response::builder()
            .status(502)
            .body(axum::body::Body::from(format!("Proxy error: {}", e)))
            .unwrap(),
    }
}
