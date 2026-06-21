use clap::Parser;
use std::process::ExitCode;
use xort::cli::Cli;

fn main() -> ExitCode {
    let cfg = match Cli::parse().into_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("xort: {e}");
            return ExitCode::from(2);
        }
    };
    match xort::run(&cfg) {
        Ok(outcome) => {
            if let Some(s) = outcome.stats {
                eprintln!(
                    "xort: {} in, {} out, {} duplicate(s) removed, {:.3}s",
                    s.lines_in, s.lines_out, s.duplicates_removed, s.elapsed_secs
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
