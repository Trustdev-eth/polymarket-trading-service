use anyhow::Result;
use clap::Parser;
use mm_clob::CLOB_HOST;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "trade-fleet")]
struct Args {
    #[arg(long, env = "FLEET_BIND", default_value = "0.0.0.0:8090")]
    bind: String,
    #[arg(long, env = "FLEET_TOKEN", default_value = "")]
    token: String,
    #[arg(long, env = "MONGODB_URI")]
    mongo_uri: String,
    #[arg(long, env = "MONGODB_DB", default_value = "ohioismtrade")]
    mongo_db: String,
    #[arg(long, env = "KEY_WRAP_SECRET", default_value = "")]
    key_wrap_secret: String,
    #[arg(long, env = "PAPER", default_value_t = 1)]
    paper: u32,
    #[arg(long, env = "ALLOW_LIVE", default_value_t = 0)]
    allow_live: u32,
    #[arg(long, env = "SHARD", default_value_t = 0)]
    shard: u32,
    #[arg(long, env = "SHARDS", default_value_t = 1)]
    shards: u32,
    #[arg(long, env = "CHAIN_ID", default_value_t = 137)]
    chain_id: u64,
    #[arg(long, env = "CLOB_HOST", default_value = CLOB_HOST)]
    clob_host: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_ansi(false)
        .init();
    let args = Args::parse();
    ohioism_trade::fleet::run(ohioism_trade::fleet::FleetArgs {
        bind: args.bind,
        token: args.token,
        mongo_uri: args.mongo_uri,
        mongo_db: args.mongo_db,
        key_wrap_secret: args.key_wrap_secret,
        paper: args.paper != 0,
        allow_live: args.allow_live != 0,
        shard: args.shard,
        shards: args.shards.max(1),
        chain_id: args.chain_id,
        clob_host: args.clob_host,
    })
    .await
}
