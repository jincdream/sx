use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// Mirror list, tried in order. Override with env DOCKER_MIRROR.
const DEFAULT_MIRRORS: &[&str] = &["docker.m.daocloud.io", "docker.1ms.run"];

/// Max retry attempts per request (for transient 404 / 5xx / network errors).
const MAX_REQUEST_ATTEMPTS: u32 = 3;

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
        // Determine the repo path (always use library/ prefix for short names)
        let (registry, repo_tag) = if s.contains('/') {
            let parts: Vec<&str> = s.splitn(2, '/').collect();
            if parts[0].contains('.') || parts[0].contains(':') {
                (parts[0].to_string(), parts[1].to_string())
            } else {
                (Self::get_mirror(), format!("library/{}", s))
            }
        } else {
            (Self::get_mirror(), format!("library/{}", s))
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

    /// Get the primary mirror registry from DOCKER_MIRROR env var, or use first default.
    fn get_mirror() -> String {
        std::env::var("DOCKER_MIRROR").unwrap_or_else(|_| DEFAULT_MIRRORS[0].to_string())
    }

    /// Get all mirrors to try in order. If DOCKER_MIRROR is set, only that one is used.
    pub fn get_mirrors() -> Vec<String> {
        if let Ok(m) = std::env::var("DOCKER_MIRROR") {
            vec![m]
        } else {
            DEFAULT_MIRRORS.iter().map(|s| s.to_string()).collect()
        }
    }
}

/// Parse Www-Authenticate header to extract realm, service, scope for anonymous token exchange.
/// Example: `Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/nginx:pull"`
fn parse_www_authenticate(header_val: &str) -> Option<(String, String, String)> {
    let header_val = header_val.strip_prefix("Bearer ")?;

    let mut realm = String::new();
    let mut service = String::new();
    let mut scope = String::new();

    for part in header_val.split(',') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix("realm=") {
            realm = val.trim_matches('"').to_string();
        } else if let Some(val) = part.strip_prefix("service=") {
            service = val.trim_matches('"').to_string();
        } else if let Some(val) = part.strip_prefix("scope=") {
            scope = val.trim_matches('"').to_string();
        }
    }

    if realm.is_empty() {
        return None;
    }
    Some((realm, service, scope))
}

/// Fetch an anonymous token from the registry's auth endpoint.
fn fetch_anonymous_token(
    client: &Client,
    realm: &str,
    service: &str,
    scope: &str,
) -> anyhow::Result<String> {
    let mut url = format!("{}?", realm);
    if !service.is_empty() {
        url.push_str(&format!("service={}&", service));
    }
    if !scope.is_empty() {
        url.push_str(&format!("scope={}", scope));
    }
    debug!("Fetching anonymous token from: {}", url);
    let resp: TokenResponse = client.get(&url).send()?.json()?;
    Ok(resp.token)
}

/// Make a registry request, handling 401 → anonymous token exchange and
/// 3xx redirects (stripping Authorization to avoid CDN rejections).
fn registry_request(
    client: &Client,
    url: &str,
    accept: &str,
    token: &mut Option<String>,
) -> anyhow::Result<Response> {
    let resp = send_with_auth(client, url, accept, token)?;

    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        // Try to get a token from the Www-Authenticate header
        if let Some(www_auth) = resp
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
        {
            debug!("Got 401, Www-Authenticate: {}", www_auth);
            if let Some((realm, service, scope)) = parse_www_authenticate(www_auth) {
                let new_token = fetch_anonymous_token(client, &realm, &service, &scope)?;
                *token = Some(new_token);

                // Retry with token
                let retry_resp = send_with_auth(client, url, accept, token)?;
                return follow_redirects(client, retry_resp);
            }
        }
        anyhow::bail!("Registry returned 401 Unauthorized for {}", url);
    }

    follow_redirects(client, resp)
}

/// Send a single GET request with optional Bearer token.
fn send_with_auth(
    client: &Client,
    url: &str,
    accept: &str,
    token: &Option<String>,
) -> anyhow::Result<Response> {
    let mut req = client.get(url).header("Accept", accept);
    if let Some(t) = token.as_ref() {
        req = req.header("Authorization", format!("Bearer {}", t));
    }
    Ok(req.send()?)
}

/// Follow 3xx redirects manually, stripping the Authorization header so that
/// CDN servers (e.g. CloudFlare) don't reject the request.
fn follow_redirects(client: &Client, resp: Response) -> anyhow::Result<Response> {
    if !resp.status().is_redirection() {
        return Ok(resp);
    }
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| anyhow::anyhow!("3xx redirect without Location header"))?
        .to_string();
    debug!("Following redirect → {}", location);
    // Follow redirect WITHOUT Authorization header
    let redirected = client.get(&location).send()?;
    Ok(redirected)
}

/// Retry-aware registry request wrapper.
/// Retries on transient errors: network failures, HTTP 404 (mirror CDN glitch), 5xx.
fn registry_request_retry(
    client: &Client,
    url: &str,
    accept: &str,
    token: &mut Option<String>,
) -> anyhow::Result<Response> {
    let mut last_err = None;
    for attempt in 0..MAX_REQUEST_ATTEMPTS {
        if attempt > 0 {
            let delay_secs = 2u64.pow(attempt);
            warn!(
                "  Retry {}/{} in {}s...",
                attempt,
                MAX_REQUEST_ATTEMPTS - 1,
                delay_secs
            );
            std::thread::sleep(std::time::Duration::from_secs(delay_secs));
        }
        match registry_request(client, url, accept, token) {
            Ok(resp) => {
                if resp.status().is_success() {
                    return Ok(resp);
                }
                let status = resp.status();
                // Retryable: 404 (mirror CDN transient), 5xx
                if status.as_u16() == 404 || status.is_server_error() {
                    warn!("  HTTP {} for {} — retryable", status, url);
                    last_err = Some(anyhow::anyhow!("HTTP {} for {}", status, url));
                    continue;
                }
                // Non-retryable status (e.g. 400, 403) — return as-is for caller
                return Ok(resp);
            }
            Err(e) => {
                warn!("  Request error: {} — retryable", e);
                last_err = Some(e);
                continue;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        anyhow::anyhow!(
            "All {} attempts exhausted for {}",
            MAX_REQUEST_ATTEMPTS,
            url
        )
    }))
}

/// Pull image with automatic mirror fallback.
/// If the image uses a default mirror, tries all mirrors in order.
pub fn pull_image(image_ref: &ImageReference, dest_dir: &PathBuf) -> anyhow::Result<()> {
    let mirrors = ImageReference::get_mirrors();
    let is_default_mirror = mirrors.iter().any(|m| m == &image_ref.registry);

    if is_default_mirror && mirrors.len() > 1 {
        let mut last_err = None;
        for (idx, mirror) in mirrors.iter().enumerate() {
            info!("Trying mirror [{}/{}]: {}", idx + 1, mirrors.len(), mirror);
            match pull_image_from(mirror, &image_ref.repo, &image_ref.tag, dest_dir) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    warn!("Mirror {} failed: {}", mirror, e);
                    last_err = Some(e);
                    // Clean up partial downloads before trying next mirror
                    let _ = fs::remove_dir_all(dest_dir);
                    let _ = fs::create_dir_all(dest_dir);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            anyhow::anyhow!(
                "All mirrors failed for {}/{}",
                image_ref.repo,
                image_ref.tag
            )
        }))
    } else {
        pull_image_from(
            &image_ref.registry,
            &image_ref.repo,
            &image_ref.tag,
            dest_dir,
        )
    }
}

/// Internal: pull image from a specific registry.
fn pull_image_from(
    registry: &str,
    repo: &str,
    tag: &str,
    dest_dir: &PathBuf,
) -> anyhow::Result<()> {
    fs::create_dir_all(dest_dir)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    info!("Pulling image: {}/v2/{}/manifests/{}", registry, repo, tag);

    let mut token: Option<String> = None;

    // Fetch manifest (handles 401 + anonymous token automatically)
    let manifest_url = format!("https://{}/v2/{}/manifests/{}", registry, repo, tag);
    let manifest_accept = "application/vnd.docker.distribution.manifest.v2+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.oci.image.index.v1+json";
    let manifest_resp =
        registry_request_retry(&client, &manifest_url, manifest_accept, &mut token)?;

    if !manifest_resp.status().is_success() {
        anyhow::bail!(
            "Failed to fetch manifest: HTTP {} from {}",
            manifest_resp.status(),
            manifest_url
        );
    }

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

        info!(
            "Resolved platform manifest: {} ({})",
            hit_digest, target_arch
        );

        let resolved_url = format!("https://{}/v2/{}/manifests/{}", registry, repo, hit_digest);
        let resolved_accept = "application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.manifest.v1+json";
        let resolved_resp =
            registry_request_retry(&client, &resolved_url, resolved_accept, &mut token)?;

        if !resolved_resp.status().is_success() {
            anyhow::bail!(
                "Failed to fetch resolved manifest: HTTP {}",
                resolved_resp.status()
            );
        }
        manifest = resolved_resp.json()?;
    }

    info!("Downloading {} layer(s)...", manifest.layers.len());

    // Download and extract layers
    for (i, layer) in manifest.layers.iter().enumerate() {
        let blob_url = format!("https://{}/v2/{}/blobs/{}", registry, repo, layer.digest);
        info!(
            "  [{}/{}] Downloading {}",
            i + 1,
            manifest.layers.len(),
            &layer.digest[..19.min(layer.digest.len())]
        );

        let mut resp = registry_request_retry(&client, &blob_url, "*/*", &mut token)?;

        if !resp.status().is_success() {
            anyhow::bail!(
                "Failed to download layer {}: HTTP {}",
                layer.digest,
                resp.status()
            );
        }

        let layer_path = dest_dir.join(format!("layer-{}.tar.gz", &layer.digest[7..15]));
        let mut file = File::create(&layer_path)?;
        std::io::copy(&mut resp, &mut file)?;

        crate::snapshot::extract_archive(&layer_path, dest_dir)?;
        fs::remove_file(layer_path)?;
    }

    info!("Image pull complete.");
    Ok(())
}
