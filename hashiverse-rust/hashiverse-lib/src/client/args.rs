//! # Client command-line argument parsing
//!
//! A placeholder clap-derived [`Args`] struct used by the reference client entry points.
//! Currently empty — all runtime knobs still live in [`crate::tools::config`] — but the
//! skeleton is here so command-line flags can be added later without forcing every caller
//! to take a new dependency on clap.

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(version, about, long_about = None)]
pub struct Args {
}

impl Args {
    pub fn new() -> Self {
        Args::parse()
    }
}

impl Default for Args {
    fn default() -> Self {
        Args::parse_from(&[""])
    }
}