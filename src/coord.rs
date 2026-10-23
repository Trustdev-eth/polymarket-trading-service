//! One Ŝ/GTR brain. Per-tenant inventory. Pro-rata FAK. Paper by default.
//! Live intents go to the UK fleet as tenant_id + size + side — no keys.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::TryStreamExt;
use mm_engine::{
    Command, Engine, EngineConfig, Event, IntentKind, PaperConfig, PaperLedger, PaperVenue,
    Position, TokenRef,
};
use mongodb::bson::{doc, DateTime as BsonDateTime};
use mongodb::{Client, Collection, Database};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

use crate::allocator::{allocate, wave_budget, Candidate, MIN_SIZE};
use crate::config::{HOUSE_EMAIL, MIN_BASE, PAPER_CASH};
use crate::db::now_ms;
use crate::mdhub::{self, MdHub};
use crate::models::{FillRow, FleetMsg, FleetReply, FleetTake, Member, Snap, Tick, WindowResult};

struct FleetConn {
    w: OwnedWriteHalf,
    r: Lines<BufReader<OwnedReadHalf>>,
}

struct FleetHello {
    yes_id: String,
    no_id: String,
    tick: f64,
    fee_bps: u32,
    neg_risk: bool,
}

impl FleetHello {
    fn from_market(market: &mm_md::ResolvedMarket) -> Self {
        Self {
            yes_id: market.yes_id_dec.clone(),
            no_id: market.no_id_dec.clone(),
            tick: market.info.tick_size,
            fee_bps: market.info.fee_bps,
            neg_risk: market.info.neg_risk,
        }
    }

    fn msg(&self) -> FleetMsg {
        FleetMsg::Hello {
            yes_id: self.yes_id.clone(),
            no_id: self.no_id.clone(),
            tick: self.tick,
            fee_bps: self.fee_bps,
            neg_risk: self.neg_risk,
        }
    }
}

struct LivePending {
    email: String,
    token: TokenRef,
    requested: f64,
    px: f64,
    kind: &'static str,
}

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

/// `MAX_NET=0` is uncapped. Never lift a zero cap up to BASE.
fn net_cap(max_net: f64, base: f64) -> f64 {
    if max_net <= 0.0 {
        0.0
    } else {
        max_net.max(base.max(MIN_BASE))
    }
}

fn take_size(max_net: f64, net: f64, long: bool, base: f64) -> f64 {
    if max_net <= 0.0 {
        return base;
    }
    let room = if long {
        (max_net - net).max(0.0)
    } else {
        (max_net + net).max(0.0)
    };
    room.min(base)
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
            max_net: net_cap(m.max_net, m.base_amount),
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

fn fill_slug(house: &Engine) -> String {
    house
        .state
        .info
        .as_ref()
        .map(|i| i.slug.clone())
        .unwrap_or_default()
}

fn slug_unix_s(slug: &str) -> Option<i64> {
    slug.rsplit('-').next()?.parse().ok()
}

fn yes_mid(hs: &mm_engine::MarketSnap) -> f64 {
    if hs.book_mid > 0.0 {
        hs.book_mid
    } else if hs.yes_bid > 0.0 && hs.yes_ask >= hs.yes_bid {
        0.5 * (hs.yes_bid + hs.yes_ask)
    } else {
        0.0
    }
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

    let mut hello = FleetHello::from_market(&market);
    let mut fleet = connect_fleet(&args.fleet_addr, &args.fleet_token, &hello).await;
    let mut last_fleet_try = now_ms() as u64;

    let mut tenants: HashMap<String, Tenant> = HashMap::new();
    let _ = refresh_tenants(&members, &mut tenants).await;
    seed_positions(&fills, &mut tenants, &market.slug).await;
    let mut last_sync = now_ms() as u64;
    let mut last_tick_ms = 0u64;
    let mut last_roll_try = 0u64;
    let mut settled: HashSet<String> = HashSet::new();
    let mut yes_bid_sz = 0.0;
    let mut yes_ask_sz = 0.0;
    let mut n_ev = 0u32;
    tracing::info!("trade-coordinator running (shared MD, pro-rata)");

    loop {
        // try_recv + sleep: never park a Tokio worker on a blocking recv.
        let dated = match hub.rx.try_recv() {
            Ok(d) => d,
            Err(crossbeam_channel::TryRecvError::Empty) => {
                sync_if_due(&members, &mut tenants, &mut last_sync).await;
                house.cfg.hedge = tenants.values().any(|t| t.hedge);
                let now = now_ms() as u64;
                ensure_fleet(
                    &mut fleet,
                    &mut last_fleet_try,
                    now,
                    &args.fleet_addr,
                    &args.fleet_token,
                    &hello,
                )
                .await;
                maybe_roll(
                    &mut house,
                    &mut tenants,
                    &members,
                    &results,
                    &mut assets_tx,
                    &mut settled,
                    &mut last_roll_try,
                    &mut fleet,
                    &mut hello,
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
                tokio::time::sleep(Duration::from_millis(20)).await;
                continue;
            }
            Err(_) => break,
        };
        n_ev = n_ev.wrapping_add(1);
        if n_ev % 64 == 0 {
            tokio::task::yield_now().await;
        }
        let now = now_ms() as u64;
        ensure_fleet(
            &mut fleet,
            &mut last_fleet_try,
            now,
            &args.fleet_addr,
            &args.fleet_token,
            &hello,
        )
        .await;
        let ev = dated.event;
        if let Event::Depth(d) = &ev {
            yes_bid_sz = d.bids[0].size;
            yes_ask_sz = d.asks[0].size;
        }
        house.cfg.hedge = tenants.values().any(|t| t.hedge);
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
                let requested = take_size(t.max_net, t.position.net(), long, t.base_amount);
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
            let mut pending: HashMap<(String, u64), LivePending> = HashMap::new();
            let mut fill_rows = Vec::new();
            let mut member_sets = Vec::new();
            for sh in &alloc.shares {
                let Some(t) = tenants.get_mut(&sh.tenant_id) else {
                    continue;
                };
                t.deficit = 0;
                t.last_alloc = format!("{:.1} / {:.1}", sh.size, sh.requested);
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
                    pending.insert(
                        (t.email.clone(), t.seq),
                        LivePending {
                            email: t.email.clone(),
                            token,
                            requested: sh.size,
                            px,
                            kind: "taker",
                        },
                    );
                } else {
                    apply_buy(&mut t.position, token, px, sh.size, fee_bps);
                    fill_rows.push(FillRow {
                        email: t.email.clone(),
                        ts_ms: now as i64,
                        token: if long { "Yes".into() } else { "No".into() },
                        size: sh.size,
                        px,
                        kind: "taker".into(),
                        paper: true,
                        slug: fill_slug(&house),
                    });
                    member_sets.push((
                        t.email.clone(),
                        t.deficit as i32,
                        t.last_alloc.clone(),
                    ));
                }
            }
            let replies = send_wave(&mut fleet, live_takes).await;
            book_live_replies(
                &mut tenants,
                &fills,
                &members,
                pending,
                replies,
                fee_bps,
                now,
                true,
                &fill_slug(&house),
            )
            .await;
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
            let mut pending: HashMap<(String, u64), LivePending> = HashMap::new();
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
                    pending.insert(
                        (t.email.clone(), t.seq),
                        LivePending {
                            email: t.email.clone(),
                            token,
                            requested: size,
                            px,
                            kind: "hedge",
                        },
                    );
                } else {
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
                        paper: true,
                        slug: fill_slug(&house),
                    });
                }
            }
            let replies = send_wave(&mut fleet, live_takes).await;
            book_live_replies(
                &mut tenants,
                &fills,
                &members,
                pending,
                replies,
                fee_bps,
                now,
                false,
                &fill_slug(&house),
            )
            .await;
            for row in fill_rows {
                let _ = fills.insert_one(row, None).await;
            }
        }

        if now.saturating_sub(last_tick_ms) >= 1_000 {
            last_tick_ms = now;
            write_tick(&ticks, &house, now).await;
            write_snaps(&house, &mut tenants, &snaps, now).await;
        }
        maybe_roll(
            &mut house,
            &mut tenants,
            &members,
            &results,
            &mut assets_tx,
            &mut settled,
            &mut last_roll_try,
            &mut fleet,
            &mut hello,
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
    let close = if house.state.settle_twap > 0.0 {
        house.state.settle_twap
    } else if hs.chainlink > 0.0 {
        hs.chainlink
    } else {
        hs.twap
    };
    let up = close > hs.k_t;
    let _ = house.on_event(Event::Settle { close }, now);
    for t in tenants.values_mut() {
        let already = results
            .find_one(doc! { "email": &t.email, "slug": &hs.slug }, None)
            .await
            .ok()
            .flatten()
            .is_some();
        if already {
            t.position = Position::default();
            continue;
        }
        if t.position.long_qty.abs() <= 1e-12 && t.position.short_qty.abs() <= 1e-12 {
            t.position = Position::default();
            continue;
        }
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
    fleet: &mut Option<FleetConn>,
    hello: &mut FleetHello,
    base: &str,
    mins: u32,
    now: u64,
) {
    let hs = house.market_snap(now);
    if hs.slug.is_empty() {
        return;
    }
    let unix = mm_md::current_bucket_unix_s(mins).await;
    let window_over = match slug_unix_s(&hs.slug) {
        Some(u) => u != unix,
        None => hs.utc1_ms > 0 && now >= hs.utc1_ms,
    };
    if !window_over {
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
    match tokio::time::timeout(
        Duration::from_secs(6),
        crate::quotes::current_market(base, mins),
    )
    .await
    {
        Ok(Ok(market)) => {
            if market.slug == hs.slug {
                return;
            }
            let _ = house.on_event(Event::LoadContract(market.info.clone()), now);
            let _ = assets_tx.send((market.yes_id_dec.clone(), market.no_id_dec.clone()));
            *hello = FleetHello::from_market(&market);
            if let Some(c) = fleet.as_mut() {
                if !send_msg(c, &hello.msg()).await {
                    *fleet = None;
                }
            }
            tracing::info!(slug = %market.slug, k = market.info.k_t, "rolled window");
        }
        Ok(Err(e)) => tracing::warn!(error = %e, "next contract"),
        Err(_) => tracing::warn!("next contract timed out"),
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
            mark_pnl: t.position.mark_pnl(yes_mid(&hs)),
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

async fn seed_positions(
    fills: &Collection<FillRow>,
    tenants: &mut HashMap<String, Tenant>,
    slug: &str,
) {
    if slug.is_empty() || tenants.is_empty() {
        return;
    }
    let mut cur = match fills.find(doc! { "slug": slug }, None).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "seed fills");
            return;
        }
    };
    while let Ok(Some(f)) = cur.try_next().await {
        let Some(t) = tenants.get_mut(&f.email) else {
            continue;
        };
        if t.live && f.paper {
            continue;
        }
        if t.paper && !f.paper {
            continue;
        }
        let token = if f.token.eq_ignore_ascii_case("yes") || f.token.eq_ignore_ascii_case("up") {
            TokenRef::Yes
        } else {
            TokenRef::No
        };
        t.position.add(token, f.px, f.size, 0.0);
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
            t.max_net = net_cap(m.max_net, t.base_amount);
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

async fn connect_fleet(addr: &str, token: &str, hello: &FleetHello) -> Option<FleetConn> {
    if addr.is_empty() {
        return None;
    }
    match TcpStream::connect(addr).await {
        Ok(s) => {
            let _ = s.set_nodelay(true);
            let (r, w) = s.into_split();
            let mut c = FleetConn {
                w,
                r: BufReader::new(r).lines(),
            };
            if !token.is_empty() && !send_msg(&mut c, &FleetMsg::Auth { token: token.into() }).await
            {
                return None;
            }
            if !send_msg(&mut c, &hello.msg()).await {
                return None;
            }
            tracing::info!(%addr, "fleet connected");
            Some(c)
        }
        Err(e) => {
            tracing::warn!(error=%e, "fleet offline — paper only");
            None
        }
    }
}

async fn ensure_fleet(
    fleet: &mut Option<FleetConn>,
    last_try: &mut u64,
    now: u64,
    addr: &str,
    token: &str,
    hello: &FleetHello,
) {
    if fleet.is_some() {
        return;
    }
    if now.saturating_sub(*last_try) < 3_000 {
        return;
    }
    *last_try = now;
    *fleet = connect_fleet(addr, token, hello).await;
}

async fn send_msg(c: &mut FleetConn, msg: &FleetMsg) -> bool {
    let Ok(line) = serde_json::to_string(msg) else {
        return false;
    };
    let mut buf = line.into_bytes();
    buf.push(b'\n');
    if c.w.write_all(&buf).await.is_err() {
        tracing::warn!("fleet write failed");
        return false;
    }
    true
}

async fn send_wave(fleet: &mut Option<FleetConn>, takes: Vec<FleetTake>) -> Vec<FleetReply> {
    let n = takes.len();
    if n == 0 {
        return Vec::new();
    }
    let Some(c) = fleet.as_mut() else {
        tracing::warn!(n, "fleet down — skip live wave");
        return Vec::new();
    };
    tracing::info!(n, "live wave fan-out");
    if !send_msg(c, &FleetMsg::Wave { takes }).await {
        *fleet = None;
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n);
    let deadline = Instant::now() + Duration::from_secs(12);
    for _ in 0..n {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            tracing::warn!("fleet reply deadline");
            *fleet = None;
            break;
        }
        match tokio::time::timeout(left.min(Duration::from_secs(8)), c.r.next_line()).await {
            Ok(Ok(Some(line))) => match serde_json::from_str::<FleetReply>(&line) {
                Ok(r) => out.push(r),
                Err(e) => tracing::warn!(error=%e, "bad fleet reply"),
            },
            Ok(Ok(None)) => {
                tracing::warn!("fleet closed");
                *fleet = None;
                break;
            }
            Ok(Err(e)) => {
                tracing::warn!(error=%e, "fleet read");
                *fleet = None;
                break;
            }
            Err(_) => {
                tracing::warn!("fleet reply timeout");
                *fleet = None;
                break;
            }
        }
    }
    out
}

async fn book_live_replies(
    tenants: &mut HashMap<String, Tenant>,
    fills: &Collection<FillRow>,
    members: &Collection<Member>,
    mut pending: HashMap<(String, u64), LivePending>,
    replies: Vec<FleetReply>,
    fee_bps: u32,
    now: u64,
    bump_deficit_on_miss: bool,
    slug: &str,
) {
    for r in replies {
        match r {
            FleetReply::Fill {
                email,
                seq,
                size,
                px,
            } => {
                let key = (email.clone(), seq);
                let Some(p) = pending.remove(&key) else {
                    continue;
                };
                if size <= 1e-9 {
                    miss_live(tenants, members, &p, "unfilled", bump_deficit_on_miss).await;
                    continue;
                }
                let Some(t) = tenants.get_mut(&email) else {
                    continue;
                };
                let fill_px = if px > 1e-9 { px } else { p.px };
                apply_buy(&mut t.position, p.token, fill_px, size, fee_bps);
                t.last_alloc = format!("{:.1} / {:.1}", size, p.requested);
                let last_alloc = t.last_alloc.clone();
                let _ = fills
                    .insert_one(
                        FillRow {
                            email: email.clone(),
                            ts_ms: now as i64,
                            token: if p.token == TokenRef::Yes {
                                "Yes".into()
                            } else {
                                "No".into()
                            },
                            size,
                            px: fill_px,
                            kind: p.kind.into(),
                            paper: false,
                            slug: slug.to_string(),
                        },
                        None,
                    )
                    .await;
                let _ = members
                    .update_one(
                        doc! { "email": email },
                        doc! { "$set": { "lastAlloc": last_alloc, "deficit": 0 } },
                        None,
                    )
                    .await;
            }
            FleetReply::Ack { email, seq, ok } => {
                let key = (email, seq);
                if let Some(p) = pending.remove(&key) {
                    if !ok || bump_deficit_on_miss {
                        miss_live(
                            tenants,
                            members,
                            &p,
                            if ok { "unfilled" } else { "nack" },
                            bump_deficit_on_miss,
                        )
                        .await;
                    }
                }
            }
            FleetReply::Reject {
                email,
                seq,
                reason,
            } => {
                let key = (email, seq);
                if let Some(p) = pending.remove(&key) {
                    miss_live(tenants, members, &p, &reason, bump_deficit_on_miss).await;
                }
            }
        }
    }
    for p in pending.into_values() {
        miss_live(tenants, members, &p, "no-reply", bump_deficit_on_miss).await;
    }
}

async fn miss_live(
    tenants: &mut HashMap<String, Tenant>,
    members: &Collection<Member>,
    p: &LivePending,
    reason: &str,
    bump_deficit: bool,
) {
    let Some(t) = tenants.get_mut(&p.email) else {
        return;
    };
    t.last_alloc = format!("0 / {:.1} ({reason})", p.requested);
    if bump_deficit {
        t.deficit = t.deficit.saturating_add(1);
    }
    let last_alloc = t.last_alloc.clone();
    let deficit = t.deficit as i32;
    let _ = members
        .update_one(
            doc! { "email": &p.email },
            doc! { "$set": { "deficit": deficit, "lastAlloc": last_alloc } },
            None,
        )
        .await;
}
