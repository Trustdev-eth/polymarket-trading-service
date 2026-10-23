use mongodb::bson::oid::ObjectId;
use mongodb::bson::DateTime as BsonDateTime;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sku {
    Test,
    Month,
}

impl Sku {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "test" => Some(Self::Test),
            "month" => Some(Self::Month),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Month => "month",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KeysDoc {
    #[serde(default)]
    pub funder: String,
    /// Legacy plaintext. New saves leave this empty and use `api_key_enc`.
    #[serde(default, rename = "apiKey")]
    pub api_key: String,
    #[serde(default, rename = "apiKeyEnc")]
    pub api_key_enc: String,
    #[serde(default, rename = "secretEnc")]
    pub secret_enc: String,
    #[serde(default, rename = "passEnc")]
    pub pass_enc: String,
    #[serde(default, rename = "keyEnc")]
    pub key_enc: String,
    #[serde(default, rename = "sigType")]
    pub sig_type: i32,
    #[serde(default, rename = "connectedAt")]
    pub connected_at: Option<BsonDateTime>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Member {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    pub id: Option<ObjectId>,
    pub email: String,
    #[serde(rename = "passwordHash")]
    pub password_hash: String,
    #[serde(default)]
    pub admin: bool,
    #[serde(default)]
    pub plan: String,
    #[serde(default, rename = "accessUntil")]
    pub access_until: Option<BsonDateTime>,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub halted: bool,
    #[serde(default)]
    pub live: bool,
    #[serde(default = "default_true")]
    pub paper: bool,
    #[serde(default = "default_true")]
    pub hedge: bool,
    #[serde(default = "default_base", rename = "baseAmount")]
    pub base_amount: f64,
    #[serde(default = "default_net", rename = "maxNet")]
    pub max_net: f64,
    /// Missing means $200. Zero is a real (lost) bankroll — do not re-seed.
    #[serde(default, rename = "paperCashUsd")]
    pub paper_cash_usd: Option<f64>,
    #[serde(default)]
    pub deficit: i32,
    #[serde(default, rename = "lastAlloc")]
    pub last_alloc: String,
    #[serde(default, rename = "tradeReferral")]
    pub trade_referral: String,
    #[serde(default, rename = "tradePaidUsd")]
    pub trade_paid_usd: f64,
    #[serde(default)]
    pub keys: Option<KeysDoc>,
    #[serde(default, rename = "createdAt")]
    pub created_at: Option<BsonDateTime>,
}

fn default_true() -> bool {
    true
}
fn default_base() -> f64 {
    5.0
}
fn default_net() -> f64 {
    15.0
}

impl Member {
    pub fn active(&self, now_ms: i64) -> bool {
        self.access_until
            .map(|d| d.timestamp_millis() > now_ms)
            .unwrap_or(false)
            && !self.blocked
    }

    pub fn keys_connected(&self) -> bool {
        self.keys.as_ref().is_some_and(|k| {
            !k.secret_enc.is_empty()
                && !k.funder.is_empty()
                && (!k.api_key_enc.is_empty() || !k.api_key.is_empty())
        })
    }

    pub fn access_until_ms(&self) -> i64 {
        self.access_until.map(|d| d.timestamp_millis()).unwrap_or(0)
    }

    pub fn paper_cash(&self) -> f64 {
        self.paper_cash_usd.unwrap_or(crate::config::PAPER_CASH)
    }

    pub fn run_flags_at(&self, now_ms: i64) -> crate::access::RunFlags {
        if self.blocked || !self.active(now_ms) {
            crate::access::RunFlags::stopped()
        } else if self.halted {
            crate::access::RunFlags::stopped()
        } else if self.live {
            crate::access::RunFlags::live()
        } else if self.paper {
            crate::access::RunFlags::paper()
        } else {
            crate::access::RunFlags::stopped()
        }
    }

    pub fn run_flags(&self) -> crate::access::RunFlags {
        self.run_flags_at(chrono::Utc::now().timestamp_millis())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicMember {
    pub email: String,
    pub admin: bool,
    pub plan: String,
    #[serde(rename = "accessUntil")]
    pub access_until: Option<String>,
    pub active: bool,
    pub blocked: bool,
    pub halted: bool,
    pub live: bool,
    pub paper: bool,
    pub hedge: bool,
    #[serde(rename = "baseAmount")]
    pub base_amount: f64,
    #[serde(rename = "maxNet")]
    pub max_net: f64,
    #[serde(rename = "paperCashUsd")]
    pub paper_cash_usd: f64,
    #[serde(rename = "keysConnected")]
    pub keys_connected: bool,
    #[serde(rename = "liveReady")]
    pub live_ready: bool,
    #[serde(rename = "paperRunning")]
    pub paper_running: bool,
    #[serde(rename = "liveRunning")]
    pub live_running: bool,
    #[serde(rename = "runStatus")]
    pub run_status: String,
    pub funder: String,
    #[serde(rename = "tradeReferral")]
    pub trade_referral: String,
    #[serde(rename = "tradePaidUsd")]
    pub trade_paid_usd: f64,
    #[serde(rename = "partnerUsd")]
    pub partner_usd: f64,
    #[serde(rename = "lastAlloc")]
    pub last_alloc: String,
    #[serde(rename = "createdAt")]
    pub created_at: Option<String>,
}

impl PublicMember {
    pub fn from_member(m: &Member, partner_share: f64, now_ms: i64) -> Self {
        Self {
            email: m.email.clone(),
            admin: m.admin,
            plan: if m.plan.is_empty() {
                "none".into()
            } else {
                m.plan.clone()
            },
            access_until: m.access_until.and_then(bson_iso),
            active: m.active(now_ms),
            blocked: m.blocked,
            halted: m.halted,
            live: m.live,
            paper: m.paper,
            hedge: m.hedge,
            base_amount: m.base_amount,
            max_net: m.max_net,
            paper_cash_usd: m.paper_cash(),
            keys_connected: m.keys_connected(),
            live_ready: m.keys_connected()
                && m.keys
                    .as_ref()
                    .is_some_and(|k| !k.key_enc.is_empty()),
            paper_running: m.run_flags_at(now_ms).paper_running(),
            live_running: m.run_flags_at(now_ms).live_running(),
            run_status: m.run_flags_at(now_ms).status().into(),
            funder: m.keys.as_ref().map(|k| k.funder.clone()).unwrap_or_default(),
            trade_referral: m.trade_referral.clone(),
            trade_paid_usd: m.trade_paid_usd,
            partner_usd: (m.trade_paid_usd * partner_share * 100.0).round() / 100.0,
            last_alloc: m.last_alloc.clone(),
            created_at: m.created_at.and_then(bson_iso),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PublicOrder {
    #[serde(rename = "orderId")]
    pub order_id: String,
    pub email: String,
    pub plan: String,
    #[serde(rename = "amountUsd")]
    pub amount_usd: f64,
    #[serde(rename = "partnerUsd")]
    pub partner_usd: f64,
    pub referral: String,
    pub status: String,
    #[serde(rename = "invoiceUrl")]
    pub invoice_url: String,
    #[serde(rename = "payCurrency")]
    pub pay_currency: String,
    #[serde(rename = "createdAt")]
    pub created_at: Option<String>,
    #[serde(rename = "paidAt")]
    pub paid_at: Option<String>,
}

impl PublicOrder {
    pub fn from_order(o: &Order) -> Self {
        Self {
            order_id: o.order_id.clone(),
            email: o.email.clone(),
            plan: o.plan.clone(),
            amount_usd: o.amount_usd,
            partner_usd: o.partner_usd,
            referral: o.referral.clone(),
            status: o.status.clone(),
            invoice_url: o.invoice_url.clone(),
            pay_currency: o.pay_currency.clone(),
            created_at: o.created_at.and_then(bson_iso),
            paid_at: o.paid_at.and_then(bson_iso),
        }
    }
}

pub fn bson_iso(d: BsonDateTime) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(d.timestamp_millis()).map(|t| t.to_rfc3339())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Order {
    #[serde(rename = "_id", skip_serializing_if = "Option::is_none")]
    pub id: Option<ObjectId>,
    #[serde(rename = "orderId")]
    pub order_id: String,
    pub email: String,
    pub plan: String,
    #[serde(rename = "amountUsd")]
    pub amount_usd: f64,
    #[serde(default, rename = "partnerUsd")]
    pub partner_usd: f64,
    #[serde(default)]
    pub referral: String,
    #[serde(default, rename = "nowpaymentsInvoiceId")]
    pub nowpayments_invoice_id: String,
    #[serde(default, rename = "invoiceUrl")]
    pub invoice_url: String,
    #[serde(default, rename = "paymentId")]
    pub payment_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, rename = "payAddress")]
    pub pay_address: String,
    #[serde(default, rename = "payAmount")]
    pub pay_amount: String,
    #[serde(default, rename = "payCurrency")]
    pub pay_currency: String,
    #[serde(default, rename = "createdAt")]
    pub created_at: Option<BsonDateTime>,
    #[serde(default, rename = "updatedAt")]
    pub updated_at: Option<BsonDateTime>,
    #[serde(default, rename = "paidAt", skip_serializing_if = "Option::is_none")]
    pub paid_at: Option<BsonDateTime>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snap {
    pub email: String,
    #[serde(default)]
    pub ts_ms: i64,
    #[serde(default)]
    pub s_hat: f64,
    #[serde(default)]
    pub yes_bid: f64,
    #[serde(default)]
    pub yes_ask: f64,
    #[serde(default)]
    pub net: f64,
    #[serde(default)]
    pub mark_pnl: f64,
    #[serde(default)]
    pub tau_s: f64,
    #[serde(default)]
    pub quote: String,
    #[serde(default)]
    pub alloc: String,
    #[serde(default)]
    pub plan: String,
    #[serde(default)]
    pub paper: bool,
    #[serde(default)]
    pub live: bool,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub binance: f64,
    #[serde(default)]
    pub chainlink: f64,
    #[serde(default)]
    pub twap: f64,
    #[serde(default)]
    pub no_bid: f64,
    #[serde(default)]
    pub no_ask: f64,
    #[serde(default)]
    pub regime: String,
    #[serde(default)]
    pub k_t: f64,
    #[serde(default)]
    pub fair: Option<f64>,
    #[serde(default)]
    pub utc0_ms: i64,
    #[serde(default)]
    pub utc1_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tick {
    pub slug: String,
    pub ts_ms: i64,
    pub at: BsonDateTime,
    #[serde(default)]
    pub binance: f64,
    #[serde(default)]
    pub chainlink: f64,
    #[serde(default)]
    pub twap: f64,
    #[serde(default)]
    pub s_hat: f64,
    #[serde(default)]
    pub yes_bid: f64,
    #[serde(default)]
    pub yes_ask: f64,
    #[serde(default)]
    pub no_bid: f64,
    #[serde(default)]
    pub no_ask: f64,
    #[serde(default)]
    pub tau_s: f64,
    #[serde(default)]
    pub quote: String,
    #[serde(default)]
    pub regime: String,
    #[serde(default)]
    pub k_t: f64,
    #[serde(default)]
    pub fair: Option<f64>,
    #[serde(default)]
    pub utc0_ms: i64,
    #[serde(default)]
    pub utc1_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowResult {
    pub email: String,
    pub slug: String,
    pub ts_ms: i64,
    pub pnl: f64,
    pub paper: bool,
    #[serde(default)]
    pub close: f64,
    #[serde(default)]
    pub k_t: f64,
    #[serde(default)]
    pub winner_up: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FillRow {
    pub email: String,
    pub ts_ms: i64,
    pub token: String,
    pub size: f64,
    pub px: f64,
    pub kind: String,
    pub paper: bool,
}

/// One live POST. Tokyo sends a whole wave so the UK box can fan out together.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FleetTake {
    pub email: String,
    pub token: String,
    pub px_ticks: i32,
    pub size: f64,
    pub tif: String,
    pub post_only: bool,
    pub seq: u64,
}

/// Tokyo → UK fleet. No secrets on this wire.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum FleetMsg {
    Auth {
        token: String,
    },
    Hello {
        yes_id: String,
        no_id: String,
        tick: f64,
        fee_bps: u32,
        neg_risk: bool,
    },
    Take {
        email: String,
        token: String,
        px_ticks: i32,
        size: f64,
        tif: String,
        post_only: bool,
        seq: u64,
        delay_ms: u64,
    },
    Wave {
        takes: Vec<FleetTake>,
    },
    Cancel {
        email: String,
        seq: u64,
    },
    Halt {
        email: String,
    },
    HaltAll,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum FleetReply {
    Ack { email: String, seq: u64, ok: bool },
    Fill { email: String, seq: u64, size: f64, px: f64 },
    Reject { email: String, seq: u64, reason: String },
}
