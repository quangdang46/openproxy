use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "openproxy", about = "Local AI routing gateway")]
pub struct Cli {
    #[arg(long, env = "HOST", default_value = "0.0.0.0")]
    pub host: String,

    #[arg(long, env = "PORT", default_value_t = 20128)]
    pub port: u16,

    #[arg(long, env = "RUST_LOG", default_value = "info")]
    pub log_filter: String,

    #[arg(long, env = "DATA_DIR")]
    pub data_dir: Option<PathBuf>,
}
