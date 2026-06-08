//! Wire format for the agent <-> core-proxy control handshake.
//!
//! The handshake structs and the per-stream preamble codec live in the shared
//! `natforge-proto` crate so the agent and core can never drift. This module just
//! re-exports them and keeps the length-prefixed framing helpers used to exchange
//! the two handshake frames before the socket is upgraded to yamux.

// The agent only *reads* preambles (the core writes them) and uses the handshake
// types; re-export exactly that surface.
pub use natforge_proto::{read_preamble, AgentHello, AgentRouteBinding, CoreReply, RouteMode};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const MAX_FRAME: u32 = 1 << 20;

pub async fn read_frame(stream: &mut TcpStream) -> anyhow::Result<Vec<u8>> {
    let len = stream.read_u32().await?;
    if len > MAX_FRAME {
        anyhow::bail!("frame too large ({len} bytes)");
    }
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn write_frame(stream: &mut TcpStream, data: &[u8]) -> anyhow::Result<()> {
    stream.write_u32(data.len() as u32).await?;
    stream.write_all(data).await?;
    stream.flush().await?;
    Ok(())
}
