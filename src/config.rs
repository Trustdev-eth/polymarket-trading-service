use anyhow::{bail, Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: String,
    pub admin_listen: Option<String>,
    pub mongo_uri: String,
    pub mongo_db: String,
    pub auth_secret: String,
    pub app_url: String,
    pub admin_url: String,
    pub admin_hosts: Vec<String>,
    pub cookie_domain: String,
    pub nowpayments_api_key: String,
    pub nowpayments_ipn_secret: String,
    pub key_wrap_secret: String,
    pub admin_emails: Vec<String>,
    pub cookie_secure: bool,
    pub partner_share: f64,
    pub telegram_bot_token: String,
    pub telegram_chat_id: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let auth_secret = std::env::var("AUTH_SECRET").unwrap_or_default();
        if auth_secret.len() < 16 {
            bail!("AUTH_SECRET must be at least 16 characters");
        }
        let mongo_uri = std::env::var("MONGODB_URI").context("MONGODB_URI is required")?;
        let key_wrap_secret = std::env::var("KEY_WRAP_SECRET").unwrap_or_default();
        crate::crypto::wrap_key(&key_wrap_secret)
            .context("KEY_WRAP_SECRET must be 32 random bytes as 64 hex chars (openssl rand -hex 32). Same value on API and UK fleet. Never store it in Mongo.")?;
        let admins = std::env::var("ADMIN_EMAILS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let cookie_secure = match std::env::var("COOKIE_SECURE") {
            Ok(v) => v != "0" && v.to_lowercase() != "false",
            Err(_) => std::env::var("APP_URL")
                .unwrap_or_default()
                .starts_with("https://"),
        };
        let partner_share = std::env::var("PARTNER_SHARE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.15);
        let admin_url = std::env::var("ADMIN_URL")
            .unwrap_or_else(|_| "https://admin.ohioismquant.vip".into())
            .trim_end_matches('/')
            .to_string();
        let extra_hosts = std::env::var("ADMIN_HOSTS").unwrap_or_default();
        Ok(Self {
            listen: std::env::var("LISTEN").unwrap_or_else(|_| "0.0.0.0:8787".into()),
            admin_listen: std::env::var("ADMIN_LISTEN")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            mongo_uri,
            mongo_db: std::env::var("MONGODB_DB").unwrap_or_else(|_| "ohioismtrade".into()),
            auth_secret,
            app_url: std::env::var("APP_URL")
                .unwrap_or_else(|_| "https://trade.ohioismquant.vip".into())
                .trim_end_matches('/')
                .to_string(),
            admin_hosts: collect_admin_hosts(&admin_url, &extra_hosts),
            admin_url,
            cookie_domain: std::env::var("COOKIE_DOMAIN").unwrap_or_default(),
            nowpayments_api_key: std::env::var("NOWPAYMENTS_API_KEY").unwrap_or_default(),
            nowpayments_ipn_secret: std::env::var("NOWPAYMENTS_IPN_SECRET").unwrap_or_default(),
            key_wrap_secret,
            admin_emails: admins,
            cookie_secure,
            partner_share,
            telegram_bot_token: std::env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default(),
            telegram_chat_id: std::env::var("TELEGRAM_CHAT_ID").unwrap_or_default(),
        })
    }

    pub fn is_admin_email(&self, email: &str) -> bool {
        self.admin_emails.iter().any(|e| e == &email.to_lowercase())
    }

    pub fn is_admin_host(&self, host_header: &str) -> bool {
        self.admin_hosts.iter().any(|h| host_matches(h, host_header))
    }
}

pub fn host_from_url(url: &str) -> String {
    url.trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches('.')
        .to_lowercase()
}

pub fn normalize_host(raw: &str) -> String {
    raw.split(',')
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches('.')
        .to_lowercase()
}

pub fn hostname_of(raw: &str) -> String {
    let s = normalize_host(raw);
    if s.starts_with('[') {
        return s
            .split(']')
            .next()
            .unwrap_or("")
            .trim_start_matches('[')
            .to_string();
    }
    if s.chars().filter(|c| *c == ':').count() == 1 {
        s.rsplit_once(':')
            .map(|(h, _)| h.to_string())
            .unwrap_or(s)
    } else {
        s
    }
}

pub fn looks_like_admin_host(host: &str) -> bool {
    let name = hostname_of(host);
    name.split('.').next() == Some("admin")
}

pub fn collect_admin_hosts(admin_url: &str, extra: &str) -> Vec<String> {
    let mut out = Vec::new();
    let push = |out: &mut Vec<String>, raw: &str| {
        let h = normalize_host(raw);
        if h.is_empty() {
            return;
        }
        if !out.iter().any(|e| e == &h) {
            out.push(h);
        }
    };
    push(&mut out, "admin.ohioismquant.vip");
    let from_url = host_from_url(admin_url);
    if looks_like_admin_host(&from_url) {
        push(&mut out, &from_url);
    }
    for part in extra.split(',') {
        push(&mut out, part);
    }
    out
}

pub fn host_matches(configured: &str, request: &str) -> bool {
    let c = normalize_host(configured);
    let r = normalize_host(request);
    if c.is_empty() || r.is_empty() {
        return false;
    }
    if r == c {
        return true;
    }
    let ch = hostname_of(&c);
    let rh = hostname_of(&r);
    if ch != rh {
        return false;
    }
    let configured_has_port = c != ch;
    if configured_has_port {
        r == c
    } else {
        true
    }
}

pub const TEST_USD: f64 = 50.0;
pub const MONTH_USD: f64 = 800.0;
pub const TEST_DAYS: i64 = 3;
pub const MONTH_DAYS: i64 = 30;
/// Polymarket CLOB minimum. Also the house default.
pub const MIN_BASE: f64 = 5.0;
pub const HOUSE_BASE: f64 = 5.0;
pub const HOUSE_NET: f64 = 0.0;
/// Test stays on the CLOB minimum. Month may size up from house best.
pub const TEST_BASE: f64 = 5.0;
pub const TEST_NET: f64 = 15.0;
pub const MONTH_BASE: f64 = 50.0;
pub const MONTH_NET: f64 = 150.0;
pub const COOKIE_NAME: &str = "trade_session";
pub const HOUSE_EMAIL: &str = "_house";
/// Virtual paper bankroll shown as Cash. Window PnL is added on settle.
pub const PAPER_CASH: f64 = 200.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_vip_host_matches() {
        let hosts = collect_admin_hosts("https://admin.ohioismquant.vip", "");
        assert!(hosts.iter().any(|h| h == "admin.ohioismquant.vip"));
        assert!(host_matches("admin.ohioismquant.vip", "admin.ohioismquant.vip"));
        assert!(host_matches("admin.ohioismquant.vip", "ADMIN.ohioismquant.vip:443"));
        assert!(!host_matches("admin.ohioismquant.vip", "trade.ohioismquant.vip"));
        assert!(!hosts.iter().any(|h| h.starts_with("127.")));
    }

    #[test]
    fn admin_url_localhost_is_not_auto_admin() {
        let hosts = collect_admin_hosts("http://127.0.0.1:8790", "");
        assert!(!hosts.iter().any(|h| h.contains("127.0.0.1")));
        assert!(hosts.contains(&"admin.ohioismquant.vip".into()));
    }

    #[test]
    fn extra_admin_hosts() {
        let hosts = collect_admin_hosts(
            "https://admin.ohioismquant.vip",
            "admin.local, 127.0.0.1:8791",
        );
        assert!(hosts.iter().any(|h| h == "admin.local"));
        assert!(hosts.iter().any(|h| h == "127.0.0.1:8791"));
        assert!(host_matches("127.0.0.1:8791", "127.0.0.1:8791"));
        assert!(!host_matches("127.0.0.1:8791", "127.0.0.1:8790"));
    }

    #[test]
    fn wrap_secret_must_be_32_byte_hex() {
        assert!(crate::crypto::wrap_key("short").is_err());
        assert!(crate::crypto::wrap_key(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        )
        .is_ok());
    }
}
