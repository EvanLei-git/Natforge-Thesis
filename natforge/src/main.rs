//! NatForge - Proxy Node (CLI agent).
//!
//!   * `service-host` - expose one or more local services through a reverse tunnel.

mod auth;
mod service_host;
mod tls;

use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};
use tracing::error;

use crate::service_host::RouteSpec;
use natforge_proto::RouteMode;

#[derive(Parser)]
#[command(name = "natforge")]
#[command(about = "NatForge data-plane agent (Service Host)", long_about = None)]
struct Cli {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Subcommand)]
enum Mode {
    /// Expose local services through a reverse tunnel.
    ServiceHost {
        /// A route to expose, as `<local_port>:<http|https|tcp>`. Repeatable.
        /// Example: --route 8000:http --route 25565:tcp
        #[arg(long = "route")]
        routes: Vec<String>,

        /// Legacy convenience: expose a single TCP port (equivalent to `--route <port>:tcp`).
        #[arg(short, long)]
        local_port: Option<u16>,

        /// Control-plane (website backend) API endpoint.
        #[arg(short, long, default_value = "http://127.0.0.1:3000")]
        control_plane: String,

        /// Region/node id to host the tunnel on (see the dashboard region list).
        /// Omit to use the platform's default region.
        #[arg(long)]
        region: Option<String>,

        /// Override the core proxy control address (host:port). Normally learned
        /// from the reservation; set this only for local dev against a node whose
        /// advertised control endpoint isn't reachable (e.g. `127.0.0.1:4000`).
        #[arg(short, long)]
        tunnel_server: Option<String>,

        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        email: Option<String>,
        #[arg(long)]
        password: Option<String>,
    },
}

fn parse_routes(routes: &[String], local_port: Option<u16>) -> Result<Vec<RouteSpec>> {
    let mut out = Vec::new();
    for spec in routes {
        let (port_s, mode_s) = spec.split_once(':').ok_or_else(|| {
            anyhow!("invalid --route '{spec}', expected <local_port>:<http|https|tcp>")
        })?;
        let local_port: u16 = port_s
            .parse()
            .map_err(|_| anyhow!("invalid port in --route '{spec}'"))?;
        let mode = match mode_s {
            "http" => RouteMode::Http,
            "https" => RouteMode::Https,
            "tcp" => RouteMode::Tcp,
            other => return Err(anyhow!("invalid mode '{other}' in --route '{spec}'")),
        };
        out.push(RouteSpec { mode, local_port });
    }
    if let Some(p) = local_port {
        out.push(RouteSpec {
            mode: RouteMode::Tcp,
            local_port: p,
        });
    }
    if out.is_empty() {
        return Err(anyhow!(
            "specify at least one --route <local_port>:<mode> (or --local-port)"
        ));
    }
    Ok(out)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .init();

    let cli = Cli::parse();
    match &cli.mode {
        Mode::ServiceHost {
            routes,
            local_port,
            control_plane,
            region,
            tunnel_server,
            token,
            email,
            password,
        } => {
            let specs = parse_routes(routes, *local_port)?;
            let session = auth::obtain_token(control_plane, token, email, password).await?;
            if let Err(e) = service_host::run(
                control_plane,
                tunnel_server.as_deref(),
                region.as_deref(),
                specs,
                &session,
            )
            .await
            {
                error!("service host stopped: {e}");
            }
        }
    }
    Ok(())
}
