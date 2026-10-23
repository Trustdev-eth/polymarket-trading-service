use std::sync::Arc;

use chrono::{TimeZone, Utc};
use mongodb::bson::{doc, Bson, DateTime as BsonDateTime};
use mongodb::{Client, Collection, Database, IndexModel};
use reqwest::Client as HttpClient;

use crate::access::next_access;
use crate::auth::RateLimit;
use crate::config::{Config, HOUSE_EMAIL, PAPER_CASH};
use crate::error::{AppError, AppResult};
use crate::models::{FillRow, Member, Order, PublicMember, Sku, Snap, Tick, WindowResult};

#[derive(Clone)]
pub struct App {
    pub cfg: Config,
    pub db: Database,
    pub http: HttpClient,
    pub limiter: Arc<RateLimit>,
    pub force_admin: bool,
}

impl App {
    pub async fn connect(cfg: Config) -> anyhow::Result<Self> {
        let db = Client::with_uri_str(&cfg.mongo_uri)
            .await?
            .database(&cfg.mongo_db);
        let app = Self {
            cfg,
            db,
            http: HttpClient::builder()
                .user_agent("ohioism-trade/0.1")
                .build()?,
            limiter: Arc::new(RateLimit::new()),
            force_admin: false,
        };
        app.ensure_indexes().await?;
        Ok(app)
    }

    pub fn with_force_admin(mut self) -> Self {
        self.force_admin = true;
        self
    }

    async fn ensure_indexes(&self) -> anyhow::Result<()> {
        let uniq = mongodb::options::IndexOptions::builder().unique(true).build();
        let _ = self
            .members()
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "email": 1 })
                    .options(uniq.clone())
                    .build(),
                None,
            )
            .await;
        let _ = self
            .orders()
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "orderId": 1 })
                    .options(uniq)
                    .build(),
                None,
            )
            .await;
        let _ = self
            .snaps()
            .create_index(IndexModel::builder().keys(doc! { "email": 1 }).build(), None)
            .await;
        let _ = self
            .fills()
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "email": 1, "ts_ms": -1 })
                    .build(),
                None,
            )
            .await;
        let ttl = mongodb::options::IndexOptions::builder()
            .expire_after(std::time::Duration::from_secs(20 * 60))
            .build();
        let _ = self
            .ticks()
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "at": 1 })
                    .options(ttl)
                    .build(),
                None,
            )
            .await;
        let _ = self
            .ticks()
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "slug": 1, "ts_ms": 1 })
                    .build(),
                None,
            )
            .await;
        let _ = self
            .results()
            .create_index(
                IndexModel::builder()
                    .keys(doc! { "email": 1, "ts_ms": -1 })
                    .build(),
                None,
            )
            .await;
        Ok(())
    }

    pub fn members(&self) -> Collection<Member> {
        self.db.collection("members")
    }
    pub fn orders(&self) -> Collection<Order> {
        self.db.collection("orders")
    }
    pub fn snaps(&self) -> Collection<Snap> {
        self.db.collection("snaps")
    }
    pub fn fills(&self) -> Collection<FillRow> {
        self.db.collection("fills")
    }
    pub fn ticks(&self) -> Collection<Tick> {
        self.db.collection("ticks")
    }
    pub fn results(&self) -> Collection<WindowResult> {
        self.db.collection("results")
    }

    pub async fn find_member(&self, email: &str) -> AppResult<Option<Member>> {
        Ok(self.members().find_one(doc! { "email": email }, None).await?)
    }

    pub async fn require_member_email(&self, email: &str) -> AppResult<Member> {
        self.find_member(email)
            .await?
            .ok_or(AppError::Unauthorized)
    }

    pub fn public(&self, m: &Member) -> PublicMember {
        PublicMember::from_member(m, self.cfg.partner_share, now_ms())
    }

    pub async fn insert_member(&self, m: &Member) -> AppResult<()> {
        self.members().insert_one(m, None).await?;
        Ok(())
    }

    pub async fn find_order(&self, order_id: &str) -> AppResult<Option<Order>> {
        Ok(self.orders().find_one(doc! { "orderId": order_id }, None).await?)
    }

    pub async fn insert_order(&self, o: &Order) -> AppResult<()> {
        self.orders().insert_one(o, None).await?;
        Ok(())
    }

    /// Returns true if this caller should grant access (first paid IPN).
    pub async fn mark_paid_if_new(
        &self,
        order_id: &str,
        status: &str,
        payment_id: &str,
        pay_address: &str,
        pay_amount: &str,
        pay_currency: &str,
        invoice_id: &str,
    ) -> AppResult<bool> {
        let now = BsonDateTime::from_millis(now_ms());
        let mut set = doc! {
            "status": status,
            "updatedAt": now,
        };
        if !payment_id.is_empty() {
            set.insert("paymentId", payment_id);
        }
        if !pay_address.is_empty() {
            set.insert("payAddress", pay_address);
        }
        if !pay_amount.is_empty() {
            set.insert("payAmount", pay_amount);
        }
        if !pay_currency.is_empty() {
            set.insert("payCurrency", pay_currency);
        }
        if !invoice_id.is_empty() {
            set.insert("nowpaymentsInvoiceId", invoice_id);
        }
        self.orders()
            .update_one(doc! { "orderId": order_id }, doc! { "$set": set }, None)
            .await?;
        if !crate::nowpayments::unlocks(status) {
            return Ok(false);
        }
        let r = self
            .orders()
            .update_one(
                unpaid_order_filter(order_id),
                doc! { "$set": { "paidAt": now, "status": status } },
                None,
            )
            .await?;
        Ok(r.matched_count == 1)
    }

    pub async fn grant_sku(&self, email: &str, sku: Sku, amount_usd: f64) -> AppResult<Member> {
        let member = self
            .find_member(email)
            .await?
            .ok_or_else(|| AppError::NotFound("member".into()))?;
        if member.admin {
            return Ok(member);
        }
        let now = Utc::now();
        let current_until = member
            .access_until
            .and_then(|d| Utc.timestamp_millis_opt(d.timestamp_millis()).single());
        let g = next_access(sku, now, &member.plan, current_until);
        let until = BsonDateTime::from_millis(g.until.timestamp_millis());
        self.members()
            .update_one(
                doc! { "email": email },
                doc! {
                    "$set": {
                        "plan": g.plan,
                        "accessUntil": until,
                        "baseAmount": g.base_amount,
                        "maxNet": g.max_net,
                        "paperCashUsd": PAPER_CASH,
                        "hedge": false,
                        "paper": true,
                        "live": false,
                        "halted": false,
                    },
                    "$inc": { "tradePaidUsd": amount_usd },
                },
                None,
            )
            .await?;
        self.find_member(email)
            .await?
            .ok_or_else(|| AppError::NotFound("member".into()))
    }

    pub async fn snap_for(&self, email: &str) -> AppResult<Snap> {
        Ok(self
            .snaps()
            .find_one(doc! { "email": email }, None)
            .await?
            .unwrap_or_else(|| Snap {
                email: email.into(),
                ..Snap::default()
            }))
    }

    pub async fn house_snap(&self) -> AppResult<Snap> {
        self.snap_for(HOUSE_EMAIL).await
    }

    pub async fn recent_fills(&self, email: &str, limit: i64) -> AppResult<Vec<FillRow>> {
        use futures_util::TryStreamExt;
        let opts = mongodb::options::FindOptions::builder()
            .sort(doc! { "ts_ms": -1 })
            .limit(limit)
            .build();
        let mut cur = self.fills().find(doc! { "email": email }, opts).await?;
        let mut out = Vec::new();
        while let Some(row) = cur.try_next().await? {
            out.push(row);
        }
        Ok(out)
    }

    pub async fn recent_ticks(&self, slug: &str, limit: i64) -> AppResult<Vec<Tick>> {
        use futures_util::TryStreamExt;
        let filter = if slug.is_empty() {
            doc! {}
        } else {
            doc! { "slug": slug }
        };
        let opts = mongodb::options::FindOptions::builder()
            .sort(doc! { "ts_ms": -1 })
            .limit(limit)
            .build();
        let mut cur = self.ticks().find(filter, opts).await?;
        let mut out = Vec::new();
        while let Some(row) = cur.try_next().await? {
            out.push(row);
        }
        out.reverse();
        Ok(out)
    }

    pub async fn last_tick(&self) -> AppResult<Option<Tick>> {
        let opts = mongodb::options::FindOptions::builder()
            .sort(doc! { "ts_ms": -1 })
            .limit(1)
            .build();
        let mut cur = self.ticks().find(doc! {}, opts).await?;
        use futures_util::TryStreamExt;
        Ok(cur.try_next().await?)
    }

    pub async fn recent_results(&self, email: &str, limit: i64) -> AppResult<Vec<WindowResult>> {
        use futures_util::TryStreamExt;
        let opts = mongodb::options::FindOptions::builder()
            .sort(doc! { "ts_ms": -1 })
            .limit(limit)
            .build();
        let mut cur = self.results().find(doc! { "email": email }, opts).await?;
        let mut out = Vec::new();
        while let Some(row) = cur.try_next().await? {
            out.push(row);
        }
        Ok(out)
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// New invoices used to serialize `paidAt: null`. In Mongo that field *exists*,
/// so `$exists: false` never granted. Match missing *or* null.
fn unpaid_order_filter(order_id: &str) -> mongodb::bson::Document {
    doc! {
        "orderId": order_id,
        "$or": [
            { "paidAt": { "$exists": false } },
            { "paidAt": Bson::Null },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpaid_filter_matches_missing_or_null_paid_at() {
        let d = unpaid_order_filter("ord-1");
        assert_eq!(d.get_str("orderId").unwrap(), "ord-1");
        let or = d.get_array("$or").unwrap();
        assert_eq!(or.len(), 2);
        let missing = or[0].as_document().unwrap();
        assert_eq!(
            missing
                .get_document("paidAt")
                .unwrap()
                .get_bool("$exists")
                .unwrap(),
            false
        );
        let nulls = or[1].as_document().unwrap();
        assert!(matches!(nulls.get("paidAt"), Some(Bson::Null)));
    }
}
