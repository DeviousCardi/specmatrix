//! SpecMatrix: send unusual-but-valid payloads to an observability backend,
//! read them back, and report what the backend did with them.

mod backend;
mod compare;
mod docker;
mod encode;
mod es;
mod case;
mod matrix;
mod otlp;
mod query;
mod report;
mod stub;
mod runner;
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
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("creating {}", dir.display()))?;
            std::fs::write(dir.join("matrix.json"), serde_json::to_string_pretty(&matrix)?)?;
            std::fs::write(dir.join("matrix.md"), matrix.to_markdown())?;
            println!("wrote {}/matrix.json and matrix.md", dir.display());
            Ok(())
        }
    }
}
