//! `scriptorium`: the headless command line. Collects the arguments, lets
//! [`scriptorium_cli::run`] do the work, and prints its output (notes on
//! stderr).

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut notes = vec![];
    let result = scriptorium_cli::run_command(&args, &mut notes);
    for n in &notes {
        eprintln!("note: {n}");
    }
    match result {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            print!("{}", e.stdout);
            eprintln!("error: {}", e.message);
            ExitCode::from(e.code)
        }
    }
}
