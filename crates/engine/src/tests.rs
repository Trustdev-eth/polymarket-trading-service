use crate::bands::Band;
use crate::config::EngineConfig;
use crate::engine::Engine;
use crate::types::*;

const UTC0: u64 = 1_800_000;
const UTC1: u64 = UTC0 + 900_000;
const NOW: u64 = UTC0 + 60_000;

fn contract(k: f64) -> ContractInfo {
    ContractInfo {
        utc0_ms: UTC0,
        utc1_ms: UTC1,
        k_t: k,
        fee_bps: 0,
        slug: "btc-updown-15m-test".into(),
        tick_size: 0.01,
        neg_risk: false,
        taker_delay_ms: 0,
    }
}

/// Engine mid-window with Ŝ flat at `s_hat`, book 0.49 / 0.51 and trading allowed.
fn ready(s_hat: f64) -> Engine {
    let mut eng = Engine::new(EngineConfig::default());
    eng.on_event(Event::LoadContract(contract(100.0)), UTC0);
    for t in (0..NOW - UTC0).step_by(50) {
        eng.oracle_bins.write(UTC0 + t, 100.0);
        eng.fast_bins.write(UTC0 + t, 100.0);
    }
    let s = &mut eng.state;
    s.s_hat = s_hat;
    s.fast_mid = s_hat;
    s.binance_mid = s_hat;
    s.bba_ts_ms = NOW;
    s.bba_from_binance = true;
    s.twap = 100.0;
    s.chainlink = 100.0;
    s.allow = AllowTrade::Ready;
    s.yes_bid = 0.49;
    s.yes_ask = 0.51;
    s.book_mid = 0.50;
    s.book_ts_ms = NOW;
    s.chainlink_ts_ms = NOW;
    eng
}

fn ready_hedge(s_hat: f64) -> Engine {
    let mut eng = ready(s_hat);
    eng.cfg.hedge = true;
    eng
}

fn band(nd2: f64) -> Band {
    Band {
        mid: 0.50,
        ref_min: 100.0,
        ref_max: 100.0,
        likely_ref: 100.0,
        nd2_scaled_min: nd2,
        nd2_scaled_max: nd2,
        expires_ms: NOW + 3_000,
    }
}

fn run(eng: &mut Engine, now: u64) -> Vec<Command> {
    eng.state.bba_ts_ms = now;
    let mut out = Vec::new();
    eng.policy(now, &mut out);
    out
}

fn submits(out: &[Command], kind: IntentKind) -> Vec<OrderIntent> {
    out.iter()
        .filter_map(|c| match c {
            Command::Submit(i) if i.kind == kind => Some(*i),
            _ => None,
        })
        .collect()
}

#[test]
fn course_defaults() {
    let cfg = EngineConfig::for_mins(15);
    assert_eq!((cfg.no_quote_s, cfg.no_hedge_s), (40.0, 15.0));
    assert_eq!((cfg.fiber_trip_ms, cfg.order_serve_ms), (90, 25));
    assert_eq!(cfg.book_ingest_ms(), cfg.fiber_trip_ms + cfg.order_serve_ms + 10);
    assert_eq!(cfg.taker_bump_ms, 250);
    assert_eq!(cfg.hedge_tick_ms, 500);
    assert_eq!(cfg.var_sample_ticks, 100);
    assert!(!cfg.hedge);
    assert_eq!(cfg.max_net, 0.0);
    assert_eq!(cfg.net_cap(), f64::INFINITY);
    assert_eq!((cfg.px_lo, cfg.px_hi), (1, 99));
    assert_eq!(cfg.book_stale_ms, 3_000);
}

#[test]
fn load_contract_waits_for_book_then_cools_down() {
    let mut eng = Engine::new(EngineConfig::default());
    assert!(eng.on_event(Event::LoadContract(contract(100.0)), UTC0).is_empty());
    assert_eq!(eng.state.allow, AllowTrade::WaitingBook);
    eng.on_event(Event::Book { ts_ms: NOW, yes_bid: 0.49, yes_ask: 0.51 }, NOW);
    assert!(matches!(eng.state.allow, AllowTrade::Cooldown(_)));
    eng.on_event(Event::Book { ts_ms: NOW + 6_000, yes_bid: 0.48, yes_ask: 0.52 }, NOW + 6_000);
    assert_eq!(eng.state.allow, AllowTrade::Ready);
}

#[test]
fn roll_cancels_all_and_resets_position() {
    let mut eng = ready(100.0);
    eng.state.position.add(TokenRef::Yes, 0.5, 5.0, 0.0);
    eng.state.halt_until_ms = UTC1 + 10_000;
    let mut next = contract(100.0);
    next.utc0_ms = UTC1;
    next.utc1_ms = UTC1 + 900_000;
    let out = eng.on_event(Event::LoadContract(next), UTC1);
    assert!(out.contains(&Command::CancelAll { reason: CancelReason::Halt }));
    assert_eq!(eng.state.position.net(), 0.0);
    assert_eq!(eng.state.allow, AllowTrade::WaitingBook);
    assert_eq!(eng.state.halt_until_ms, 0);
}

#[test]
fn chainlink_fallback_does_not_print_as_binance() {
    let mut eng = ready(101.0);
    eng.state.bba_from_binance = false;
    eng.state.bba_ts_ms = NOW - 10_000;
    eng.on_event(Event::Chainlink { ts_ms: NOW, value: 99.0 }, NOW);
    assert!((eng.state.binance_mid - 101.0).abs() < 1e-9);
    assert!((eng.market_snap(NOW).binance_mid - 101.0).abs() < 1e-9);
}

#[test]
fn bba_stale_halts() {
    let mut eng = ready(100.0);
    eng.state.bba_ts_ms = NOW - 10_000;
    eng.state.chainlink_ts_ms = NOW - 10_000;
    let out = eng.on_event(Event::InvariantTick, NOW);
    assert!(out.iter().any(|c| matches!(c, Command::Halt { reason: HaltReason::BbaStale, .. })));
    assert!(eng.state.halt_until_ms > NOW);
}

#[test]
fn fill_books_price_and_fee() {
    let mut eng = ready(100.0);
    eng.on_event(
        Event::Fill {
            client_seq: 0,
            token: TokenRef::Yes,
            px: 0.5,
            size: 5.0,
            fee: 0.1,
            is_taker: true,
        },
        NOW,
    );
    assert_eq!(eng.state.position.long_qty, 0.0, "unknown fills are ignored");
    eng.on_event(
        Event::Fill {
            client_seq: 7,
            token: TokenRef::No,
            px: 0.43,
            size: 2.5,
            fee: 0.04,
            is_taker: true,
        },
        NOW,
    );
    let p = eng.state.position;
    assert_eq!(p.short_qty, 2.5);
    assert!((p.short_cost - 1.075).abs() < 1e-12);
    assert!((p.fees - 0.04).abs() < 1e-12);
}

#[test]
fn taker_lifts_ask_at_touch_when_under_fak_minus_fee() {
    let mut eng = ready(100.02);
    eng.state.yes_ask = 0.52;
    eng.bands.push(band(700.0));
    let takes = submits(&run(&mut eng, NOW), IntentKind::Taker);
    assert_eq!(takes.len(), 1);
    let t = takes[0];
    assert_eq!((t.token, t.px_ticks, t.tif), (TokenRef::Yes, 52, Tif::Fak));
    assert!(!t.post_only);
}

#[test]
fn taker_skips_ask_above_fak_minus_fee() {
    // fak_long = 0.57, lecture fee ~1.48c → cap 55 < touch 58.
    let mut eng = ready(100.02);
    eng.state.yes_ask = 0.58;
    eng.bands.push(band(700.0));
    assert!(submits(&run(&mut eng, NOW), IntentKind::Taker).is_empty());
    let k = eng.state.skip.expect("skip recorded");
    assert_eq!((k.token, k.touch, k.cap), (TokenRef::Yes, 58, 55));
}

#[test]
fn taker_sells_through_no_at_touch() {
    let mut eng = ready(99.98);
    eng.state.yes_bid = 0.48;
    eng.bands.push(band(700.0));
    let takes = submits(&run(&mut eng, NOW), IntentKind::Taker);
    assert_eq!(takes.len(), 1);
    assert_eq!((takes[0].token, takes[0].px_ticks), (TokenRef::No, 52));
}

#[test]
fn taker_respects_retry_dedup_and_max_net() {
    let mut eng = ready(100.02);
    eng.state.yes_ask = 0.52;
    eng.bands.push(band(700.0));
    let first = submits(&run(&mut eng, NOW), IntentKind::Taker)[0];
    let mut out = Vec::new();
    eng.oms.on_ack(first.client_seq, NOW + 5, &mut out);
    assert!(submits(&run(&mut eng, NOW + 100), IntentKind::Taker).is_empty(), "retry cooldown");
    assert!(submits(&run(&mut eng, NOW + 400), IntentKind::Taker).is_empty(), "same price is deduped");

    eng.state.last_take_p = 0;
    eng.cfg.max_net = 15.0;
    eng.state.position.add(TokenRef::Yes, 0.52, eng.cfg.max_net, 0.0);
    assert!(submits(&run(&mut eng, NOW + 800), IntentKind::Taker).is_empty(), "max_net gate");
}

#[test]
fn max_net_zero_does_not_cap() {
    let mut eng = ready(100.02);
    eng.cfg.max_net = 0.0;
    eng.state.yes_ask = 0.52;
    eng.bands.push(band(700.0));
    eng.state.position.add(TokenRef::Yes, 0.52, 50.0, 0.0);
    let takes = submits(&run(&mut eng, NOW), IntentKind::Taker);
    assert_eq!(takes.len(), 1);
    assert_eq!(takes[0].size, eng.cfg.base_amount);
}

#[test]
fn hedge_off_skips_and_cancels_resting() {
    let mut eng = ready_hedge(100.0);
    eng.bands.push(band(700.0));
    eng.state.position.add(TokenRef::Yes, 0.5, 5.0, 0.0);
    let h = submits(&run(&mut eng, NOW), IntentKind::Hedge)[0];
    let mut out = Vec::new();
    eng.oms.on_ack(h.client_seq, NOW + 5, &mut out);

    eng.cfg.hedge = false;
    let out = run(&mut eng, NOW + 500);
    assert_eq!(out, vec![Command::Cancel { client_seq: h.client_seq, reason: CancelReason::Halt }]);
    let mut ack = Vec::new();
    eng.oms.on_ack(h.client_seq, NOW + 600, &mut ack);
    assert!(submits(&run(&mut eng, NOW + 1_000), IntentKind::Hedge).is_empty());
}

#[test]
fn hedge_one_in_flight_sized_to_net_at_cadence() {
    let mut eng = ready_hedge(100.0);
    eng.bands.push(band(700.0));
    eng.state.position.add(TokenRef::Yes, 0.5, 3.0, 0.0);

    let h = submits(&run(&mut eng, NOW), IntentKind::Hedge);
    assert_eq!(h.len(), 1);
    assert_eq!((h[0].token, h[0].tif, h[0].size), (TokenRef::No, Tif::Gtc, 3.0));
    // lecture fee 1.5625c + inventory −0.18c → ceil(0.5138)=52 → NO 48.
    assert_eq!(h[0].px_ticks, 48);

    for t in [100, 600, 1_100] {
        assert!(submits(&run(&mut eng, NOW + t), IntentKind::Hedge).is_empty(), "pending ack at +{t}ms");
    }
    let mut out = Vec::new();
    eng.oms.on_ack(h[0].client_seq, NOW + 1_200, &mut out);
    assert!(run(&mut eng, NOW + 1_700).is_empty(), "same hedge is kept");
}

#[test]
fn hedge_never_crosses_past_touch() {
    let mut eng = ready_hedge(100.0);
    eng.bands.push(band(700.0));
    eng.state.position.add(TokenRef::No, 0.5, 50.0, 0.0);
    let h = submits(&run(&mut eng, NOW), IntentKind::Hedge);
    assert_eq!((h[0].token, h[0].px_ticks, h[0].size, h[0].post_only), (TokenRef::Yes, 50, 5.0, true));
}

#[test]
fn hedge_replaces_on_price_change_and_cancels_when_flat() {
    let mut eng = ready_hedge(100.0);
    eng.bands.push(band(700.0));
    eng.state.position.add(TokenRef::Yes, 0.5, 5.0, 0.0);
    let h = submits(&run(&mut eng, NOW), IntentKind::Hedge)[0];
    let mut out = Vec::new();
    eng.oms.on_ack(h.client_seq, NOW + 5, &mut out);

    eng.state.yes_bid = 0.55;
    eng.state.yes_ask = 0.57;
    let out = run(&mut eng, NOW + 500);
    assert_eq!(out, vec![Command::Cancel { client_seq: h.client_seq, reason: CancelReason::Replace }]);
    assert!(run(&mut eng, NOW + 1_000).is_empty(), "wait for cancel ack");
    let mut out = Vec::new();
    eng.oms.on_ack(h.client_seq, NOW + 1_100, &mut out);
    let h2 = submits(&run(&mut eng, NOW + 1_500), IntentKind::Hedge);
    // ceil(0.50)+touch floor 0.55+1 → max(50, 56)=56 → NO 44.
    assert_eq!((h2[0].token, h2[0].px_ticks), (TokenRef::No, 44));

    eng.oms.on_ack(h2[0].client_seq, NOW + 1_600, &mut out);
    eng.on_event(
        Event::Fill {
            client_seq: h2[0].client_seq,
            token: TokenRef::No,
            px: 0.44,
            size: 5.0,
            fee: 0.0,
            is_taker: false,
        },
        NOW + 1_700,
    );
    assert!(run(&mut eng, NOW + 2_000).is_empty(), "flat: nothing to hedge");
}

#[test]
fn lock_in_cancels_everything() {
    let mut eng = ready_hedge(100.0);
    eng.bands.push(band(700.0));
    eng.state.position.add(TokenRef::Yes, 0.5, 5.0, 0.0);
    let h = submits(&run(&mut eng, NOW), IntentKind::Hedge)[0];
    let mut out = Vec::new();
    eng.oms.on_ack(h.client_seq, NOW + 5, &mut out);
    let late = UTC1 - 10_000;
    let out = eng.on_event(Event::BinanceBba { ts_exch_ms: late, bid: 100.1, ask: 100.3 }, late);
    assert!(out.iter().any(|c| matches!(c, Command::Cancel { .. })));
    assert!(!out.iter().any(|c| matches!(c, Command::Submit(_))));
}

#[test]
fn no_takes_inside_no_quote_but_hedges_continue() {
    let mut eng = ready_hedge(100.02);
    eng.state.yes_ask = 0.52;
    eng.bands.push(Band { expires_ms: UTC1, ..band(700.0) });
    eng.state.position.add(TokenRef::No, 0.5, 5.0, 0.0);
    let now = UTC1 - 30_000;
    eng.state.bba_ts_ms = now;
    let out = run(&mut eng, now);
    assert!(submits(&out, IntentKind::Taker).is_empty());
    assert_eq!(submits(&out, IntentKind::Hedge).len(), 1);
}

#[test]
fn deterministic_replay() {
    let events = [
        Event::LoadContract(contract(100.0)),
        Event::BinanceBba { ts_exch_ms: NOW - 2_000, bid: 99.9, ask: 100.1 },
        Event::Book { ts_ms: NOW - 1_000, yes_bid: 0.49, yes_ask: 0.51 },
        Event::BinanceBba { ts_exch_ms: NOW, bid: 100.0, ask: 100.2 },
        Event::Book { ts_ms: NOW + 6_000, yes_bid: 0.48, yes_ask: 0.52 },
    ];
    let replay = || {
        let mut eng = Engine::new(EngineConfig::default());
        let mut all = Vec::new();
        for (i, e) in events.iter().enumerate() {
            all.extend(eng.on_event(e.clone(), NOW + i as u64 * 1_000));
        }
        (all, eng.state.s_hat)
    };
    assert_eq!(replay(), replay());
}

#[test]
fn market_result_includes_fees() {
    let mut eng = ready(100.0);
    eng.state.position.add(TokenRef::Yes, 0.40, 5.0, 0.1);
    eng.state.settle_close = 101.0;
    let r = eng.market_result();
    assert_eq!(r.winner_up, Some(true));
    assert!((r.pnl.unwrap() - (5.0 - 2.0 - 0.1)).abs() < 1e-12);
}

#[test]
fn twap_near_open_latches_strike() {
    let mut eng = Engine::new(EngineConfig::default());
    eng.on_event(Event::LoadContract(contract(0.0)), UTC0);
    eng.on_event(Event::Twap { ts_ms: UTC0 + 1_000, value: 64_000.0 }, UTC0 + 1_000);
    assert_eq!(eng.state.k_t(), 64_000.0);
    eng.on_event(Event::Twap { ts_ms: UTC0 + 60_000, value: 65_000.0 }, UTC0 + 60_000);
    assert_eq!(eng.state.k_t(), 64_000.0);
}

#[test]
fn chainlink_owns_shift_over_twap() {
    let mut eng = Engine::new(EngineConfig::default());
    eng.on_event(Event::LoadContract(contract(100.0)), UTC0);
    eng.on_event(Event::BinanceBba { ts_exch_ms: UTC0, bid: 100.10, ask: 100.12 }, UTC0);
    eng.on_event(Event::Chainlink { ts_ms: UTC0, value: 100.00 }, UTC0);
    let after_cl = eng.state.shift_ewma;
    eng.on_event(Event::Twap { ts_ms: UTC0, value: 99.00 }, UTC0);
    assert_eq!(eng.state.shift_ewma, after_cl, "TWAP must not overwrite Chainlink μ");
}

#[test]
fn unchanged_book_keeps_old_band_so_binance_move_takes() {
    let mut eng = ready(100.0);
    eng.state.book_ts_ms = NOW - 1_000;
    eng.state.yes_ask = 0.51;
    eng.bands.push(band(700.0));
    eng.state.s_hat = 100.02;
    for i in 1..=4 {
        eng.oracle_bins.write(NOW + i * 250, 100.02);
    }
    eng.state.bba_ts_ms = NOW + 1_000;
    let out = eng.on_event(Event::Book { ts_ms: NOW + 1_000, yes_bid: 0.49, yes_ask: 0.51 }, NOW + 1_000);
    assert_eq!(eng.bands.newest().unwrap().ref_max, 100.0, "stale quote keeps its reference");
    assert_eq!(eng.bands.newest().unwrap().expires_ms, NOW + 1_000 + eng.cfg.gtr_ms);
    let takes = submits(&out, IntentKind::Taker);
    assert_eq!(takes.len(), 1, "stale book vs moved Ŝ is the take");
    assert_eq!((takes[0].token, takes[0].px_ticks), (TokenRef::Yes, 51));
}

#[test]
fn first_book_builds_gtr_band_even_during_cooldown() {
    let mut eng = ready(100.0);
    eng.state.allow = AllowTrade::WaitingBook;
    eng.state.book_ts_ms = 0;
    eng.state.yes_bid = 0.0;
    eng.state.yes_ask = 0.0;
    eng.bands.clear();
    eng.on_event(
        Event::Book {
            ts_ms: NOW,
            yes_bid: 0.999,
            yes_ask: 1.0,
        },
        NOW,
    );
    let b = eng.bands.newest().expect("first book must mint a GTR band");
    assert!((b.mid - 0.9995).abs() < 1e-9);
    assert!(matches!(eng.state.allow, AllowTrade::Cooldown(_)));
    assert_eq!(eng.state.quote_ts_ms, NOW, "quotes still computed in cooldown");

    eng.on_event(
        Event::Book {
            ts_ms: NOW + 100,
            yes_bid: 0.999,
            yes_ask: 1.0,
        },
        NOW + 100,
    );
    assert_eq!(eng.bands.newest().unwrap().mid, b.mid, "frozen CLOB extends the same band");
    assert_eq!(eng.bands.newest().unwrap().expires_ms, NOW + 100 + eng.cfg.gtr_ms);
}

#[test]
fn expired_gtr_stays_alive_on_quiet_fresh_book() {
    let mut eng = ready(100.0);
    eng.state.book_ts_ms = NOW - 1_000;
    eng.state.yes_bid = 0.49;
    eng.state.yes_ask = 0.51;
    eng.state.book_mid = 0.50;
    eng.bands.push(Band {
        expires_ms: NOW - 1,
        ..band(700.0)
    });
    eng.state.s_hat = 100.02;
    eng.state.last_s_hat = 100.0;
    eng.state.fast_mid = 100.02;
    let out = eng.on_event(
        Event::BinanceBba {
            ts_exch_ms: NOW,
            bid: 100.01,
            ask: 100.03,
        },
        NOW,
    );
    let mut buf = [Band::default(); 64];
    assert!(
        eng.bands.valid(NOW, &mut buf) >= 1,
        "quiet CLOB within book_stale_ms must not drop GTR just because the band aged out"
    );
    assert_ne!(eng.state.allow, AllowTrade::WaitingBook);
    let takes = submits(&out, IntentKind::Taker);
    assert_eq!(takes.len(), 1, "Ŝ vs last live book refs is still a take");
    assert_eq!((takes[0].token, takes[0].px_ticks), (TokenRef::Yes, 51));
}

#[test]
fn stale_book_drops_gtr_and_does_not_take() {
    let mut eng = ready(100.02);
    eng.state.book_ts_ms = NOW - 5_000;
    eng.state.yes_ask = 0.51;
    eng.bands.push(Band {
        expires_ms: NOW + 3_000,
        ..band(700.0)
    });
    let out = eng.on_event(
        Event::BinanceBba {
            ts_exch_ms: NOW,
            bid: 100.01,
            ask: 100.03,
        },
        NOW,
    );
    assert!(eng.bands.newest().is_none(), "dead CLOB must not keep GTR");
    assert_eq!(eng.state.allow, AllowTrade::WaitingBook);
    assert!(submits(&out, IntentKind::Taker).is_empty());
}

#[test]
fn stale_chainlink_does_not_drive_model_s() {
    let mut eng = ready(100.02);
    eng.state.chainlink = 200.0;
    eng.state.chainlink_ts_ms = NOW - 60_000;
    assert!((eng.model_s(NOW) - 100.02).abs() < 1e-12);
    eng.state.chainlink_ts_ms = NOW;
    assert!((eng.model_s(NOW) - 200.0).abs() < 1e-12);
}

#[test]
fn chainlink_mid_is_fresh_enough_to_take() {
    let mut eng = ready(100.02);
    eng.state.bba_from_binance = false;
    eng.state.bba_ts_ms = NOW - 2_500;
    eng.state.chainlink_ts_ms = NOW - 400;
    eng.state.yes_ask = 0.52;
    eng.bands.push(band(700.0));
    let mut out = Vec::new();
    eng.policy(NOW, &mut out);
    assert_eq!(submits(&out, IntentKind::Taker).len(), 1);
}

#[test]
fn stale_binance_without_chainlink_blocks_take() {
    let mut eng = ready(100.02);
    eng.state.bba_from_binance = true;
    eng.state.bba_ts_ms = NOW - 2_500;
    eng.state.yes_ask = 0.52;
    eng.bands.push(band(700.0));
    let mut out = Vec::new();
    eng.policy(NOW, &mut out);
    assert!(submits(&out, IntentKind::Taker).is_empty());
}

#[test]
fn lock_in_uses_last_twap_not_mean() {
    let mut eng = Engine::new(EngineConfig::default());
    eng.on_event(Event::LoadContract(contract(100.0)), UTC0);
    let t0 = UTC1 - 50_000;
    eng.on_event(Event::Twap { ts_ms: t0, value: 100.0 }, t0);
    eng.on_event(Event::Twap { ts_ms: t0 + 10_000, value: 110.0 }, t0 + 10_000);
    assert!((eng.state.locked_s() - 110.0).abs() < 1e-12);
}

#[test]
fn tick_range_allows_wings_not_rails() {
    let eng = ready(100.0);
    assert!(eng.px_in_range(1) && eng.px_in_range(99));
    assert!(!eng.px_in_range(0) && !eng.px_in_range(100));
}
