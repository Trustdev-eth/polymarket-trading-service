use anyhow::Result;
#[cfg(not(windows))]
use futures_util::{SinkExt, StreamExt};
use mm_engine::TokenRef;
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;
#[cfg(not(windows))]
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::POLY_USER_WS;

#[derive(Clone, Debug)]
pub struct UserAuth {
    pub api_key: String,
    pub secret: String,
    pub passphrase: String,
}

#[derive(Clone, Debug)]
pub enum UserParsed {
    Fill {
        cloid: [u8; 32],
        token: TokenRef,
        px_ticks: i32,
        size: f64,
        is_taker: bool,
    },
    Live {
        cloid: [u8; 32],
    },
}

pub async fn run_user_ws(
    auth: UserAuth,
    markets: Vec<String>,
    tx: UnboundedSender<UserParsed>,
) -> Result<()> {
    loop {
        #[cfg(windows)]
        {
            let auth_c = auth.clone();
            let markets_c = markets.clone();
            let tx_c = tx.clone();
            let res = tokio::task::spawn_blocking(move || stream_once_std(&auth_c, &markets_c, &tx_c))
                .await?;
            if let Err(e) = res {
                tracing::warn!(error=%e, "poly user ws");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        #[cfg(not(windows))]
        if let Err(e) = stream_once(&auth, &markets, &tx).await {
            tracing::warn!(error=%e, "poly user ws");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}

#[cfg(windows)]
fn stream_once_std(
    auth: &UserAuth,
    markets: &[String],
    tx: &UnboundedSender<UserParsed>,
) -> Result<()> {
    let sub = json!({
        "markets": markets,
        "type": "user",
        "auth": {
            "apiKey": auth.api_key,
            "secret": auth.secret,
            "passphrase": auth.passphrase,
        }
    });
    crate::win_ws::connect_text_loop(
        POLY_USER_WS,
        Some(sub.to_string()),
        None,
        std::time::Duration::from_secs(15),
        |txt| {
            for ev in parse_user_events(txt) {
                let _ = tx.send(ev);
            }
        },
    )
}

#[cfg(not(windows))]
async fn stream_once(
    auth: &UserAuth,
    markets: &[String],
    tx: &UnboundedSender<UserParsed>,
) -> Result<()> {
    let (ws, _) = connect_async(POLY_USER_WS).await?;
    let (mut sink, mut read) = ws.split();
    let sub = json!({
        "markets": markets,
        "type": "user",
        "auth": {
            "apiKey": auth.api_key,
            "secret": auth.secret,
            "passphrase": auth.passphrase,
        }
    });
    sink.send(Message::Text(sub.to_string().into())).await?;
    while let Some(msg) = read.next().await {
        let msg = msg?;
        let txt = match msg {
            Message::Text(t) => t,
            Message::Ping(p) => {
                let _ = sink.send(Message::Pong(p)).await;
                continue;
            }
            Message::Close(_) => break,
            _ => continue,
        };
        for ev in parse_user_events(&txt) {
            if tx.send(ev).is_err() {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Translate CLOB user-channel JSON into fills / live acks.
pub fn parse_user_events(txt: &str) -> Vec<UserParsed> {
    let v: Value = match serde_json::from_str(txt) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let items: Vec<Value> = if let Some(a) = v.as_array() {
        a.clone()
    } else {
        vec![v]
    };
    let mut out = Vec::new();
    for msg in items {
        let event_type = msg.get("event_type").and_then(|x| x.as_str()).unwrap_or("");
        let status = msg.get("status").and_then(|x| x.as_str()).unwrap_or("");
        if event_type == "trade" && status == "MATCHED" {
            if let Some(ev) = fill_from_trade(&msg) {
                out.push(ev);
            }
        }
        if event_type == "order" && (status == "LIVE" || status == "MATCHED") {
            if let Some(cloid) = order_cloid(&msg) {
                out.push(UserParsed::Live { cloid });
            }
        }
    }
    out
}

fn order_cloid(msg: &Value) -> Option<[u8; 32]> {
    let id = msg.get("id").and_then(|x| x.as_str())?;
    decode_cloid32(id)
}

fn fill_from_trade(msg: &Value) -> Option<UserParsed> {
    let px: f64 = msg
        .get("price")
        .and_then(|x| x.as_str().and_then(|s| s.parse().ok()).or_else(|| x.as_f64()))?;
    let size: f64 = msg
        .get("size")
        .and_then(|x| x.as_str().and_then(|s| s.parse().ok()).or_else(|| x.as_f64()))?;
    let outcome = msg.get("outcome").and_then(|x| x.as_str()).unwrap_or("Up");
    let token = if outcome.eq_ignore_ascii_case("Down") || outcome.eq_ignore_ascii_case("No") {
        TokenRef::No
    } else {
        TokenRef::Yes
    };
    let px_ticks = (px * 100.0).round() as i32;
    let cloid = msg
        .get("id")
        .and_then(|x| x.as_str())
        .and_then(decode_cloid32)
        .unwrap_or([0u8; 32]);
    Some(UserParsed::Fill {
        cloid,
        token,
        px_ticks,
        size,
        is_taker: msg.get("trader_side").and_then(|x| x.as_str()) == Some("TAKER"),
    })
}

pub fn decode_cloid32(id: &str) -> Option<[u8; 32]> {
    let h = id.trim().trim_start_matches("0x");
    if h.len() < 64 {
        return None;
    }
    let start = h.len() - 64;
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&h[start + i * 2..start + i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

pub fn cloid_hex(c: &[u8; 32]) -> String {
    let mut s = String::from("0x");
    for b in c {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_matched_trade() {
        let s = r#"{"event_type":"trade","status":"MATCHED","price":"0.51","size":"5","outcome":"Up","trader_side":"TAKER"}"#;
        let evs = parse_user_events(s);
        match &evs[0] {
            UserParsed::Fill {
                token,
                px_ticks,
                size,
                is_taker,
                ..
            } => {
                assert_eq!(*token, TokenRef::Yes);
                assert_eq!(*px_ticks, 51);
                assert!((*size - 5.0).abs() < 1e-9);
                assert!(*is_taker);
            }
            _ => panic!("fill"),
        }
    }
}
