//! Command-line entry point for the WH3MM Steam helper.

use std::{env, io::Write, process};

use wh3mm_steam_helper::{HelperPaths, run_streaming_with_args};

fn main() {
    let paths = HelperPaths::from_env();
    let args = env::args().skip(1);

    match run_streaming_with_args(args, &paths, |output| {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{output}")
            .and_then(|()| stdout.flush())
            .map_err(|error| wh3mm_steam_helper::HelperError::output(error.to_string()))
    }) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("{error}");
            process::exit(error.exit_code());
        }
    }
}
