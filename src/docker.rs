//! Starting and stopping a backend from its adapter's `container:` block.
//!
//! Shelling out to the docker CLI rather than using a client library: the
//! adapter already describes the container in the terms the CLI takes, and a
//! maintainer reproducing a finding runs the same command by hand.

use anyhow::{Context, Result};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::backend::Container;

pub fn container_name(backend: &str) -> String {
    format!("specmatrix-{backend}")
}

/// A committed adapter must pin its tag. A floating tag makes every result a
/// claim about whatever the registry served that morning, and `docs/BACKENDS.md`
/// is explicit that a matrix without versions reads as a claim about the present.
pub fn check_pinned(container: &Container) -> Result<()> {
    // Split the last path segment, not the whole reference: a registry with a
    // port ("localhost:5000/foo") carries a colon and no tag at all.
    let last = container.image.rsplit('/').next().unwrap_or("");
    let tag = last.split_once(':').map(|(_, tag)| tag).unwrap_or("");
    if tag.is_empty() || tag == "latest" {
        anyhow::bail!(
            "adapter image {} is not pinned; pin the exact tag you tested",
            container.image
        );
    }
    Ok(())
}

/// Where an inline `container.config` is written before the container starts.
/// Fixed rather than derived from the backend name: `down` never needs to
/// know it, and a stale file from a previous run is overwritten, not
/// accumulated.
fn config_host_path(backend: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("specmatrix-{backend}-config.yaml"))
}

pub fn docker_run_args(backend: &str, container: &Container) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        container_name(backend),
        "-p".to_string(),
        format!("{0}:{0}", container.port),
    ];
    for port in &container.extra_ports {
        args.push("-p".to_string());
        args.push(format!("{port}:{port}"));
    }
    if container.config.is_some() {
        args.push("-v".to_string());
        args.push(format!("{}:/etc/specmatrix/config.yaml", config_host_path(backend).display()));
    }
    // Sorted, so two runs of one adapter produce the same command line and a
    // difference in a log is a real difference.
    let mut env: Vec<_> = container.env.iter().collect();
    env.sort();
    for (key, value) in env {
        args.push("-e".to_string());
        args.push(format!("{key}={value}"));
    }
    args.push(container.image.clone());
    args.extend(container.command.iter().cloned());
    args
}

/// Starts the container and blocks until its readiness probe passes.
pub fn up(backend: &str, container: &Container) -> Result<String> {
    check_pinned(container)?;
    let _ = down(backend);
    if let Some(config) = &container.config {
        std::fs::write(config_host_path(backend), config)
            .context("writing the adapter's inline config to a temp file")?;
    }
    let output = Command::new("docker")
        .args(docker_run_args(backend, container))
        .output()
        .context("running docker; is it installed and running?")?;
    if !output.status.success() {
        anyhow::bail!("docker run failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let url = format!("http://localhost:{}", container.port);
    if let Some(ready) = &container.ready {
        wait_ready(&url, &ready.request, ready.expect_status)
            .with_context(|| format!("waiting for {backend} to become ready"))?;
        if ready.settle_ms > 0 {
            std::thread::sleep(Duration::from_millis(ready.settle_ms));
        }
    }
    Ok(url)
}

fn wait_ready(base_url: &str, request: &str, expect_status: u16) -> Result<()> {
    let (_, path) = request
        .split_once(' ')
        .with_context(|| format!("ready.request must be `METHOD /path`, got {request:?}"))?;
    let path = path.trim();
    // An absolute URL is used as written, same as `send()` does for a
    // read-back on a store's other port — Tempo's readiness is its query
    // API on 3200, a different port than the OTLP receiver `container.port`
    // points `base_url` at.
    let url = if path.starts_with("http://") || path.starts_with("https://") {
        path.to_string()
    } else {
        format!("{base_url}{path}")
    };
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(2)).build()?;
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if let Ok(response) = client.get(&url).send() {
            if response.status().as_u16() == expect_status {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            anyhow::bail!("{url} did not return {expect_status} within 180s");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub fn down(backend: &str) -> Result<()> {
    Command::new("docker")
        .args(["rm", "-f", &container_name(backend)])
        .output()
        .context("running docker rm")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parseable() -> Container {
        serde_yaml::from_str(
            r#"
image: quay.io/parseablehq/parseable:v2.9.4
command: ["parseable", "local-store"]
port: 8000
env:
  P_USERNAME: admin
  P_PASSWORD: admin
"#,
        )
        .expect("container block parses")
    }

    /// Named after the backend, so a rerun replaces the container rather than
    /// colliding with one left behind.
    #[test]
    fn the_container_is_named_after_the_backend() {
        let args = docker_run_args("parseable", &parseable());
        let at = args.iter().position(|a| a == "--name").expect("--name present");
        assert_eq!(args[at + 1], "specmatrix-parseable");
    }

    #[test]
    fn the_port_is_published_on_the_same_number_inside_and_out() {
        let args = docker_run_args("parseable", &parseable());
        assert!(args.windows(2).any(|w| w[0] == "-p" && w[1] == "8000:8000"), "{args:?}");
    }

    /// Every declared variable must reach the container, or a backend starts
    /// with different settings than the matrix claims it was tested under.
    #[test]
    fn every_declared_env_var_is_passed() {
        let args = docker_run_args("parseable", &parseable());
        let passed: Vec<&String> =
            args.windows(2).filter(|w| w[0] == "-e").map(|w| &w[1]).collect();
        assert_eq!(passed.len(), 2);
        assert!(passed.iter().any(|v| v.as_str() == "P_USERNAME=admin"), "{passed:?}");
        assert!(passed.iter().any(|v| v.as_str() == "P_PASSWORD=admin"), "{passed:?}");
    }

    /// Image then command, in that order, or docker reads the command as part
    /// of the image reference.
    #[test]
    fn the_image_precedes_the_command() {
        let args = docker_run_args("parseable", &parseable());
        let image = args.iter().position(|a| a == "quay.io/parseablehq/parseable:v2.9.4");
        let command = args.iter().position(|a| a == "local-store");
        assert!(image < command, "{args:?}");
    }

    /// A store that answers ingest and query on two ports of one container
    /// needs both published, or its own cross-port read-back has nothing to
    /// reach.
    #[test]
    fn extra_ports_are_published_alongside_the_main_one() {
        let c: Container =
            serde_yaml::from_str("image: x/y:1\nport: 4318\nextra_ports: [16686]\n").unwrap();
        let args = docker_run_args("x", &c);
        assert!(args.windows(2).any(|w| w[0] == "-p" && w[1] == "4318:4318"), "{args:?}");
        assert!(args.windows(2).any(|w| w[0] == "-p" && w[1] == "16686:16686"), "{args:?}");
    }

    /// No `config:` means no mount at all, so every adapter written before
    /// this existed runs unchanged.
    #[test]
    fn no_config_means_no_volume_mount() {
        let args = docker_run_args("parseable", &parseable());
        assert!(!args.iter().any(|a| a == "-v"), "{args:?}");
    }

    /// An inline config is mounted at the fixed path every adapter's own
    /// `command` can point a flag at.
    #[test]
    fn an_inline_config_is_mounted_at_the_fixed_path() {
        let c: Container =
            serde_yaml::from_str("image: x/y:1\nport: 1\nconfig: |\n  key: value\n").unwrap();
        let args = docker_run_args("x", &c);
        let at = args.iter().position(|a| a == "-v").expect("-v present");
        assert!(args[at + 1].ends_with(":/etc/specmatrix/config.yaml"), "{}", args[at + 1]);
    }

    /// A settle is optional and defaults to none, so no adapter waits for a
    /// gap it does not have.
    #[test]
    fn a_container_without_a_settle_waits_for_nothing() {
        let c: Container = serde_yaml::from_str(
            "image: x/y:1\nport: 1\nready:\n  request: GET /health\n  expect_status: 200\n",
        )
        .unwrap();
        assert_eq!(c.ready.unwrap().settle_ms, 0);
    }

    #[test]
    fn an_unpinned_image_is_refused() {
        let c: Container =
            serde_yaml::from_str("image: openobserve/openobserve:latest\nport: 5080\n").unwrap();
        assert!(format!("{}", check_pinned(&c).unwrap_err()).contains("latest"));
    }

    /// A reference with no tag at all is equally unpinned.
    #[test]
    fn an_untagged_image_is_refused() {
        let c: Container = serde_yaml::from_str("image: grafana/loki\nport: 3100\n").unwrap();
        assert!(check_pinned(&c).is_err());
    }

    /// A registry with a port carries a colon and no tag; splitting the whole
    /// reference on the last colon would call that pinned.
    #[test]
    fn a_registry_port_is_not_mistaken_for_a_tag() {
        let c: Container =
            serde_yaml::from_str("image: localhost:5000/openobserve\nport: 5080\n").unwrap();
        assert!(check_pinned(&c).is_err());
    }

    #[test]
    fn a_pinned_image_is_accepted() {
        let c: Container =
            serde_yaml::from_str("image: openobserve/openobserve:v0.92.2\nport: 5080\n").unwrap();
        assert!(check_pinned(&c).is_ok());
    }

    /// Every adapter in the repository must be startable and pinned.
    #[test]
    fn every_committed_adapter_is_pinned_and_startable() {
        for entry in std::fs::read_dir("backends").expect("backends/ exists") {
            let path = entry.unwrap().path();
            if path.extension().map(|e| e != "yaml").unwrap_or(true) {
                continue;
            }
            let adapter = crate::backend::Backend::load(&path)
                .unwrap_or_else(|e| panic!("{} does not parse: {e:#}", path.display()));
            let container = adapter
                .container
                .as_ref()
                .unwrap_or_else(|| panic!("{} declares no container", path.display()));
            check_pinned(container).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        }
    }

    /// Every adapter must be able to say which version it is.
    ///
    /// A column without a version is a claim about the past that reads as a
    /// claim about the present, which is the one thing `docs/BACKENDS.md` says
    /// a column may not do. This is checked here rather than left to review
    /// because it already slipped through once: the Jaeger adapter shipped with
    /// no `version_from` at all and published a blank version for a whole
    /// suite, and nothing failed, because the audit only checked that the image
    /// was pinned.
    #[test]
    fn every_committed_adapter_can_report_a_version() {
        for entry in std::fs::read_dir("backends").expect("backends/ exists") {
            let path = entry.unwrap().path();
            if path.extension().map(|e| e != "yaml").unwrap_or(true) {
                continue;
            }
            let adapter = crate::backend::Backend::load(&path)
                .unwrap_or_else(|e| panic!("{} does not parse: {e:#}", path.display()));
            let vf = adapter
                .version_from
                .as_ref()
                .unwrap_or_else(|| panic!("{} declares no version_from", path.display()));
            assert!(
                vf.field.is_some() || vf.pattern.is_some(),
                "{}: version_from needs either a `field` or a `pattern`",
                path.display()
            );
        }
    }
}
