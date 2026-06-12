//! Command-line entry point for the WH3MM Steam helper.

use std::{env, process};

use wh3mm_steam_helper::{HelperPaths, run_with_args};

fn main() {
    let paths = HelperPaths::from_env();
    let args = env::args().skip(1);

    match run_with_args(args, &paths) {
        Ok(output) => {
            println!("{output}");
        }
        Err(error) => {
            eprintln!("{error}");
            process::exit(error.exit_code());
        }
    }
}
