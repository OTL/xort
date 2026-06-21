use clap::Parser;
use fsort::cli::Cli;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cfg = Cli::parse().into_config();
    match fsort::run(&cfg) {
        Ok(outcome) => {
            if let Some(s) = outcome.stats {
                eprintln!(
                    "fsort: {} in, {} out, {} duplicate(s) removed, {:.3}s",
                    s.lines_in, s.lines_out, s.duplicates_removed, s.elapsed_secs
                );
            }
            ExitCode::from(outcome.exit_code as u8)
        }
        Err(e) => {
            eprintln!("fsort: {e}");
            ExitCode::from(2)
        }
    }
}
