#[path = "asr_performance/args.rs"]
mod args;
#[path = "asr_performance/corpus.rs"]
mod corpus;
#[path = "asr_performance/report.rs"]
mod report;
#[path = "asr_performance/runner.rs"]
mod runner;

use std::io::Write;

fn main() {
    let result = args::parse(std::env::args_os().skip(1)).and_then(runner::execute);
    match result {
        Ok((report, mut output)) => {
            if serde_json::to_writer_pretty(&mut output, &report).is_err()
                || output.write_all(b"\n").is_err()
            {
                eprintln!("PTT2me ASR performance benchmark failed: could not write JSON report");
                std::process::exit(2);
            }
            if report.status != "PASS" {
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("PTT2me ASR performance benchmark failed: {error}");
            std::process::exit(2);
        }
    }
}
