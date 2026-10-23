//! One Binance + one Chainlink/TWAP + one CLOB market WS. Fan-out to the coordinator.
//! Never one socket per trader.

use std::time::Duration;

use anyhow::Result;
use mm_engine::Event;
use mm_md::{now_ns, DatedEvent, EventTx};

pub struct MdHub {
    pub rx: crossbeam_channel::Receiver<DatedEvent>,
    /// Kept alive so the CLOB market WS does not see a dropped watch sender.
    pub assets_tx: tokio::sync::watch::Sender<(String, String)>,
}

pub async fn spawn(
    base: &str,
    mins: u32,
    binance: &str,
    yes_id_dec: String,
    no_id_dec: String,
) -> Result<MdHub> {
    // Unbounded on purpose: Binance bookTicker is hundreds/sec. A bounded
    // queue plus a blocking send deadlocks the Tokio workers (WS unread,
    // Mongo replies unread, window never rolls).
    let (tx, rx) = crossbeam_channel::unbounded::<DatedEvent>();
    {
        let (sym, tx) = (binance.to_string(), tx.clone());
        tokio::spawn(async move {
            let _ = mm_md::binance::run_book_ticker(&sym, tx).await;
        });
    }
    {
        let (base, tx) = (base.to_string(), tx.clone());
        tokio::spawn(async move {
            let _ = mm_md::rtds::run_twap(&base, tx).await;
        });
    }
    let (assets_tx, assets_rx) = tokio::sync::watch::channel((yes_id_dec, no_id_dec));
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let _ = mm_md::poly_book::run_market_ws(assets_rx, tx, true).await;
        });
    }
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut i = tokio::time::interval(Duration::from_millis(250));
            loop {
                i.tick().await;
                let _ = tx.send(DatedEvent {
                    recv_ns: now_ns(),
                    event: Event::InvariantTick,
                });
            }
        });
    }
    {
        let tx = tx.clone();
        let assets_rx = assets_tx.subscribe();
        let base = base.to_string();
        let binance = binance.to_string();
        tokio::spawn(async move {
            crate::quotes::run_rest_feeds(base, mins, binance, assets_rx, tx).await;
        });
    }
    let _keep: EventTx = tx;
    Ok(MdHub { rx, assets_tx })
}
