use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use std::path::PathBuf;
use tracing::{debug, error, info, warn};
use tracing_subscriber::{EnvFilter, FmtSubscriber};
use uuid::Uuid;

mod build;
mod metadata;
mod netns;
mod os;
mod overlay;
mod sandbox;
mod server;
mod snapshot;

use build::build;
use build::cache::{
    list_build_artifacts, prune_build_artifacts, BuildCachePruneMode, BuildCacheScope,
};
use metadata::{load_metadata, register_snapshot, save_metadata, SandboxMetadata};
use overlay::OverlayMount;
use sandbox::run_sandbox;
use snapshot::{create_archive, extract_archive, get_sandboxes_dir};

#[derive(Parser, Debug)]
#[command(name = "mini-daytona-rs")]
#[command(about = "极简容器运行时", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    #[arg(
        short,
        long,
        global = true,
        help = "设置日志级别 (e.g., debug, info, warn, error)"
    )]
    log_level: Option<String>,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// 从 Dockerfile 构建快照
    Build {
        dockerfile: PathBuf,
        #[arg(default_value = ".")]
        context: PathBuf,
    },
    /// 从快照启动一个容器沙箱
    Start { snapshot: PathBuf },
    /// 将当前沙箱打包为快照
    Snapshot { sandbox_id: String, output: PathBuf },
    /// 列出所有快照与沙箱
    List,
    /// 管理全局 build artifact 缓存
    Cache {
        #[command(subcommand)]
        command: CacheCommands,
    },
    /// 销毁指定的沙箱环境
    Destroy { sandbox_id: String },
    /// 启动 API Server (3000端口)
    Server,
}

#[derive(clap::Subcommand, Debug)]
enum CacheCommands {
    /// 列出 build artifact 缓存
    List,
    /// 清理 build artifact 缓存
    Prune {
        #[arg(long)]
        dockerfile_md5: Option<String>,
        #[arg(long)]
        cache_key: Option<String>,
    },
    /// 清空所有 build artifact 缓存
    Clear,
}

fn setup_logging(level: &Option<String>) {
    let mut filter = EnvFilter::from_default_env();
    if let Some(lvl) = level {
        filter = filter.add_directive(lvl.parse().unwrap_or_else(|_| "info".parse().unwrap()));
    } else if std::env::var("RUST_LOG").is_err() {
        filter = filter.add_directive("info".parse().unwrap());
    }

    let subscriber = FmtSubscriber::builder()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(false)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("Failed to initialize logging");
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    setup_logging(&cli.log_level);

    debug!("Starting mini-daytona-rs...");

    match &cli.command {
        Commands::Build {
            dockerfile,
            context,
        } => {
            info!("Building from Dockerfile: {:?}", dockerfile);
            let snapshot_path = build(dockerfile, context).context("Failed to build snapshot")?;
            info!("Snapshot created: {:?}", snapshot_path);

            let mut metadata = load_metadata().context("Failed to load metadata")?;
            register_snapshot(&mut metadata, snapshot_path, None, None, None);
            save_metadata(&metadata).context("Failed to save metadata")?;
        }
        Commands::Start { snapshot } => {
            let sandbox_id = Uuid::new_v4().to_string();
            info!("Initializing sandbox {}...", sandbox_id);
            let sandbox_dir = get_sandboxes_dir()?.join(&sandbox_id);
            std::fs::create_dir_all(&sandbox_dir).context("Failed to create sandbox directory")?;

            let base_dir = sandbox_dir.join("base");
            std::fs::create_dir_all(&base_dir).context("Failed to create base directory")?;
            extract_archive(snapshot, &base_dir).context("Failed to extract archive")?;

            let upper_dir = sandbox_dir.join("upper");
            let work_dir = sandbox_dir.join("work");
            let merged_dir = sandbox_dir.join("merged");

            debug!("Mounting overlay directories");
            let overlay =
                OverlayMount::new(vec![base_dir], upper_dir, work_dir, merged_dir.clone())
                    .context("Failed to prepare overlay mount")?;
            overlay.mount().context("Failed to mount overlay")?;

            let mut metadata = load_metadata()?;
            metadata.sandboxes.insert(
                sandbox_id.clone(),
                SandboxMetadata {
                    id: sandbox_id.clone(),
                    snapshot_id: "".to_string(),
                    created_at: Utc::now().to_rfc3339(),
                    dir: sandbox_dir.clone(),
                    pid: None,
                    ip: None,
                    resources: sandbox::ResourceLimits::default(),
                },
            );
            save_metadata(&metadata)?;

            info!("Starting sandbox execution: {}", sandbox_id);
            if let Err(e) = run_sandbox(
                &sandbox_id,
                merged_dir.to_str().unwrap(),
                &[],
                None,
                None,
                sandbox::SandboxProfile::Runtime,
            ) {
                error!("Sandbox {} failed: {}", sandbox_id, e);
            }

            info!("Cleaning up overlay mount");
            overlay.unmount().context("Failed to unmount overlay")?;
        }
        Commands::Snapshot { sandbox_id, output } => {
            let metadata = load_metadata()?;
            if let Some(sandbox) = metadata.sandboxes.get(sandbox_id) {
                let merged_dir = sandbox.dir.join("merged");
                info!(
                    "Creating snapshot from sandbox {} to {:?}",
                    sandbox_id, output
                );
                create_archive(&merged_dir, output).context("Failed to create archive")?;
                info!("Snapshot successfully saved to: {:?}", output);
            } else {
                warn!("Sandbox not found: {}", sandbox_id);
                anyhow::bail!("Sandbox {} not found", sandbox_id);
            }
        }
        Commands::List => {
            let metadata = load_metadata()?;
            info!("Snapshots:");
            for (id, snapshot) in &metadata.snapshots {
                println!("  {} - {:?}", id, snapshot.path);
            }
            info!("Sandboxes:");
            for (id, sandbox) in &metadata.sandboxes {
                println!("  {} - {:?}", id, sandbox.dir);
            }
        }
        Commands::Cache { command } => match command {
            CacheCommands::List => {
                for artifact in list_build_artifacts()? {
                    println!(
                        "{} dockerfile_md5={} context_hash={} path={:?} last_used_at={}",
                        artifact.cache_key,
                        artifact.dockerfile_md5,
                        artifact.context_hash,
                        artifact.snapshot_path,
                        artifact.last_used_at
                    );
                }
            }
            CacheCommands::Prune {
                dockerfile_md5,
                cache_key,
            } => {
                let mode = if let Some(cache_key) = cache_key.clone() {
                    BuildCachePruneMode::Scope(BuildCacheScope::CacheKey(cache_key))
                } else if let Some(dockerfile_md5) = dockerfile_md5.clone() {
                    BuildCachePruneMode::Scope(BuildCacheScope::DockerfileMd5(dockerfile_md5))
                } else {
                    BuildCachePruneMode::RemoveMissingOnly
                };

                let removed = prune_build_artifacts(mode)?;
                info!("Pruned {} build artifact cache entrie(s)", removed.len());
                for artifact in removed {
                    println!(
                        "removed {} -> {:?}",
                        artifact.cache_key, artifact.snapshot_path
                    );
                }
            }
            CacheCommands::Clear => {
                let removed = prune_build_artifacts(BuildCachePruneMode::ClearAll)?;
                info!("Cleared {} build artifact cache entrie(s)", removed.len());
            }
        },
        Commands::Destroy { sandbox_id } => {
            let mut metadata = load_metadata()?;
            if let Some(sandbox) = metadata.sandboxes.remove(sandbox_id) {
                debug!("Destroying sandbox: {}", sandbox_id);
                let merged_dir = sandbox.dir.join("merged");

                // Attempt to quietly unmount first just in case
                let overlay = OverlayMount::new(
                    vec![],
                    sandbox.dir.join("upper"),
                    sandbox.dir.join("work"),
                    merged_dir,
                )
                .ok();
                if let Some(mnt) = overlay {
                    let _ = mnt.unmount();
                }

                std::fs::remove_dir_all(&sandbox.dir)
                    .context("Failed to clean up sandbox directory")?;
                save_metadata(&metadata)?;
                info!("Successfully destroyed sandbox: {}", sandbox_id);
            } else {
                warn!("Sandbox not found: {}", sandbox_id);
                anyhow::bail!("Sandbox {} not found", sandbox_id);
            }
        }
        Commands::Server => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(server::run_server())?;
        }
    }

    Ok(())
}
