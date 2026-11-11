use anyhow::{ensure, Result};
use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(name = env!("CARGO_PKG_NAME"))]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(author = env!("CARGO_PKG_AUTHORS"))]
#[command(about = env!("CARGO_PKG_DESCRIPTION"), long_about = None)]
#[command(
    after_help = concat!("Repository:    ", env!("CARGO_PKG_REPOSITORY"))
)]
pub struct Args {
    #[arg(short = 'p', long, default_value = "8765")]
    pub http_port: u16,
    #[arg(short = 'u', long, default_value = "5678")]
    pub upstream_port: u16,
    /// Increase log verbosity (use twice to dump SOCKS5 packets)
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// Only log errors
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

pub fn parse_and_validate() -> Result<Args> {
    let args = Args::parse();
    ensure!(
        args.http_port != args.upstream_port,
        "HTTP port ({}) cannot be the same as upstream port ({})",
        args.http_port,
        args.upstream_port
    );
    ensure!(
        !(args.quiet && args.verbose > 0),
        "--quiet and --verbose cannot be used together"
    );
    Ok(args)
}
