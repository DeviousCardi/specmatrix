//! SpecMatrix: send unusual-but-valid payloads to an observability backend,
//! read them back, and report what the backend did with them.

mod backend;
mod case;
mod compare;
mod docker;
mod encode;
mod es;
mod loki;
mod matrix;
mod otlp;
mod query;
mod remote_write;
mod report;
mod runner;
mod stub;
mod template;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "specmatrix", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a suite of checks against one backend.
    Run {
        /// Backend adapter name, resolved to backends/<name>.yaml
        #[arg(long)]
        backend: String,
        /// Protocol suite, resolved to cases/<suite>/*.yaml
        #[arg(long)]
        suite: String,
        /// Base URL of the running backend. Defaults to the port in the
        /// adapter's container block, which is where `specmatrix up` puts it.
        #[arg(long)]
        url: Option<String>,
        /// Directory holding the corpus
        #[arg(long, default_value = "cases")]
        cases: PathBuf,
        /// Directory holding backend adapters
        #[arg(long, default_value = "backends")]
        backends: PathBuf,
        /// Emit results as JSON instead of a table
        #[arg(long)]
        json: bool,
        /// Print every request and response
        #[arg(long, short)]
        verbose: bool,
    },
    /// Convert a case payload into the bytes a backend receives.
    ///
    /// The corpus keeps payloads in a readable format, but two of the metric
    /// protocols only exist on the wire as protobuf — remote-write is also
    /// snappy-compressed. Without this, "reproduce it with curl" is impossible
    /// for exactly the backends whose findings are hardest to argue, and a
    /// maintainer has to take the runner's word for what was sent.
    Encode {
        /// Payload file, in the format the case declares
        payload: PathBuf,
        /// The format the file is written in
        #[arg(long)]
        from: String,
        /// The wire encoding to produce
        #[arg(long)]
        to: String,
        /// Where to write the bytes. Defaults to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Start a backend's container and wait until its readiness probe passes.
    Up {
        #[arg(long)]
        backend: String,
        #[arg(long, default_value = "backends")]
        backends: PathBuf,
    },
    /// Remove a backend's container.
    Down {
        #[arg(long)]
        backend: String,
    },
    /// Render a matrix.json as a single static page.
    Render {
        /// Path to a matrix.json produced by `specmatrix matrix`
        matrix: PathBuf,
        /// Where to write the page. Defaults to matrix.html beside the JSON.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run one suite against several backends and write the matrix.
    Matrix {
        /// Comma-separated adapter names
        #[arg(long, value_delimiter = ',')]
        backends: Vec<String>,
        #[arg(long)]
        suite: String,
        #[arg(long, default_value = "cases")]
        cases: PathBuf,
        #[arg(long = "adapters", default_value = "backends")]
        adapter_dir: PathBuf,
        /// Directory to write matrix.json and matrix.md into. Defaults to
        /// results/<date>/<suite>/.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Start each backend's container before its run and remove it after,
        /// so the whole matrix reproduces from the repository alone.
        #[arg(long)]
        manage: bool,
    },
}

/// The most recent prior run of `suite` under `results_dir`, strictly before
/// `current_generated`'s own date — so a rerun on the same day as a previous
/// one, or a re-render of an existing matrix.json, never compares against
/// itself. `results/<date>/<suite>/` is the layout every `matrix` and
/// `render` invocation writes, so this reads back exactly what was written.
fn find_previous_run(
    results_dir: &std::path::Path,
    suite: &str,
    current_generated: &str,
) -> Option<(String, matrix::Matrix)> {
    let current_date = current_generated.get(..10)?;
    let mut candidates: Vec<(String, PathBuf)> = std::fs::read_dir(results_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let date = e.file_name().to_string_lossy().into_owned();
            // Only a directory actually named as a date sorts and compares
            // meaningfully against `current_date`.
            if date.len() == 10 && date.as_str() < current_date {
                let json = e.path().join(suite).join("matrix.json");
                json.exists().then_some((date, json))
            } else {
                None
            }
        })
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    let (date, json_path) = candidates.pop()?;
    let text = std::fs::read_to_string(&json_path).ok()?;
    let previous: matrix::Matrix = serde_json::from_str(&text).ok()?;
    Some((format!("../../{date}/{suite}/matrix.html"), previous))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run { backend, suite, url, cases, backends, json, verbose } => {
            let adapter_path = backends.join(format!("{backend}.yaml"));
            let adapter = backend::Backend::load(&adapter_path)
                .with_context(|| format!("loading adapter {}", adapter_path.display()))?;
            let corpus = case::load_suite(&cases, &suite)
                .with_context(|| format!("loading suite {suite} from {}", cases.display()))?;
            if corpus.is_empty() {
                anyhow::bail!("no cases found for suite {suite} under {}", cases.display());
            }

            let url = url
                .or_else(|| adapter.default_url())
                .context("no --url given and the adapter declares no container port")?;
            let run = runner::Runner::new(adapter, url, verbose)?;
            let outcome = run.run_suite(&suite, &corpus);

            if json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            } else {
                report::print_table(&outcome);
            }
            Ok(())
        }

        Command::Encode { payload, from, to, out } => {
            let raw = std::fs::read(&payload)
                .with_context(|| format!("reading {}", payload.display()))?;
            // Template variables are rendered so the result is actually
            // sendable: a payload still carrying `{{ now_ms }}` encodes to a
            // timestamp of zero, which every store refuses for its own reasons
            // and none of them the one being reproduced.
            let vars = template::sendable_vars();
            let rendered = template::render_bytes(&raw, &vars);
            let wire = encode::to_wire(&from, &to, &rendered)?;
            match out {
                Some(path) => {
                    std::fs::write(&path, &wire)
                        .with_context(|| format!("writing {}", path.display()))?;
                    eprintln!("wrote {} ({} bytes)", path.display(), wire.len());
                    for (name, value) in encode::headers_for(&to) {
                        eprintln!("  -H '{name}: {value}'");
                    }
                }
                None => {
                    use std::io::Write;
                    std::io::stdout().write_all(&wire)?;
                }
            }
            Ok(())
        }

        Command::Up { backend, backends } => {
            let path = backends.join(format!("{backend}.yaml"));
            let adapter = backend::Backend::load(&path)
                .with_context(|| format!("loading adapter {}", path.display()))?;
            let container = adapter
                .container
                .as_ref()
                .with_context(|| format!("adapter {backend} declares no container"))?;
            let url = docker::up(&backend, container)?;
            println!("{} ready at {url}", adapter.name);
            Ok(())
        }

        Command::Down { backend } => docker::down(&backend),

        Command::Render { matrix: path, out } => {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let matrix: matrix::Matrix = serde_json::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            // The corpus commit lets a reader check out exactly the checks
            // that produced these cells.
            let commit = std::process::Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
            let previous =
                find_previous_run(&PathBuf::from("results"), &matrix.suite, &matrix.generated);
            let history = previous
                .as_ref()
                .map(|(link, m)| matrix::History { link: link.clone(), matrix: m });
            let page = matrix.to_html(commit.as_deref(), history.as_ref());
            let out = out.unwrap_or_else(|| path.with_file_name("matrix.html"));
            std::fs::write(&out, page).with_context(|| format!("writing {}", out.display()))?;
            println!("wrote {}", out.display());
            Ok(())
        }

        Command::Matrix { backends, suite, cases, adapter_dir, out, manage } => {
            let corpus = case::load_suite(&cases, &suite)
                .with_context(|| format!("loading suite {suite}"))?;
            if corpus.is_empty() {
                anyhow::bail!("no cases found for suite {suite}");
            }
            let mut outcomes = Vec::new();
            for name in &backends {
                let path = adapter_dir.join(format!("{name}.yaml"));
                let adapter = backend::Backend::load(&path)
                    .with_context(|| format!("loading adapter {}", path.display()))?;
                // A backend that does not speak this protocol is left out of
                // the table rather than filling a column with harness errors.
                if !adapter.protocols.contains_key(&suite) {
                    eprintln!("  {name}: no {suite} protocol declared, skipping");
                    continue;
                }
                let url = if manage {
                    let container = adapter
                        .container
                        .as_ref()
                        .with_context(|| format!("adapter {name} declares no container"))?;
                    eprintln!("  {name}: starting {}", container.image);
                    docker::up(name, container)?
                } else {
                    adapter
                        .default_url()
                        .with_context(|| format!("adapter {name} declares no container port"))?
                };
                eprintln!("  {name}: running {suite}");
                let run = runner::Runner::new(adapter, url, false)?;
                outcomes.push(run.run_suite(&suite, &corpus));
                if manage {
                    docker::down(name)?;
                }
            }

            let now = chrono::Utc::now();
            let matrix = matrix::Matrix::new(&suite, outcomes, now);
            let dir = out.unwrap_or_else(|| {
                PathBuf::from("results").join(now.format("%Y-%m-%d").to_string()).join(&suite)
            });
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            std::fs::write(dir.join("matrix.json"), serde_json::to_string_pretty(&matrix)?)?;
            std::fs::write(dir.join("matrix.md"), matrix.to_markdown())?;
            let commit = std::process::Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
            let previous =
                find_previous_run(&PathBuf::from("results"), &matrix.suite, &matrix.generated);
            let history = previous
                .as_ref()
                .map(|(link, m)| matrix::History { link: link.clone(), matrix: m });
            std::fs::write(
                dir.join("matrix.html"),
                matrix.to_html(commit.as_deref(), history.as_ref()),
            )?;
            println!("wrote {}/matrix.{{json,md,html}}", dir.display());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("specmatrix-find-previous-{name}-{:x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_matrix(results_dir: &std::path::Path, date: &str, suite: &str) {
        let dir = results_dir.join(date).join(suite);
        std::fs::create_dir_all(&dir).unwrap();
        let m = matrix::Matrix::new(
            suite,
            vec![],
            chrono::DateTime::parse_from_rfc3339(&format!("{date}T00:00:00Z"))
                .unwrap()
                .with_timezone(&chrono::Utc),
        );
        std::fs::write(dir.join("matrix.json"), serde_json::to_string(&m).unwrap()).unwrap();
    }

    /// Among several prior runs, the most recent one strictly before the
    /// current date is chosen — not the first found, not the oldest.
    #[test]
    fn the_most_recent_prior_run_before_today_is_chosen() {
        let dir = scratch_dir("several");
        write_matrix(&dir, "2026-01-01", "otlp-logs");
        write_matrix(&dir, "2026-04-01", "otlp-logs");
        write_matrix(&dir, "2026-07-01", "otlp-logs");
        let (link, previous) = find_previous_run(&dir, "otlp-logs", "2026-09-08T00:00:00+00:00")
            .expect("a previous run exists");
        assert_eq!(&previous.generated[..10], "2026-07-01");
        assert!(link.contains("2026-07-01"), "{link}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A run on or after the current date is never picked as "previous" —
    /// including a rerun on the same day, which must not compare against
    /// itself.
    #[test]
    fn a_run_on_or_after_the_current_date_is_never_previous() {
        let dir = scratch_dir("same-day");
        write_matrix(&dir, "2026-09-08", "otlp-logs");
        assert!(find_previous_run(&dir, "otlp-logs", "2026-09-08T00:00:00+00:00").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A previous run of a different suite is never mistaken for this one's
    /// history.
    #[test]
    fn a_different_suites_run_is_not_history() {
        let dir = scratch_dir("other-suite");
        write_matrix(&dir, "2026-07-01", "es-bulk");
        assert!(find_previous_run(&dir, "otlp-logs", "2026-09-08T00:00:00+00:00").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_results_directory_at_all_is_simply_no_history() {
        let dir = std::env::temp_dir().join("specmatrix-find-previous-does-not-exist");
        assert!(find_previous_run(&dir, "otlp-logs", "2026-09-08T00:00:00+00:00").is_none());
    }
}
