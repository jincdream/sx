use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
struct TokenResponse {
    token: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ManifestLayer {
    digest: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Platform {
    architecture: String,
    os: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ManifestDescriptor {
    digest: String,
    platform: Option<Platform>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    #[serde(default)]
    layers: Vec<ManifestLayer>,
    #[serde(default)]
    manifests: Vec<ManifestDescriptor>,
}

pub struct ImageReference {
    pub registry: String,
    pub repo: String,
    pub tag: String,
}

impl ImageReference {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let (registry, repo_tag) = if s.contains('/') {
            let parts: Vec<&str> = s.splitn(2, '/').collect();
            if parts[0].contains('.') || parts[0].contains(':') {
                (parts[0].to_string(), parts[1].to_string())
            } else {
                (
                    "registry-1.docker.io".to_string(),
                    format!("library/{}", s),
                )
            }
        } else {
            (
                "registry-1.docker.io".to_string(),
                format!("library/{}", s),
            )
        };

        let (repo, tag) = if repo_tag.contains(':') {
            let parts: Vec<&str> = repo_tag.splitn(2, ':').collect();
            (parts[0].to_string(), parts[1].to_string())
        } else {
            (repo_tag, "latest".to_string())
        };

        Ok(Self {
            registry,
            repo,
            tag,
        })
    }
}

pub fn pull_image(image_ref: &ImageReference, dest_dir: &PathBuf) -> anyhow::Result<()> {
    fs::create_dir_all(dest_dir)?;
    let client = Client::new();

    let auth_url = format!(
        "https://auth.docker.io/token?service=registry.docker.io&scope=repository:{}:pull",
        image_ref.repo
    );
    let token_resp: TokenResponse = client.get(&auth_url).send()?.json()?;
    let token = token_resp.token;

    let manifest_url = format!(
        "https://{}/v2/{}/manifests/{}",
        image_ref.registry, image_ref.repo, image_ref.tag
    );
    let mut manifest_resp = client
        .get(&manifest_url)
        .header(
            "Accept",
            "application/vnd.docker.distribution.manifest.v2+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.oci.image.index.v1+json",
        )
        .header("Authorization", format!("Bearer {}", token))
        .send()?;

    let mut manifest: Manifest = manifest_resp.json()?;

    // If it's a manifest list, resolve the correct architecture
    if !manifest.manifests.is_empty() {
        let arch = std::env::consts::ARCH;
        let target_arch = match arch {
            "aarch64" => "arm64",
            "x86_64" => "amd64",
            _ => arch,
        };

        let mut hit_digest = manifest.manifests[0].digest.clone();
        for m in &manifest.manifests {
            if let Some(plat) = &m.platform {
                if plat.architecture == target_arch && plat.os == "linux" {
                    hit_digest = m.digest.clone();
                    break;
                }
            }
        }

        let resolved_url = format!(
            "https://{}/v2/{}/manifests/{}",
            image_ref.registry, image_ref.repo, hit_digest
        );
        manifest_resp = client
            .get(&resolved_url)
            .header(
                "Accept",
                "application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.manifest.v1+json",
            )
            .header("Authorization", format!("Bearer {}", token))
            .send()?;
        manifest = manifest_resp.json()?;
    }

    // Now we must have layers
    for layer in manifest.layers {
        let blob_url = format!(
            "https://{}/v2/{}/blobs/{}",
            image_ref.registry, image_ref.repo, layer.digest
        );
        let mut resp = client
            .get(&blob_url)
            .header("Authorization", format!("Bearer {}", token))
            .send()?;

        let layer_path = dest_dir.join(format!("layer-{}.tar.gz", &layer.digest[7..15]));
        let mut file = File::create(&layer_path)?;
        std::io::copy(&mut resp, &mut file)?;

        crate::snapshot::extract_archive(&layer_path, dest_dir)?;
        fs::remove_file(layer_path)?;
    }

    Ok(())
}
