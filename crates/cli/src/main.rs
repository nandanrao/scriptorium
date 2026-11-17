//! `scriptorium`: the headless command line. Collects the arguments, lets
//! [`scriptorium_cli::run`] do the work, and prints its output (notes on
//! stderr). `scriptorium mcp` serves MCP on stdin and stdout instead
//! ([`scriptorium_cli::mcp`]).

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // The agent connector: serves MCP until stdin ends.
    if args.first() == Some(&"mcp") {
        if args.len() > 1 {
            eprintln!("error: `scriptorium mcp` takes no arguments (got {})", args[1..].join(" "));
            return ExitCode::from(scriptorium_cli::commands::EXIT_USAGE);
        }
        scriptorium_cli::mcp::serve();
        return ExitCode::SUCCESS;
    }
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
