//! NatForge - Proxy Node (CLI agent).
//!
//!   * `service-host` - expose one or more local services through a reverse tunnel.

// This workspace is written in an intentionally expanded, one-statement-per-line
// style: explicit `for`/`match` blocks instead of iterator and Option/Result
// combinator chains, for readability. Allow the clippy lints that would otherwise
// push it back toward the terser idiomatic forms.
#![allow(
    clippy::manual_map,
    clippy::manual_filter,
    clippy::manual_find,
    clippy::manual_flatten,
    clippy::manual_unwrap_or,
    clippy::manual_unwrap_or_default,
    clippy::needless_range_loop,
    clippy::comparison_chain
)]

mod auth;
mod device;
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
        /// A route to expose, as `<local_port>:<http|https|tcp|udp|both>`. Repeatable.
        /// Example: --route 8000:http --route 25565:tcp --route 2456:udp
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

    /// Enroll this machine as a persistent, dashboard-managed device (one-time). Prints
    /// a short code; enter it in the dashboard's "Add device" to link this machine.
    Enroll {
        /// Control-plane (website backend) API endpoint.
        #[arg(short, long, default_value = "http://127.0.0.1:3000")]
        control_plane: String,
    },

    /// Run the enrolled device using the stored device token.
    Run {
        /// Override the control-plane endpoint (defaults to the one from enrolment).
        #[arg(short, long)]
        control_plane: Option<String>,
        /// Override the node control address (host:port), for local dev.
        #[arg(short, long)]
        tunnel_server: Option<String>,
    },

    /// Install the agent as a background service (systemd user unit) that starts on
    /// boot and auto-restarts. Run `natforge enroll` first, then this.
    InstallService,

    /// Remove the background service installed by `install-service`.
    UninstallService,
}

fn parse_routes(routes: &[String], local_port: Option<u16>) -> Result<Vec<RouteSpec>> {
    let mut out = Vec::new();
    for spec in routes {
        let (port_s, mode_s) = match spec.split_once(':') {
            Some(v) => v,
            None => {
                return Err(anyhow!(
                    "invalid --route '{spec}', expected <local_port>:<http|https|tcp|udp|both>"
                ));
            }
        };
        let local_port: u16 = match port_s.parse() {
            Ok(v) => v,
            Err(_) => {
                return Err(anyhow!("invalid port in --route '{spec}'"));
            }
        };
        let mode = match mode_s {
            "http" => RouteMode::Http,
            "https" => RouteMode::Https,
            "tcp" => RouteMode::Tcp,
            "udp" => RouteMode::Udp,
            "both" => RouteMode::Both,
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

/// Path to the per-user systemd unit for the agent.
fn service_unit_path() -> Result<std::path::PathBuf> {
    let home = match std::env::var("HOME") {
        Ok(v) => v,
        Err(_) => {
            return Err(anyhow!("HOME is not set"));
        }
    };
    Ok(std::path::PathBuf::from(home).join(".config/systemd/user/natforge.service"))
}

/// Run a command, failing with a clear message (e.g. if systemd is not installed).
fn sh(cmd: &str, args: &[&str]) -> Result<()> {
    let status = match std::process::Command::new(cmd).args(args).status() {
        Ok(v) => v,
        Err(e) => {
            return Err(anyhow!(
                "could not run `{cmd}` ({e}); is systemd installed?"
            ));
        }
    };
    if !status.success() {
        return Err(anyhow!("`{cmd} {}` failed ({status})", args.join(" ")));
    }
    Ok(())
}

/// Install a systemd *user* service that runs `natforge run` on boot, auto-restarting,
/// with linger enabled so it survives logout and reboot. Needs no root.
fn install_service() -> Result<()> {
    let exe = std::env::current_exe()?;
    let path = service_unit_path()?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    let unit = format!(
        "[Unit]\nDescription=NatForge agent\n\n[Service]\nExecStart={} run\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
        exe.display()
    );
    std::fs::write(&path, unit)?;
    sh("systemctl", &["--user", "daemon-reload"])?;
    sh(
        "systemctl",
        &["--user", "enable", "--now", "natforge.service"],
    )?;
    if sh("loginctl", &["enable-linger"]).is_err() {
        eprintln!(
            "note: could not enable linger automatically; run `loginctl enable-linger` so the agent survives a reboot."
        );
    }
    println!("Installed. The NatForge agent now runs in the background and starts on boot.");
    println!("  status: systemctl --user status natforge");
    println!("  logs:   journalctl --user -u natforge -f");
    println!("  (run `natforge enroll` first if you have not, so the agent has a device token)");
    Ok(())
}

/// Stop and remove the service installed by `install_service`.
fn uninstall_service() -> Result<()> {
    let _ = sh(
        "systemctl",
        &["--user", "disable", "--now", "natforge.service"],
    );
    if let Ok(path) = service_unit_path() {
        let _ = std::fs::remove_file(path);
    }
    let _ = sh("systemctl", &["--user", "daemon-reload"]);
    println!("Removed the NatForge background service.");
    Ok(())
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
        Mode::Enroll { control_plane } => {
            let dev = device::enroll(control_plane).await?;
            device::save(&dev)?;
            println!(
                "Enrolled '{}'. Token saved to {}. Start it any time with: natforge run",
                dev.name,
                device::config_path()?.display()
            );
        }
        Mode::Run {
            control_plane,
            tunnel_server,
        } => {
            let dev = device::load()?;
            let cp = match control_plane.clone() {
                Some(v) => v,
                None => dev.control_plane.clone(),
            };
            tracing::info!(
                "running device '{}' (#{}) against {cp}",
                dev.name,
                dev.device_id
            );
            if let Err(e) =
                service_host::run_device(&cp, &dev.device_token, tunnel_server.as_deref()).await
            {
                error!("device run stopped: {e}");
            }
        }
        Mode::InstallService => install_service()?,
        Mode::UninstallService => uninstall_service()?,
    }
    Ok(())
}
