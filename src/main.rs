use clap::Parser;
use musorter::{cli, error, exec, plan, report};

#[tokio::main]
async fn main() {
    let code = match run().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("musorter: {e}");
            2
        }
    };
    std::process::exit(code);
}

async fn run() -> Result<i32, error::AppError> {
    let args = cli::Args::parse();
    let cfg = cli::validate(args)?;

    // Phase 1+2: build the complete in→out map (no disk writes).
    let plan = plan::build(&cfg).await?;

    // Phase 3: execute (skipped entirely in dry-run mode — zero writes).
    let exec = if plan.dry_run {
        None
    } else {
        Some(exec::execute(&plan).await)
    };

    // Phase 5: report + exit code.
    if std::env::var_os("MUSORTER_DEBUG_STATS").is_some() {
        eprintln!("musorter stats: hashed={}", plan.hashes_computed);
    }
    let (lines, summary) = report::render(&plan, exec.as_ref(), cfg.omit_success);
    for l in &lines {
        println!("{l}");
    }
    for l in report::summary_block(&summary) {
        println!("{l}");
    }

    Ok(if summary.unprocessed > 0 { 1 } else { 0 })
}
