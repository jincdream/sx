pub mod parser;
pub mod registry;

use self::parser::{parse_dockerfile, Instruction};
use self::registry::{pull_image, ImageReference};
use crate::overlay::OverlayMount;
use crate::sandbox::run_sandbox;
use crate::snapshot::{create_archive, get_bases_dir, get_cache_dir, get_snapshots_dir};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;
use anyhow::Result;
use tracing::info;

pub fn build(
    dockerfile_path: &Path,
    context_dir: &Path,
) -> Result<PathBuf> {
    let instructions = parse_dockerfile(dockerfile_path)?;
    let mut lower_dirs: Vec<PathBuf> = Vec::new();
    let mut _workdir = "/".to_string();
    let mut env = Vec::new();
    let mut _entrypoint = None;
    let mut _cmd = None;

    let bases_dir = get_bases_dir()?;
    let cache_dir = get_cache_dir()?;

    for instruction in instructions.iter() {
        match instruction {
            Instruction::From(image) => {
                let image_ref = ImageReference::parse(image)?;
                let layer_dir = bases_dir.join(Uuid::new_v4().to_string());
                fs::create_dir_all(&layer_dir)?;
                pull_image(&image_ref, &layer_dir)?;
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

                run_sandbox(merged_dir.to_str().unwrap(), &["/bin/sh", "-c", cmd_str])?;

                overlay.unmount()?;

                if fs::rename(&upper_dir, &cache_layer).is_err() {
                    fs::create_dir_all(&cache_layer)?;
                    let content_options = fs_extra::dir::CopyOptions {
                        content_only: true,
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

                if fs::rename(&upper_dir, &cache_layer).is_err() {
                    fs::create_dir_all(&cache_layer)?;
                    let content_options = fs_extra::dir::CopyOptions {
                        content_only: true,
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
        }
    }

    let snapshot_id = Uuid::new_v4().to_string();
    let snapshot_path = get_snapshots_dir()?.join(format!("{}.tar.gz", snapshot_id));

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

    create_archive(&merged_final, &snapshot_path)?;

    overlay.unmount()?;
    fs::remove_dir_all(final_temp)?;

    Ok(snapshot_path)
}

fn compute_cache_key(lower_dirs: &[PathBuf], instruction: &str) -> String {
    let mut hasher = Sha256::new();
    for dir in lower_dirs {
        hasher.update(dir.to_str().unwrap().as_bytes());
    }
    hasher.update(instruction.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)
}
