use clap::{CommandFactory, Parser};
use std::process::ExitCode;
use xort::cli::Cli;

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Generators that print and exit before doing any sorting.
    if let Some(shell) = cli.completions {
        let mut cmd = Cli::command();
        clap_complete::generate(shell, &mut cmd, "xort", &mut std::io::stdout());
        return ExitCode::SUCCESS;
    }
    if cli.man {
        let man = clap_mangen::Man::new(Cli::command());
        if let Err(e) = man.render(&mut std::io::stdout()) {
            eprintln!("xort: {e}");
            return ExitCode::from(2);
        }
        return ExitCode::SUCCESS;
    }

    let cfg = match cli.into_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("xort: {e}");
            return ExitCode::from(2);
        }
    };
    match xort::run(&cfg) {
        Ok(outcome) => {
            if let Some(s) = outcome.stats {
                let spill = match s.chunks {
                    Some(c) => format!(", {c} spilled chunk(s)"),
                    None => String::new(),
                };
                eprintln!(
                    "xort: {} in, {} out, {} duplicate(s) removed{}, {:.3}s",
                    s.lines_in, s.lines_out, s.duplicates_removed, spill, s.elapsed_secs
                );
            }
            ExitCode::from(outcome.exit_code as u8)
        }
        Err(e) => {
            eprintln!("xort: {e}");
            ExitCode::from(2)
        }
    }
}
