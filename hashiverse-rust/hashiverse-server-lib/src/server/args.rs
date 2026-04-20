//! # Server command-line arguments
//!
//! Clap-derived [`Args`] for the server binary: listen port, filesystem base path,
//! disk quota (MB), passphrase source (direct env, mounted secret, or file path),
//! log level, and a `skip_pq_commitment_bytes` testing flag.
//!
//! A builder-style constructor is provided for tests that want to spin up servers
//! with synthetic configs without touching `clap`'s `std::env::args` parser.

use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// Network port to use
    #[arg(long, default_value_t = 443)]
    pub port: u16,

    /// Force local network
    #[arg(long, default_value_t = false)]
    pub force_local_network: bool,

    /// Base path to server environments
    #[arg(long, default_value = "./.hashiverse")]
    pub base_path: String,

    /// Maximum size of the posts database
    #[arg(long, default_value_t = 20*1024)]
    pub max_post_database_size_megabytes: usize,

    /// From where can we read the passphrase passed into the container?
    #[arg(long)]
    pub passphrase_path: Option<String>,

    /// Log level
    #[arg(long, default_value = "trace")]
    pub log_level: String,

    /// Log level
    #[arg(long, default_value = "false")]
    pub skip_pq_commitment_bytes: bool
}

impl Args {
    pub fn new() -> Self {
        Args::parse()
    }

    pub fn default_for_testing() -> Self {
        let selfie = Self::parse_from([""]);
        selfie.with_skip_pq_commitment_bytes(true).with_force_local_network(true)
    }


    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn with_force_local_network(mut self, v: bool) -> Self {
        self.force_local_network = v;
        self
    }

    pub fn with_base_path(mut self, base_path: String) -> Self {
        self.base_path = base_path;
        self
    }

    pub fn with_max_post_database_size_megabytes(mut self, max_post_database_size_megabytes: usize) -> Self {
        self.max_post_database_size_megabytes = max_post_database_size_megabytes;
        self
    }

    pub fn with_skip_pq_commitment_bytes(mut self, skip_pq_commitment_bytes: bool) -> Self {
        self.skip_pq_commitment_bytes = skip_pq_commitment_bytes;
        self
    }
}

impl Default for Args {
    fn default() -> Self {
        Self::parse_from([""])
    }
}
