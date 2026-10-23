use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "trade-coordinator")]
struct Args {
    #[arg(long, env = "MONGODB_URI")]
    mongo_uri: String,
    #[arg(long, env = "MONGODB_DB", default_value = "ohioismtrade")]
    mongo_db: String,
    #[arg(long, env = "BASE", default_value = "btc")]
    base: String,
    #[arg(long, env = "MINS", default_value_t = 15)]
    mins: u32,
    #[arg(long, env = "BINANCE_SYMBOL", default_value = "btcusdt")]
    binance: String,
    #[arg(long, env = "MAX_WAVE", default_value_t = 80.0)]
    max_wave: f64,
    #[arg(long, env = "FLEET_ADDR", default_value = "")]
    fleet_addr: String,
    #[arg(long, env = "FLEET_TOKEN", default_value = "")]
    fleet_token: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_ansi(false)
        .init();
    let args = Args::parse();
    ohioism_trade::coord::run(ohioism_trade::coord::CoordArgs {
        mongo_uri: args.mongo_uri,
        mongo_db: args.mongo_db,
        base: args.base,
        mins: args.mins,
        binance: args.binance,
        max_wave: args.max_wave,
        fleet_addr: args.fleet_addr,
        fleet_token: args.fleet_token,
    })
    .await
}
