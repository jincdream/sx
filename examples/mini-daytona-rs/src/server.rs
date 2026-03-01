use axum::{
    extract::{Path, Query, State},
    routing::{get, post, delete},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing::{info, error};
use uuid::Uuid;
use chrono::Utc;
use std::sync::Arc;

use crate::build::build;
use crate::metadata::{load_metadata, save_metadata, SandboxMetadata, SnapshotMetadata};
use crate::overlay::OverlayMount;
use crate::sandbox::run_sandbox;
use crate::snapshot::{create_archive, extract_archive, get_sandboxes_dir};

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
pub struct FileReadQuery {
    path: String,
}

pub async fn run_server() -> anyhow::Result<()> {
    let app = Router::new()
        .route("/api/build", post(handle_build))
        .route("/api/start", post(handle_start))
        .route("/api/snapshot", post(handle_snapshot))
        .route("/api/list", get(handle_list))
        .route("/api/sandbox/{id}", delete(handle_destroy))
        .route("/api/sandbox/{id}/exec", post(handle_exec))
        .route("/api/sandbox/{id}/file", get(handle_file_read))
        .route("/api/sandbox/{id}/file", post(handle_file_write))
        .route("/api/sandbox/{id}/file", delete(handle_file_delete))
        .with_state(Arc::new(AppState {}));

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    info!("Starting API server on {}", addr);
    
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    
    Ok(())
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
        
        let mut metadata = load_metadata()?;
        let snapshot_id = Uuid::new_v4().to_string();
        metadata.snapshots.insert(
            snapshot_id.clone(),
            SnapshotMetadata {
                id: snapshot_id.clone(),
                path: snapshot_path.clone(),
                created_at: Utc::now().to_rfc3339(),
                entrypoint: None,
                cmd: None,
                env: None,
            },
        );
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

async fn handle_start(
    State(_state): State<Arc<AppState>>,
    Json(payload): Json<StartRequest>,
) -> Json<ApiResponse<StartResponse>> {
    info!("API Start requested from snapshot: {}", payload.snapshot);
    
    let result: anyhow::Result<StartResponse> = tokio::task::spawn_blocking(move || {
        let snapshot_path = PathBuf::from(payload.snapshot);
        let sandbox_id = Uuid::new_v4().to_string();
        let sandbox_dir = get_sandboxes_dir()?.join(&sandbox_id);
        
        std::fs::create_dir_all(&sandbox_dir)?;
        let base_dir = sandbox_dir.join("base");
        std::fs::create_dir_all(&base_dir)?;
        
        extract_archive(&snapshot_path, &base_dir)?;
        
        let upper_dir = sandbox_dir.join("upper");
        let work_dir = sandbox_dir.join("work");
        let merged_dir = sandbox_dir.join("merged");
        
        let overlay = OverlayMount::new(
            vec![base_dir],
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
                snapshot_id: "".to_string(),
                created_at: Utc::now().to_rfc3339(),
                dir: sandbox_dir.clone(),
            },
        );
        save_metadata(&metadata)?;
        
        let local_sandbox_id = sandbox_id.clone();
        
        // Spawn the blocking sandbox process in another thread so we can return the ID
        std::thread::spawn(move || {
            info!("Starting sandbox execution: {}", local_sandbox_id);
            // Use an infinite sleep so the primary container process doesn't exit immediately
            if let Err(e) = run_sandbox(merged_dir.to_str().unwrap(), &["tail", "-f", "/dev/null"]) {
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
            let overlay = OverlayMount::new(
                vec![],
                sandbox.dir.join("upper"),
                sandbox.dir.join("work"),
                merged_dir,
            ).ok();
            if let Some(mnt) = overlay {
                let _ = mnt.unmount();
            }
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

async fn handle_exec(
    Path(id): Path<String>,
    Json(payload): Json<ExecRequest>,
) -> Json<ApiResponse<ExecResponse>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            let merged_dir = sandbox.dir.join("merged");
            if payload.cmd.is_empty() {
                anyhow::bail!("Command cannot be empty");
            }
            let output = std::process::Command::new("chroot")
                .arg(&merged_dir)
                .args(&payload.cmd)
                .output()?;
            
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

async fn handle_file_read(
    Path(id): Path<String>,
    Query(query): Query<FileReadQuery>,
) -> Json<ApiResponse<String>> {
    let result = tokio::task::spawn_blocking(move || {
        let metadata = load_metadata()?;
        if let Some(sandbox) = metadata.sandboxes.get(&id) {
            let merged_dir = sandbox.dir.join("merged");
            let safe_path = query.path.trim_start_matches('/');
            let target = merged_dir.join(safe_path);
            
            // Basic security check to prevent directory traversal
            if !target.starts_with(&merged_dir) {
                anyhow::bail!("Invalid path");
            }
            
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
            let safe_path = payload.path.trim_start_matches('/');
            let target = merged_dir.join(safe_path);
            
            if !target.starts_with(&merged_dir) {
                anyhow::bail!("Invalid path");
            }
            
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
            let safe_path = payload.path.trim_start_matches('/');
            let target = merged_dir.join(safe_path);
            
            if !target.starts_with(&merged_dir) {
                anyhow::bail!("Invalid path");
            }
            
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
