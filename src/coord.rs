//! One Ŝ/GTR brain. Per-tenant inventory. Pro-rata FAK. Paper by default.
//! Live intents go to the UK fleet as tenant_id + size + side — no keys.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::Result;
use futures_util::TryStreamExt;
use mm_engine::{
    Command, Engine, EngineConfig, Event, IntentKind, PaperConfig, PaperLedger, PaperVenue,
    Position, TokenRef,
};
use mongodb::bson::{doc, DateTime as BsonDateTime};
use mongodb::{Client, Collection, Database};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::allocator::{allocate, wave_budget, Candidate, MIN_SIZE};
use crate::config::{HOUSE_EMAIL, MIN_BASE, PAPER_CASH};
use crate::db::now_ms;
use crate::mdhub::{self, MdHub};
use crate::models::{FillRow, FleetMsg, FleetTake, Member, Snap, Tick, WindowResult};

pub struct CoordArgs {
    pub mongo_uri: String,
    pub mongo_db: String,
    pub base: String,
    pub mins: u32,
    pub binance: String,
    pub max_wave: f64,
    pub fleet_addr: String,
    pub fleet_token: String,
}

struct Tenant {
    email: String,
    position: Position,
    deficit: u32,
    last_alloc: String,
    paper: bool,
    live: bool,
    plan: String,
    base_amount: f64,
    max_net: f64,
    hedge: bool,
    last_snap_ms: u64,
    seq: u64,
}

fn signer_on_file(m: &Member) -> bool {
    m.keys.as_ref().is_some_and(|k| !k.key_enc.is_empty())
}

impl Tenant {
    fn from_member(m: &Member) -> Self {
        Self {
            email: m.email.clone(),
            position: Position::default(),
            deficit: m.deficit.max(0) as u32,
            last_alloc: m.last_alloc.clone(),
            paper: m.run_flags().paper_running() || (m.live && !signer_on_file(m)),
            live: m.run_flags().live_running() && signer_on_file(m),
            plan: m.plan.clone(),
            base_amount: m.base_amount.max(MIN_BASE),
            max_net: m.max_net.max(m.base_amount.max(MIN_BASE)),
            hedge: m.hedge,
            last_snap_ms: 0,
            seq: 1,
        }
    }
}

fn apply_buy(pos: &mut Position, token: TokenRef, px: f64, size: f64, fee_bps: u32) {
    let fee = size * px * (fee_bps as f64) / 10_000.0;
    pos.add(token, px, size, fee);
}

pub async fn run(args: CoordArgs) -> Result<()> {
    let db: Database = Client::with_uri_str(&args.mongo_uri)
        .await?
        .database(&args.mongo_db);
    let members: Collection<Member> = db.collection("members");
    let snaps: Collection<Snap> = db.collection("snaps");
    let fills: Collection<FillRow> = db.collection("fills");
    let ticks: Collection<Tick> = db.collection("ticks");
    let results: Collection<WindowResult> = db.collection("results");

    let market = crate::quotes::current_market(&args.base, args.mins).await?;
    tracing::info!(slug = %market.slug, k = market.info.k_t, "coordinator market");

    let mut house = Engine::new(EngineConfig::for_mins(args.mins));
    house.on_event(Event::LoadContract(market.info.clone()), now_ms() as u64);
    let mut house_venue = PaperVenue::new(PaperConfig {
        latency_ms: 0,
        taker_delay_ms: Some(0),
        depth: true,
    });
    let mut house_ledger = PaperLedger::default();

    let hub: MdHub = mdhub::spawn(
        &args.base,
        args.mins,
        &args.binance,
        market.yes_id_dec.clone(),
        market.no_id_dec.clone(),
    )
    .await?;
    let mut assets_tx = hub.assets_tx;

    let mut fleet = connect_fleet(&args.fleet_addr, &args.fleet_token).await;
    if let Some(s) = fleet.as_mut() {
        send_msg(
            s,
            &FleetMsg::Hello {
                yes_id: market.yes_id_dec.clone(),
                no_id: market.no_id_dec.clone(),
                tick: market.info.tick_size,
                fee_bps: market.info.fee_bps,
                neg_risk: market.info.neg_risk,
            },
        )
        .await;
    }

    let mut tenants: HashMap<String, Tenant> = HashMap::new();
    let mut last_sync = 0u64;
    let mut last_tick_ms = 0u64;
    let mut last_roll_try = 0u64;
    let mut settled: HashSet<String> = HashSet::new();
    let mut yes_bid_sz = 0.0;
    let mut yes_ask_sz = 0.0;
    tracing::info!("trade-coordinator running (shared MD, pro-rata)");

    loop {
        let dated = match hub.rx.recv_timeout(Duration::from_millis(50)) {
            Ok(d) => d,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                sync_if_due(&members, &mut tenants, &mut last_sync).await;
                let now = now_ms() as u64;
                maybe_roll(
                    &mut house,
                    &mut tenants,
                    &members,
                    &results,
                    &mut assets_tx,
                    &mut settled,
                    &mut last_roll_try,
                    &args.base,
                    args.mins,
                    now,
                )
                .await;
                if now.saturating_sub(last_tick_ms) >= 1_000 {
                    last_tick_ms = now;
                    write_tick(&ticks, &house, now).await;
                    write_snaps(&house, &mut tenants, &snaps, now).await;
                }
                continue;
            }
            Err(_) => break,
        };
        let now = now_ms() as u64;
        let ev = dated.event;
        if let Event::Depth(d) = &ev {
            yes_bid_sz = d.bids[0].size;
            yes_ask_sz = d.asks[0].size;
        }
        let cmds = house.on_event(ev.clone(), now);
        let _ = house.paper_on_market(&mut house_venue, &mut house_ledger, &ev, now);
        let more = house.paper_follow(&mut house_venue, &mut house_ledger, &cmds, now);
        let _ = house.paper_advance(&mut house_venue, &mut house_ledger, now);
        sync_if_due(&members, &mut tenants, &mut last_sync).await;

        let mut taker: Option<(TokenRef, i32, f64)> = None;
        let mut hedge_px: Option<(TokenRef, i32)> = None;
        for c in cmds.iter().chain(more.0.iter()) {
            if let Command::Submit(i) = c {
                match i.kind {
                    IntentKind::Taker => taker = Some((i.token, i.px_ticks, i.size)),
                    IntentKind::Hedge => hedge_px = Some((i.token, i.px_ticks)),
                }
            }
        }

        if let Some((token, px_ticks, _house_sz)) = taker {
            let long = token == TokenRef::Yes;
            let touch = if long { yes_ask_sz } else { yes_bid_sz };
            let budget = wave_budget(touch, args.max_wave);
            let mut cands = Vec::new();
            for t in tenants.values() {
                let net = t.position.net();
                let room = if long {
                    (t.max_net - net).max(0.0)
                } else {
                    (t.max_net + net).max(0.0)
                };
                let requested = room.min(t.base_amount);
                if requested + 1e-9 >= MIN_SIZE {
                    cands.push(Candidate {
                        tenant_id: t.email.clone(),
                        requested,
                        deficit: t.deficit,
                    });
                }
            }
            let alloc = allocate(budget, MIN_SIZE, cands);
            tracing::info!(
                budget,
                shares = alloc.shares.len(),
                missed = alloc.missed.len(),
                "wave"
            );
            let px = px_ticks as f64 / 100.0;
            let fee_bps = house.state.info.as_ref().map(|i| i.fee_bps).unwrap_or(0);
            for m in &alloc.missed {
                if let Some(t) = tenants.get_mut(m) {
                    t.deficit = t.deficit.saturating_add(1);
                    t.last_alloc = "0 / requested (thin book)".into();
                }
            }
            let mut live_takes = Vec::new();
            let mut fill_rows = Vec::new();
            let mut member_sets = Vec::new();
            for sh in &alloc.shares {
                let Some(t) = tenants.get_mut(&sh.tenant_id) else {
                    continue;
                };
                t.deficit = 0;
                t.last_alloc = format!("{:.1} / {:.1}", sh.size, sh.requested);
                apply_buy(&mut t.position, token, px, sh.size, fee_bps);
                fill_rows.push(FillRow {
                    email: t.email.clone(),
                    ts_ms: now as i64,
                    token: if long { "Yes".into() } else { "No".into() },
                    size: sh.size,
                    px,
                    kind: "taker".into(),
                    paper: t.paper || !t.live,
                });
                member_sets.push((
                    t.email.clone(),
                    t.deficit as i32,
                    t.last_alloc.clone(),
                ));
                if t.live {
                    t.seq += 1;
                    live_takes.push(FleetTake {
                        email: t.email.clone(),
                        token: if long { "yes".into() } else { "no".into() },
                        px_ticks,
                        size: sh.size,
                        tif: "FAK".into(),
                        post_only: false,
                        seq: t.seq,
                    });
                }
            }
            send_wave(fleet.as_mut(), live_takes).await;
            for row in fill_rows {
                let _ = fills.insert_one(row, None).await;
            }
            for (email, deficit, last_alloc) in member_sets {
                let _ = members
                    .update_one(
                        doc! { "email": email },
                        doc! { "$set": { "deficit": deficit, "lastAlloc": last_alloc } },
                        None,
                    )
                    .await;
            }
        }

        if let Some((token, px_ticks)) = hedge_px {
            let px = px_ticks as f64 / 100.0;
            let fee_bps = house.state.info.as_ref().map(|i| i.fee_bps).unwrap_or(0);
            let mut live_takes = Vec::new();
            let mut fill_rows = Vec::new();
            for t in tenants.values_mut() {
                if !t.hedge {
                    continue;
                }
                let need = (t.position.net().abs() * 100.0).floor() / 100.0;
                if need < 1.0 {
                    continue;
                }
                let size = need.min(t.base_amount);
                apply_buy(&mut t.position, token, px, size, fee_bps);
                fill_rows.push(FillRow {
                    email: t.email.clone(),
                    ts_ms: now as i64,
                    token: if token == TokenRef::Yes {
                        "Yes".into()
                    } else {
                        "No".into()
                    },
                    size,
                    px,
                    kind: "hedge".into(),
                    paper: t.paper || !t.live,
                });
                if t.live {
                    t.seq += 1;
                    live_takes.push(FleetTake {
                        email: t.email.clone(),
                        token: if token == TokenRef::Yes {
                            "yes".into()
                        } else {
                            "no".into()
                        },
                        px_ticks,
                        size,
                        tif: "GTC".into(),
                        post_only: true,
                        seq: t.seq,
                    });
                }
            }
            send_wave(fleet.as_mut(), live_takes).await;
            for row in fill_rows {
                let _ = fills.insert_one(row, None).await;
            }
        }

        write_snaps(&house, &mut tenants, &snaps, now).await;
        if now.saturating_sub(last_tick_ms) >= 1_000 {
            last_tick_ms = now;
            write_tick(&ticks, &house, now).await;
        }
        maybe_roll(
            &mut house,
            &mut tenants,
            &members,
            &results,
            &mut assets_tx,
            &mut settled,
            &mut last_roll_try,
            &args.base,
            args.mins,
            now,
        )
        .await;
    }
    Ok(())
}

fn complement(px: f64) -> f64 {
    if px > 0.0 {
        (1.0 - px).max(0.0)
    } else {
        0.0
    }
}

fn tick_from_house(hs: &mm_engine::MarketSnap, now: u64) -> Tick {
    Tick {
        slug: hs.slug.clone(),
        ts_ms: now as i64,
        at: BsonDateTime::from_millis(now as i64),
        binance: hs.binance_mid,
        chainlink: hs.chainlink,
        twap: hs.twap,
        s_hat: hs.s_hat,
        yes_bid: hs.yes_bid,
        yes_ask: hs.yes_ask,
        no_bid: complement(hs.yes_ask),
        no_ask: complement(hs.yes_bid),
        tau_s: hs.tau_s,
        quote: hs.quote.to_string(),
        regime: hs.regime.to_string(),
        k_t: hs.k_t,
        fair: hs.fair,
        utc0_ms: hs.utc0_ms as i64,
        utc1_ms: hs.utc1_ms as i64,
    }
}

async fn write_tick(ticks: &Collection<Tick>, house: &Engine, now: u64) {
    let hs = house.market_snap(now);
    if hs.slug.is_empty() {
        return;
    }
    if let Err(e) = ticks.insert_one(tick_from_house(&hs, now), None).await {
        tracing::warn!(error = %e, "tick insert");
    }
}

async fn settle_window(
    house: &mut Engine,
    tenants: &mut HashMap<String, Tenant>,
    members: &Collection<Member>,
    results: &Collection<WindowResult>,
    now: u64,
) {
    let hs = house.market_snap(now);
    let close = if hs.chainlink > 0.0 {
        hs.chainlink
    } else {
        hs.twap
    };
    let up = close > hs.k_t;
    let _ = house.on_event(Event::Settle { close }, now);
    for t in tenants.values_mut() {
        let pnl = t.position.settle_pnl(up);
        let _ = results
            .insert_one(
                WindowResult {
                    email: t.email.clone(),
                    slug: hs.slug.clone(),
                    ts_ms: now as i64,
                    pnl,
                    paper: t.paper,
                    close,
                    k_t: hs.k_t,
                    winner_up: Some(up),
                },
                None,
            )
            .await;
        if t.paper {
            let _ = members
                .update_one(
                    doc! { "email": &t.email, "paperCashUsd": { "$exists": false } },
                    doc! { "$set": { "paperCashUsd": PAPER_CASH } },
                    None,
                )
                .await;
            let _ = members
                .update_one(
                    doc! { "email": &t.email },
                    doc! { "$inc": { "paperCashUsd": pnl } },
                    None,
                )
                .await;
        }
        t.position = Position::default();
    }
    tracing::info!(slug = %hs.slug, close, up, n = tenants.len(), "window settled");
}

async fn maybe_roll(
    house: &mut Engine,
    tenants: &mut HashMap<String, Tenant>,
    members: &Collection<Member>,
    results: &Collection<WindowResult>,
    assets_tx: &mut tokio::sync::watch::Sender<(String, String)>,
    settled: &mut HashSet<String>,
    last_try: &mut u64,
    base: &str,
    mins: u32,
    now: u64,
) {
    let hs = house.market_snap(now);
    if hs.slug.is_empty() || hs.utc1_ms == 0 {
        return;
    }
    if now < hs.utc1_ms {
        return;
    }
    if now.saturating_sub(*last_try) < 3_000 {
        return;
    }
    *last_try = now;
    if !settled.contains(&hs.slug) {
        settle_window(house, tenants, members, results, now).await;
        settled.insert(hs.slug.clone());
    }
    let unix = mm_md::current_bucket_unix_s(mins).await;
    if (unix as u64).saturating_mul(1000) == hs.utc0_ms {
        return;
    }
    match crate::quotes::current_market(base, mins).await {
        Ok(market) => {
            let _ = house.on_event(Event::LoadContract(market.info.clone()), now);
            let _ = assets_tx.send((market.yes_id_dec, market.no_id_dec));
            tracing::info!(slug = %market.slug, k = market.info.k_t, "rolled window");
        }
        Err(e) => tracing::warn!(error = %e, "next contract"),
    }
}

async fn write_snaps(
    house: &Engine,
    tenants: &mut HashMap<String, Tenant>,
    snaps: &Collection<Snap>,
    now: u64,
) {
    let hs = house.market_snap(now);
    let house_doc = Snap {
        email: HOUSE_EMAIL.into(),
        ts_ms: now as i64,
        s_hat: hs.s_hat,
        yes_bid: hs.yes_bid,
        yes_ask: hs.yes_ask,
        net: hs.net,
        mark_pnl: hs.mark_pnl,
        tau_s: hs.tau_s,
        quote: hs.quote.to_string(),
        alloc: "house".into(),
        plan: "house".into(),
        paper: true,
        live: false,
        slug: hs.slug.clone(),
        binance: hs.binance_mid,
        chainlink: hs.chainlink,
        twap: hs.twap,
        no_bid: complement(hs.yes_ask),
        no_ask: complement(hs.yes_bid),
        regime: hs.regime.to_string(),
        k_t: hs.k_t,
        fair: hs.fair,
        utc0_ms: hs.utc0_ms as i64,
        utc1_ms: hs.utc1_ms as i64,
    };
    let upsert = mongodb::options::ReplaceOptions::builder().upsert(true).build();
    let _ = snaps
        .replace_one(doc! { "email": HOUSE_EMAIL }, &house_doc, upsert.clone())
        .await;
    for t in tenants.values_mut() {
        if now.saturating_sub(t.last_snap_ms) < 1_000 {
            continue;
        }
        t.last_snap_ms = now;
        let snap = Snap {
            email: t.email.clone(),
            ts_ms: now as i64,
            s_hat: hs.s_hat,
            yes_bid: hs.yes_bid,
            yes_ask: hs.yes_ask,
            net: t.position.net(),
            mark_pnl: t.position.mark_pnl(hs.book_mid),
            tau_s: hs.tau_s,
            quote: hs.quote.to_string(),
            alloc: t.last_alloc.clone(),
            plan: t.plan.clone(),
            paper: t.paper,
            live: t.live,
            slug: hs.slug.clone(),
            binance: hs.binance_mid,
            chainlink: hs.chainlink,
            twap: hs.twap,
            no_bid: complement(hs.yes_ask),
            no_ask: complement(hs.yes_bid),
            regime: hs.regime.to_string(),
            k_t: hs.k_t,
            fair: hs.fair,
            utc0_ms: hs.utc0_ms as i64,
            utc1_ms: hs.utc1_ms as i64,
        };
        let _ = snaps
            .replace_one(doc! { "email": &t.email }, &snap, upsert.clone())
            .await;
    }
}

async fn sync_if_due(
    col: &Collection<Member>,
    tenants: &mut HashMap<String, Tenant>,
    last_sync: &mut u64,
) {
    let now = now_ms() as u64;
    if now.saturating_sub(*last_sync) <= 2_000 {
        return;
    }
    *last_sync = now;
    if let Err(e) = refresh_tenants(col, tenants).await {
        tracing::warn!(error = %e, "tenant sync");
    }
}

async fn refresh_tenants(
    col: &Collection<Member>,
    tenants: &mut HashMap<String, Tenant>,
) -> Result<()> {
    let now = BsonDateTime::from_millis(now_ms());
    let mut cur = col
        .find(doc! {
            "accessUntil": { "$gt": now },
            "halted": { "$ne": true },
            "blocked": { "$ne": true },
            "$or": [{ "paper": true }, { "live": true }],
        }, None)
        .await?;
    let mut live = Vec::new();
    while let Some(m) = cur.try_next().await? {
        if !m.admin {
            live.push(m);
        }
    }
    let emails: HashSet<_> = live.iter().map(|m| m.email.clone()).collect();
    tenants.retain(|k, _| emails.contains(k));
    for m in live {
        if let Some(t) = tenants.get_mut(&m.email) {
            t.base_amount = m.base_amount.max(MIN_BASE);
            t.max_net = m.max_net.max(t.base_amount);
            t.hedge = m.hedge;
            t.paper = m.run_flags().paper_running() || (m.live && !signer_on_file(&m));
            t.live = m.run_flags().live_running() && signer_on_file(&m);
            t.plan = m.plan;
        } else {
            tenants.insert(m.email.clone(), Tenant::from_member(&m));
        }
        if m.paper_cash_usd.is_none() {
            let _ = col
                .update_one(
                    doc! { "email": &m.email, "paperCashUsd": { "$exists": false } },
                    doc! { "$set": { "paperCashUsd": PAPER_CASH } },
                    None,
                )
                .await;
        }
    }
    Ok(())
}

async fn connect_fleet(addr: &str, token: &str) -> Option<TcpStream> {
    if addr.is_empty() {
        return None;
    }
    match TcpStream::connect(addr).await {
        Ok(mut s) => {
            if !token.is_empty() {
                send_msg(&mut s, &FleetMsg::Auth { token: token.into() }).await;
            }
            tracing::info!(%addr, "fleet connected");
            Some(s)
        }
        Err(e) => {
            tracing::warn!(error=%e, "fleet offline — paper only");
            None
        }
    }
}

async fn send_msg(s: &mut TcpStream, msg: &FleetMsg) {
    if let Ok(line) = serde_json::to_string(msg) {
        let mut buf = line.into_bytes();
        buf.push(b'\n');
        let _ = s.write_all(&buf).await;
    }
}

async fn send_wave(fleet: Option<&mut TcpStream>, takes: Vec<FleetTake>) {
    if takes.is_empty() {
        return;
    }
    if let Some(s) = fleet {
        tracing::info!(n = takes.len(), "live wave fan-out");
        send_msg(s, &FleetMsg::Wave { takes }).await;
    }
}
