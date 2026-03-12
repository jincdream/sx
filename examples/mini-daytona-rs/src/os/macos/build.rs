use std::path::Path;
use tracing::info;

pub fn get_cache_key_ext() -> Option<String> {
    // Include host Python version so the global artifact cache invalidates
    // when Python is upgraded — native C-extension .so files are version-specific.
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .ok()
}

pub fn build_instruction(
    cmd_str: &str,
    merged_dir: &Path,
    workdir: &str,
    env: &[String],
) -> anyhow::Result<()> {
    // Skip apt-get / dpkg commands — they require Debian/Ubuntu and cannot
    // work on macOS.  Packages that need native deps (e.g. Chromium)
    // should use platform-native installers (npm bundled downloads, brew, etc.).
    if cmd_str.contains("apt-get") || cmd_str.contains("dpkg") || cmd_str.contains("apt ") {
        info!(
            "[macOS] Skipping Linux-only package manager command: {}",
            cmd_str
        );
        return Ok(());
    }

    // Due to macOS's lack of true namespaces/chroot without root,
    // absolute paths in RUN commands (like `echo > /usr/...`) would otherwise target the host.
    // Rewrite common root paths into merged_dir and run directly in development mode.
    let merged_dir_str = merged_dir.to_str().unwrap();
    let safe_cmd = cmd_str
        .replace(" /usr/", &format!(" {}/usr/", merged_dir_str))
        .replace(" /etc/", &format!(" {}/etc/", merged_dir_str))
        .replace(" /var/", &format!(" {}/var/", merged_dir_str))
        .replace(" /opt/", &format!(" {}/opt/", merged_dir_str))
        .replace(" /home/", &format!(" {}/home/", merged_dir_str))
        .replace(" /bin/", &format!(" {}/bin/", merged_dir_str));

    let actual_workdir = if workdir == "/" {
        merged_dir.to_path_buf()
    } else {
        merged_dir.join(workdir.trim_start_matches('/'))
    };
    std::fs::create_dir_all(&actual_workdir)?;

    // Discover Python site-packages inside the image so pip installs
    // packages into the merged_dir instead of the host, and so that
    // subsequent RUN commands can import already-installed packages.
    let py_site_packages = std::fs::read_dir(format!("{}/usr/local/lib", merged_dir_str))
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(|e| e.ok())
                .find(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    n.starts_with("python") && e.path().join("site-packages").is_dir()
                })
                .map(|e| format!("{}/site-packages", e.path().to_string_lossy()))
        });

    // Replace bare pip/pip3 with python3 -m pip to ensure pip
    // uses the same Python version that python3 resolves to.
    let safe_cmd = if safe_cmd.starts_with("pip3 ") || safe_cmd.starts_with("pip ") {
        safe_cmd.replacen(
            safe_cmd.split_whitespace().next().unwrap(),
            "python3 -m pip",
            1,
        )
    } else {
        safe_cmd
            .replace(" && pip3 ", " && python3 -m pip ")
            .replace(" && pip ", " && python3 -m pip ")
    };

    // On macOS, strip --ignore-scripts from npm install so that
    // post-install scripts (e.g. puppeteer Chromium download) run.
    let safe_cmd = safe_cmd.replace(" --ignore-scripts", "");

    let mut build_cmd = std::process::Command::new("/bin/sh");
    build_cmd
        .arg("-c")
        .arg(&safe_cmd)
        .current_dir(&actual_workdir);

    // Forward Dockerfile ENV vars to the build command.
    for e in env {
        if let Some((k, v)) = e.split_once('=') {
            // On macOS, do NOT set PUPPETEER_SKIP_CHROMIUM_DOWNLOAD
            // so that `npm install puppeteer` downloads a native Chromium.
            if k == "PUPPETEER_SKIP_CHROMIUM_DOWNLOAD" {
                continue;
            }
            build_cmd.env(k, v);
        }
    }

    // Direct pip to install into the image's site-packages, not the host's.
    if let Some(ref sp) = py_site_packages {
        build_cmd.env("PIP_TARGET", sp);
        // Only set PYTHONPATH for non-pip commands; the host pip
        // (Python 3.12+) breaks when it loads the image's Python 3.9
        // pip modules via PYTHONPATH (pkgutil.ImpImporter removed).
        if !safe_cmd.contains("pip ") && !safe_cmd.starts_with("pip") {
            build_cmd.env("PYTHONPATH", sp);
        }
    }

    let output = build_cmd.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!(
            "macOS build command failed: {}{}{}",
            safe_cmd,
            if stderr.is_empty() { "" } else { " stderr: " },
            stderr
        );
    }
    Ok(())
}
