//! Std-socket WebSocket + Windows Schannel TLS (no tokio/mio AFD, no rustls webpki).

use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use native_tls::TlsConnector;
use tungstenite::client::{client, IntoClientRequest};
use tungstenite::{Error, Message};

pub fn connect_text_loop(
    url: &str,
    first_text: Option<String>,
    abort: Option<Arc<AtomicBool>>,
    ping_every: Duration,
    mut on_text: impl FnMut(&str),
) -> Result<()> {
    let req = url.into_client_request()?;
    let host = req
        .uri()
        .host()
        .ok_or_else(|| anyhow::anyhow!("ws url missing host"))?
        .to_string();
    let port = req.uri().port_u16().unwrap_or(443);
    let tcp = TcpStream::connect((host.as_str(), port))
        .with_context(|| format!("ws tcp {host}:{port}"))?;
    let _ = tcp.set_nodelay(true);
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(1)));
    let tls = TlsConnector::new()?.connect(&host, tcp)?;
    let (mut ws, _) = client(req, tls)?;
    if let Some(t) = first_text {
        ws.send(Message::Text(t.into()))?;
    }
    let mut idle = 0u32;
    let ping_ticks = ping_every.as_secs().max(1) as u32;
    loop {
        if abort.as_ref().is_some_and(|a| a.load(Ordering::Relaxed)) {
            break;
        }
        match ws.read() {
            Ok(Message::Text(t)) => {
                idle = 0;
                on_text(t.as_str());
            }
            Ok(Message::Ping(p)) => {
                ws.send(Message::Pong(p))?;
            }
            Ok(Message::Pong(_)) => {}
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                idle += 1;
                if idle >= ping_ticks {
                    idle = 0;
                    ws.send(Message::Ping(Vec::new().into()))?;
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
