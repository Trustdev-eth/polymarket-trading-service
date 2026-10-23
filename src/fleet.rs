//! UK multi-key CLOB fleet. Decrypts tenant keys here. Tokyo only sends tenant_id + size + side.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::join_all;
use futures_util::TryStreamExt;
use mm_clob::{parse_address, ApiCreds, ClobClient, DualLimiter, MarketResolved, V2Signer};
use mongodb::bson::doc;
use mongodb::{Client, Collection};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::crypto::{open, wrap_key};
use crate::models::{FleetMsg, FleetReply, FleetTake, Member};

type SlotMap = HashMap<String, Arc<Mutex<Slot>>>;

pub struct FleetArgs {
    pub bind: String,
    pub token: String,
    pub mongo_uri: String,
    pub mongo_db: String,
    pub key_wrap_secret: String,
    pub paper: bool,
    pub allow_live: bool,
    pub shard: u32,
    pub shards: u32,
    pub chain_id: u64,
    pub clob_host: String,
}

struct Slot {
    clob: ClobClient,
    limiter: DualLimiter,
}

pub async fn run(args: FleetArgs) -> Result<()> {
    if args.allow_live {
        wrap_key(&args.key_wrap_secret)?;
    }
    if args.shards == 0 {
        anyhow::bail!("SHARDS must be >= 1");
    }
    let db = Client::with_uri_str(&args.mongo_uri)
        .await?
        .database(&args.mongo_db);
    let members: Collection<Member> = db.collection("members");
    let slots = load_slots(&members, &args).await?;
    tracing::info!(n = slots.len(), paper = args.paper, shard = args.shard, "fleet keys loaded");

    let listener = TcpListener::bind(&args.bind).await?;
    tracing::info!(bind = %args.bind, "trade-fleet listening");
    loop {
        let (sock, peer) = listener.accept().await?;
        tracing::info!(%peer, "coordinator connected");
        let args = clone_args(&args);
        let members = members.clone();
        let mut slots = load_slots(&members, &args).await.unwrap_or_default();
        tokio::spawn(async move {
            if let Err(e) = handle(sock, args, members, &mut slots).await {
                tracing::warn!(error=%e, "fleet session");
            }
        });
    }
}

fn clone_args(a: &FleetArgs) -> FleetArgs {
    FleetArgs {
        bind: a.bind.clone(),
        token: a.token.clone(),
        mongo_uri: a.mongo_uri.clone(),
        mongo_db: a.mongo_db.clone(),
        key_wrap_secret: a.key_wrap_secret.clone(),
        paper: a.paper,
        allow_live: a.allow_live,
        shard: a.shard,
        shards: a.shards,
        chain_id: a.chain_id,
        clob_host: a.clob_host.clone(),
    }
}

fn on_shard(email: &str, shard: u32, shards: u32) -> bool {
    if shards <= 1 {
        return true;
    }
    let h = email.bytes().fold(0u32, |a, b| a.wrapping_mul(16777619) ^ b as u32);
    h % shards == shard
}

async fn load_slots(col: &Collection<Member>, args: &FleetArgs) -> Result<SlotMap> {
    let mut cur = col.find(doc! { "keys.secretEnc": { "$exists": true, "$ne": "" } }, None).await?;
    let mut out = HashMap::new();
    let master = wrap_key(&args.key_wrap_secret).ok();
    let paper = args.paper || !args.allow_live;
    while let Some(m) = cur.try_next().await? {
        if !on_shard(&m.email, args.shard, args.shards) {
            continue;
        }
        let Some(k) = m.keys else { continue };
        let Some(master) = master.as_ref() else { continue };
        let api_key = if !k.api_key_enc.is_empty() {
            let Ok(v) = open(master, &k.api_key_enc) else { continue };
            v
        } else if !k.api_key.is_empty() {
            k.api_key.clone()
        } else {
            continue;
        };
        let Ok(secret) = open(master, &k.secret_enc) else { continue };
        let passphrase = if k.pass_enc.is_empty() {
            String::new()
        } else {
            open(master, &k.pass_enc).unwrap_or_default()
        };
        let Ok(funder) = parse_address(&k.funder) else { continue };
        // Encrypted wallet key decrypts here only. Empty keyEnc keeps the slot paper.
        const PAPER_SIGNER: &str =
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
        let (signer, has_signer) = if k.key_enc.is_empty() {
            match V2Signer::from_hex_on_chain(PAPER_SIGNER, args.chain_id) {
                Ok(s) => (s, false),
                Err(_) => continue,
            }
        } else {
            let Ok(pk) = open(master, &k.key_enc) else { continue };
            match V2Signer::from_hex_on_chain(&pk, args.chain_id) {
                Ok(s) => (s, true),
                Err(_) => continue,
            }
        };
        let creds = ApiCreds {
            api_key,
            secret,
            passphrase,
        };
        let clob = ClobClient::new(
            args.clob_host.clone(),
            signer,
            creds,
            funder,
            k.sig_type as u8,
            paper || !has_signer,
        );
        out.insert(
            m.email.clone(),
            Arc::new(Mutex::new(Slot {
                clob,
                limiter: DualLimiter::production(),
            })),
        );
    }
    Ok(out)
}

async fn handle(
    sock: TcpStream,
    args: FleetArgs,
    members: Collection<Member>,
    slots: &mut SlotMap,
) -> Result<()> {
    let (r, mut w) = sock.into_split();
    let mut lines = BufReader::new(r).lines();
    let mut authed = args.token.is_empty();
    let mut hello: Option<MarketResolved> = None;
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: FleetMsg = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error=%e, "bad fleet frame");
                continue;
            }
        };
        match msg {
            FleetMsg::Auth { token } => {
                authed = token.trim() == args.token.trim();
                if !authed {
                    anyhow::bail!("fleet auth failed");
                }
            }
            FleetMsg::Hello {
                yes_id,
                no_id,
                tick,
                fee_bps,
                neg_risk,
            } => {
                if !authed {
                    anyhow::bail!("auth first");
                }
                let yes = mm_clob::u256_from_dec(&yes_id).map_err(|e| anyhow::anyhow!(e))?;
                let no = mm_clob::u256_from_dec(&no_id).map_err(|e| anyhow::anyhow!(e))?;
                let mkt = MarketResolved {
                    yes_id: yes,
                    no_id: no,
                    tick_size: tick,
                    fee_bps,
                    neg_risk,
                };
                for s in slots.values() {
                    let mut s = s.lock().await;
                    s.clob.set_market(mkt.clone());
                    s.clob.warmup().await;
                }
                hello = Some(mkt);
            }
            FleetMsg::Take {
                email,
                token,
                px_ticks,
                size,
                tif,
                post_only,
                seq,
                delay_ms: _,
            } => {
                if !authed {
                    continue;
                }
                fan_out(
                    slots,
                    hello.as_ref(),
                    vec![FleetTake {
                        email,
                        token,
                        px_ticks,
                        size,
                        tif,
                        post_only,
                        seq,
                    }],
                    &mut w,
                )
                .await;
            }
            FleetMsg::Wave { takes } => {
                if !authed {
                    continue;
                }
                match load_slots(&members, &args).await {
                    Ok(fresh) => {
                        *slots = fresh;
                        if let Some(mkt) = hello.as_ref() {
                            for s in slots.values() {
                                let mut s = s.lock().await;
                                s.clob.set_market(mkt.clone());
                            }
                        }
                        tracing::info!(n = slots.len(), "fleet keys reloaded");
                    }
                    Err(e) => tracing::warn!(error=%e, "fleet key reload"),
                }
                tracing::info!(n = takes.len(), "fan-out live POSTs");
                fan_out(slots, hello.as_ref(), takes, &mut w).await;
            }
            FleetMsg::Halt { email } => {
                if let Some(s) = slots.get(&email) {
                    let s = s.lock().await;
                    let _ = s.clob.cancel_all().await;
                }
            }
            FleetMsg::HaltAll => {
                for s in slots.values() {
                    let s = s.lock().await;
                    let _ = s.clob.cancel_all().await;
                }
            }
            FleetMsg::Cancel { email, .. } => {
                if let Some(s) = slots.get(&email) {
                    let s = s.lock().await;
                    let _ = s.clob.cancel_all().await;
                }
            }
        }
    }
    Ok(())
}

async fn fan_out(
    slots: &SlotMap,
    hello: Option<&MarketResolved>,
    takes: Vec<FleetTake>,
    w: &mut (impl AsyncWriteExt + Unpin),
) {
    let hello = hello.cloned();
    let futs: Vec<_> = takes
        .into_iter()
        .map(|t| {
            let slot = slots.get(&t.email).cloned();
            let hello = hello.clone();
            async move { post_take_slot(slot, hello.as_ref(), t).await }
        })
        .collect();
    for reply in join_all(futs).await {
        if let Ok(buf) = serde_json::to_vec(&reply) {
            let mut line = buf;
            line.push(b'\n');
            let _ = w.write_all(&line).await;
        }
    }
}

async fn post_take_slot(
    slot: Option<Arc<Mutex<Slot>>>,
    hello: Option<&MarketResolved>,
    t: FleetTake,
) -> FleetReply {
    let Some(slot) = slot else {
        return FleetReply::Reject {
            email: t.email,
            seq: t.seq,
            reason: "no-slot".into(),
        };
    };
    let mut slot = slot.lock().await;
    if !slot.limiter.try_acquire(1.0) {
        return FleetReply::Reject {
            email: t.email,
            seq: t.seq,
            reason: "limiter".into(),
        };
    }
    let Some(mkt) = hello else {
        return FleetReply::Reject {
            email: t.email,
            seq: t.seq,
            reason: "no-hello".into(),
        };
    };
    let token_id = if t.token.eq_ignore_ascii_case("yes") {
        mkt.yes_id
    } else {
        mkt.no_id
    };
    let px = t.px_ticks as f64 / 100.0;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let signed = slot.clob.sign_limit(
        token_id,
        true,
        px,
        t.size,
        &t.tif,
        t.post_only,
        now,
        t.seq as u128,
        0,
    );
    let Ok((body, _, _)) = signed else {
        return FleetReply::Reject {
            email: t.email,
            seq: t.seq,
            reason: "sign".into(),
        };
    };
    match slot.clob.post_order_bytes(&body).await {
        Ok(v) => reply_from_post(t, v),
        Err(e) => FleetReply::Reject {
            email: t.email,
            seq: t.seq,
            reason: e,
        },
    }
}

fn json_f64(v: Option<&serde_json::Value>) -> Option<f64> {
    match v? {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn reply_from_post(t: FleetTake, v: serde_json::Value) -> FleetReply {
    if v.get("success").and_then(|x| x.as_bool()) == Some(false) {
        let reason = v
            .get("errorMsg")
            .or_else(|| v.get("error"))
            .and_then(|x| x.as_str())
            .unwrap_or("rejected")
            .to_string();
        return FleetReply::Reject {
            email: t.email,
            seq: t.seq,
            reason,
        };
    }
    let size = ["takingAmount", "taking_amount", "size", "filledSize"]
        .into_iter()
        .find_map(|k| json_f64(v.get(k)));
    let px = json_f64(v.get("price")).unwrap_or(t.px_ticks as f64 / 100.0);
    let status = v
        .get("status")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_lowercase();
    match size {
        Some(n) if n > 1e-9 => {
            return FleetReply::Fill {
                email: t.email,
                seq: t.seq,
                size: n,
                px,
            };
        }
        Some(_) => {}
        None if status == "matched" => {
            return FleetReply::Fill {
                email: t.email,
                seq: t.seq,
                size: t.size,
                px,
            };
        }
        None => {}
    }
    if t.post_only {
        return FleetReply::Ack {
            email: t.email,
            seq: t.seq,
            ok: true,
        };
    }
    FleetReply::Reject {
        email: t.email,
        seq: t.seq,
        reason: "unfilled".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn take(size: f64, post_only: bool) -> FleetTake {
        FleetTake {
            email: "a@b.c".into(),
            token: "yes".into(),
            px_ticks: 51,
            size,
            tif: if post_only { "GTC".into() } else { "FAK".into() },
            post_only,
            seq: 7,
        }
    }

    #[test]
    fn fill_uses_taking_amount() {
        let r = reply_from_post(
            take(15.0, false),
            json!({ "success": true, "takingAmount": "5.0", "price": "0.51", "status": "matched" }),
        );
        match r {
            FleetReply::Fill { size, px, seq, .. } => {
                assert!((size - 5.0).abs() < 1e-9);
                assert!((px - 0.51).abs() < 1e-9);
                assert_eq!(seq, 7);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn zero_taking_amount_is_not_a_fill() {
        let r = reply_from_post(
            take(15.0, false),
            json!({ "success": true, "takingAmount": 0, "status": "matched" }),
        );
        match r {
            FleetReply::Reject { reason, .. } => assert_eq!(reason, "unfilled"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn post_only_resting_is_ack_not_fill() {
        let r = reply_from_post(
            take(15.0, true),
            json!({ "success": true, "status": "live" }),
        );
        match r {
            FleetReply::Ack { ok, .. } => assert!(ok),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn success_false_is_reject() {
        let r = reply_from_post(
            take(15.0, false),
            json!({ "success": false, "errorMsg": "not enough balance" }),
        );
        match r {
            FleetReply::Reject { reason, .. } => assert_eq!(reason, "not enough balance"),
            other => panic!("{other:?}"),
        }
    }
}
