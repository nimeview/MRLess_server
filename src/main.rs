use anyhow::Result;
use clap::Parser;
use mrless::config::{AppConfig, CliOverrides};
use mrless::identity::{default_data_dir, Identity};
use mrless::node::Node;
use mrless::www_server;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "mrless", about = "MRLess local mesh node")]
struct Args {
    /// Display name for this peer (also marks the node onboarded)
    #[arg(long, short)]
    name: Option<String>,

    /// Isolate multiple nodes on one machine
    #[arg(long, default_value = "default")]
    instance: String,

    /// TCP port for mesh peer links (overrides config.json)
    #[arg(long)]
    mesh_port: Option<u16>,

    /// WebSocket port for the local UI (overrides config.json)
    #[arg(long)]
    ws_port: Option<u16>,

    /// UDP broadcast discovery port (overrides config.json)
    #[arg(long)]
    discovery_port: Option<u16>,

    /// Serve the Solid UI from this folder (packaged desktop)
    #[arg(long)]
    www: Option<PathBuf>,

    /// HTTP port for `--www` (default 3000)
    #[arg(long, default_value_t = 3000)]
    ui_port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let data_dir = default_data_dir(&args.instance);
    let cli = CliOverrides {
        mesh_port: args.mesh_port,
        ws_port: args.ws_port,
        discovery_port: args.discovery_port,
    };
    let cfg = AppConfig::load_or_create(&data_dir, &cli)?;

    let filter = match std::env::var_os("RUST_LOG") {
        Some(_) => EnvFilter::from_default_env(),
        None => EnvFilter::new(format!("mrless={}", cfg.log_level)),
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let identity = Identity::load_or_create(&data_dir, args.name)?;
    let ws_port = cfg.ws_port;

    tracing::info!(
        "starting MRLess node {} as '{}' onboarded={} (data: {})",
        identity.node_id,
        identity.name,
        identity.onboarded,
        data_dir.display()
    );

    let _node = Node::start(data_dir, cfg, identity)?;

    if let Some(www) = args.www {
        www_server::serve_www(www, args.ui_port, ws_port).await?;
    } else {
        std::future::pending::<()>().await;
    }
    Ok(())
}
