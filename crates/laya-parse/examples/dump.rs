//! Print the chunks of one file: `cargo run -p laya-parse --example dump -- <file> [--text]`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: dump <file> [--text]");
        return ExitCode::FAILURE;
    };
    let show_text = args.any(|a| a == "--text");
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    for c in laya_parse::chunk_source(&path, &src) {
        println!(
            "L{}-{} ({} lines) [{}] {} | {} | defines: {} | refs: {}",
            c.start_line,
            c.end_line,
            c.line_count(),
            c.lang.as_str(),
            c.kind,
            c.symbol,
            c.defines.join(", "),
            c.refs.join(", ")
        );
        if show_text {
            println!("{}\n", c.text);
        }
    }
    ExitCode::SUCCESS
}
