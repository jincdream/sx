#![allow(unused)]

pub mod cache;
pub mod parser;
pub mod registry;

use self::cache::{
    compute_context_hash, compute_dockerfile_md5, publish_build_artifact,
    resolve_cached_build_artifact,
};
use self::parser::{parse_dockerfile, Instruction};
use self::registry::{pull_image, ImageReference};
use crate::overlay::OverlayMount;
use crate::sandbox::{run_sandbox, SandboxProfile};
use crate::snapshot::{get_bases_dir, get_cache_dir};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::info;
use uuid::Uuid;

pub fn build(dockerfile_path: &Path, context_dir: &Path) -> Result<PathBuf> {
    if let Some(cached_artifact) = resolve_cached_build_artifact(dockerfile_path, context_dir)? {
        return Ok(cached_artifact.snapshot_path);
    }

    let dockerfile_md5 = compute_dockerfile_md5(dockerfile_path)?;
    let context_hash = compute_context_hash(context_dir)?;
    let instructions = parse_dockerfile(dockerfile_path)?;
    let mut lower_dirs: Vec<PathBuf> = Vec::new();
    let mut _workdir = "/".to_string();
    let mut env: Vec<String> = Vec::new();
    let mut _entrypoint = None;
    let mut _cmd = None;
    let mut _user = None;
    let mut _expose = None;

    let bases_dir = get_bases_dir()?;
    let cache_dir = get_cache_dir()?;

    for instruction in instructions.iter() {
        match instruction {
            Instruction::From(image) => {
                let image_ref = ImageReference::parse(image)?;

                // Use a deterministic cache key so repeated builds skip the pull
                let cache_key = {
                    let mut hasher = Sha256::new();
                    hasher.update(
                        format!(
                            "{}/{}/{}",
                            image_ref.registry, image_ref.repo, image_ref.tag
                        )
                        .as_bytes(),
                    );
                    format!("base-{:x}", hasher.finalize())
                };
                let layer_dir = bases_dir.join(&cache_key);

                if layer_dir.exists() {
                    info!("Using cached base image: {} ({})", image, cache_key);
                } else {
                    info!("Pulling base image: {} → {}", image, cache_key);
                    fs::create_dir_all(&layer_dir)?;
                    if let Err(e) = pull_image(&image_ref, &layer_dir) {
                        // Clean up partial download on failure
                        let _ = fs::remove_dir_all(&layer_dir);
                        return Err(e);
                    }
                }
                lower_dirs.push(layer_dir);
            }
            Instruction::Run(cmd_str) => {
                let cache_key = compute_cache_key(&lower_dirs, &format!("RUN {}", cmd_str));
                let cache_layer = cache_dir.join(&cache_key);

                if cache_layer.exists() {
                    info!("Using cache for RUN: {}", cmd_str);
                    lower_dirs.push(cache_layer);
                    continue;
                }

                let temp_dir = std::env::temp_dir().join(Uuid::new_v4().to_string());
                fs::create_dir_all(&temp_dir)?;

                let upper_dir = temp_dir.join("upper");
                let work_dir = temp_dir.join("work");
                let merged_dir = temp_dir.join("merged");

                let overlay = OverlayMount::new(
                    lower_dirs.clone(),
                    upper_dir.clone(),
                    work_dir,
                    merged_dir.clone(),
                )?;
                overlay.mount()?;

                crate::os::sys::build_instruction(cmd_str, &merged_dir, &_workdir, &env)?;

                overlay.unmount()?;

                // Remove resolv.conf injected by network setup — it's host-specific, not a build artifact
                let _ = fs::remove_file(upper_dir.join("etc/resolv.conf"));

                // Save to cache (skip if a concurrent build already populated it)
                if !cache_layer.exists()
                    && fs::rename(&upper_dir, &cache_layer).is_err()
                {
                    fs::create_dir_all(&cache_layer)?;
                    let content_options = fs_extra::dir::CopyOptions {
                        content_only: true,
                        overwrite: true,
                        ..Default::default()
                    };
                    fs_extra::dir::copy(&upper_dir, &cache_layer, &content_options)?;
                }
                lower_dirs.push(cache_layer);

                fs::remove_dir_all(temp_dir)?;
            }
            Instruction::Copy { src, dst } | Instruction::Add { src, dst } => {
                let cache_key = compute_cache_key(&lower_dirs, &format!("COPY {} {}", src, dst));
                let cache_layer = cache_dir.join(&cache_key);

                if cache_layer.exists() {
                    info!("Using cache for COPY: {} -> {}", src, dst);
                    lower_dirs.push(cache_layer);
                    continue;
                }

                let temp_dir = std::env::temp_dir().join(Uuid::new_v4().to_string());
                fs::create_dir_all(&temp_dir)?;

                let upper_dir = temp_dir.join("upper");
                let work_dir = temp_dir.join("work");
                let merged_dir = temp_dir.join("merged");

                let overlay = OverlayMount::new(
                    lower_dirs.clone(),
                    upper_dir.clone(),
                    work_dir,
                    merged_dir.clone(),
                )?;
                overlay.mount()?;

                let src_path = context_dir.join(src);
                let dst_path = merged_dir.join(dst.trim_start_matches('/'));

                if let Some(parent) = dst_path.parent() {
                    fs::create_dir_all(parent)?;
                }

                if src_path.is_dir() {
                    fs_extra::dir::copy(src_path, dst_path, &fs_extra::dir::CopyOptions::new())?;
                } else {
                    fs::copy(src_path, dst_path)?;
                }

                overlay.unmount()?;

                if !cache_layer.exists()
                    && fs::rename(&upper_dir, &cache_layer).is_err()
                {
                    fs::create_dir_all(&cache_layer)?;
                    let content_options = fs_extra::dir::CopyOptions {
                        content_only: true,
                        overwrite: true,
                        ..Default::default()
                    };
                    fs_extra::dir::copy(&upper_dir, &cache_layer, &content_options)?;
                }
                lower_dirs.push(cache_layer);

                fs::remove_dir_all(temp_dir)?;
            }
            Instruction::Workdir(dir) => {
                _workdir = dir.clone();
            }
            Instruction::Env { key, value } => {
                env.push(format!("{}={}", key, value));
            }
            Instruction::Entrypoint(vec) => {
                _entrypoint = Some(vec.clone());
            }
            Instruction::Cmd(vec) => {
                _cmd = Some(vec.clone());
            }
            Instruction::User(user) => {
                _user = Some(user.clone());
            }
            Instruction::Expose(expose) => {
                _expose = Some(expose.clone());
            }
        }
    }

    let final_temp = std::env::temp_dir().join(Uuid::new_v4().to_string());
    fs::create_dir_all(&final_temp)?;

    let upper_final = final_temp.join("upper");
    let work_final = final_temp.join("work");
    let merged_final = final_temp.join("merged");

    let overlay = OverlayMount::new(
        lower_dirs.clone(),
        upper_final.clone(),
        work_final,
        merged_final.clone(),
    )?;
    overlay.mount()?;

    let publish_result = publish_build_artifact(&dockerfile_md5, &context_hash, &merged_final);
    let unmount_result = overlay.unmount();
    let cleanup_result = fs::remove_dir_all(&final_temp);

    let artifact = publish_result?;
    unmount_result?;
    cleanup_result?;

    Ok(artifact.snapshot_path)
}

fn compute_cache_key(lower_dirs: &[PathBuf], instruction: &str) -> String {
    let mut hasher = Sha256::new();
    #[cfg(target_os = "macos")]
    {
        hasher.update(b"macos-overlay-v3");
        // Include host Python version so cache invalidates when Python changes.
        // Native C-extension .so files are version-specific; a stale cache with
        // cpython-312 .so files breaks when exec uses Python 3.13.
        if let Ok(out) = std::process::Command::new("python3")
            .arg("--version")
            .output()
        {
            hasher.update(&out.stdout);
        }
    }
    for dir in lower_dirs {
        hasher.update(dir.to_str().unwrap().as_bytes());
    }
    hasher.update(instruction.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)
}
