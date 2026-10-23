use anyhow::Result;
use ohioism_trade::config::Config;
use ohioism_trade::db::App;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_ansi(false)
        .init();

    let cfg = Config::from_env()?;
    let listen = cfg.listen.clone();
    let admin_listen = cfg.admin_listen.clone();
    let admin_url = cfg.admin_url.clone();
    let app = App::connect(cfg).await?;
    tracing::info!(db = %app.cfg.mongo_db, admin_url = %admin_url, "trade-api");

    if let Some(addr) = admin_listen {
        let admin_app = app.clone().with_force_admin();
        let listener = TcpListener::bind(&addr).await?;
        tracing::info!(%addr, "admin listening");
        let router = ohioism_trade::api::router(admin_app).layer(TraceLayer::new_for_http());
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                tracing::error!(error = %e, "admin server stopped");
            }
        });
    }

    let router = ohioism_trade::api::router(app).layer(TraceLayer::new_for_http());
    let listener = TcpListener::bind(&listen).await?;
    tracing::info!(%listen, "listening");
    axum::serve(listener, router).await?;
    Ok(())
}
