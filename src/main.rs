//! SpecMatrix: send unusual-but-valid payloads to an observability backend,
//! read them back, and report what the backend did with them.

mod backend;
mod compare;
mod encode;
mod es;
mod case;
mod otlp;
mod query;
mod report;
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
        /// Base URL of the running backend, e.g. http://localhost:8000
        #[arg(long)]
        url: String,
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

            let run = runner::Runner::new(adapter, url, verbose)?;
            let outcome = run.run_suite(&suite, &corpus);

            if json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            } else {
                report::print_table(&outcome);
            }
            Ok(())
        }
    }
}
