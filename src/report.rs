//! Printing results.
//!
//! Deliberately plain: a verdict, an id, and one line saying what happened. No
//! score and no ranking, because a backend failing ten cosmetic checks is not
//! worse than one silently dropping metrics, and a single number invites exactly
//! that comparison.

use crate::runner::{Outcome, Verdict};

pub fn print_table(outcome: &Outcome) {
    let version = outcome.backend_version.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
    println!("\n  {}{}  {}", outcome.backend, version, outcome.url);
    println!("  suite: {}\n", outcome.suite);

    let width = outcome.results.iter().map(|r| r.id.len()).max().unwrap_or(0).max(24);

    for result in &outcome.results {
        println!(
            "  {:<width$}  {:<7}  {}",
            result.id,
            label(result.verdict),
            result.detail,
            width = width
        );
    }

    let pass = outcome.count(Verdict::Pass);
    let reject = outcome.count(Verdict::Reject);
    let alter = outcome.count(Verdict::Alter);
    let not_applicable = outcome.count(Verdict::NotApplicable);
    println!(
        "\n  {} checks, {pass} pass, {reject} reject, {alter} alter, {not_applicable} n/a",
        outcome.results.len()
    );

    if alter > 0 {
        // Worth calling out separately. A rejection is visible to whoever sent
        // the data; an alteration is not, and that is the whole point.
        println!("  {alter} accepted then changed or lost — these fail silently in production");
    }
    println!();
}

fn label(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Pass => "PASS",
        Verdict::Reject => "REJECT",
        Verdict::Alter => "ALTER",
        Verdict::NotApplicable => "N/A",
    }
}
