use std::convert::Infallible;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Json;
use chrono::{Duration as ChronoDuration, TimeZone, Utc};
use futures_util::stream::unfold;
use mongodb::bson::{doc, DateTime as BsonDateTime};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::access::next_run;
use crate::auth::{
    clear_cookie, client_ip, email_from_headers, hash_password, normalize_email, request_host,
    session_cookie, sign_session, valid_email, verify_password,
};
use crate::capital::{self, house_best};
use crate::config::{HOUSE_BASE, HOUSE_NET, MIN_BASE, MONTH_USD, PAPER_CASH, TEST_USD};
use crate::crypto::{seal, wrap_key};
use crate::db::{now_ms, App};
use crate::error::{AppError, AppResult};
use crate::html;
use crate::models::{KeysDoc, Member, Order, PublicOrder, Sku};
use crate::nowpayments;
use crate::telegram;

fn cookie_response(body: Value, set_cookie: String) -> Response {
    let mut res = Json(body).into_response();
    res.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&set_cookie).unwrap_or_else(|_| HeaderValue::from_static("")),
    );
    res
}

async fn load_session(app: &App, headers: &HeaderMap) -> AppResult<Member> {
    let email = email_from_headers(&app.cfg, headers).ok_or(AppError::Unauthorized)?;
    let m = app.require_member_email(&email).await?;
    if m.blocked {
        return Err(AppError::Forbidden("This account is blocked.".into()));
    }
    Ok(m)
}

fn require_vip(member: &Member) -> AppResult<()> {
    if crate::access::vip_open(member.admin, member.access_until_ms(), now_ms(), member.blocked) {
        Ok(())
    } else {
        Err(AppError::Forbidden("Subscribe first.".into()))
    }
}

async fn require_admin(app: &App, headers: &HeaderMap) -> AppResult<Member> {
    let m = load_session(app, headers).await?;
    if !m.admin {
        return Err(AppError::Forbidden("admin only".into()));
    }
    Ok(m)
}

fn is_admin_request(app: &App, headers: &HeaderMap) -> bool {
    app.force_admin || app.cfg.is_admin_host(&request_host(headers))
}

fn admin_login_response(app: &App, headers: &HeaderMap) -> Response {
    if is_admin_request(app, headers) {
        html::admin_login(&app.cfg.app_url).into_response()
    } else {
        Redirect::to("/login?next=/admin").into_response()
    }
}

pub async fn landing(State(app): State<App>, headers: HeaderMap) -> Response {
    if is_admin_request(&app, &headers) {
        return admin_page_inner(&app, &headers).await;
    }
    let member = load_session(&app, &headers).await.ok();
    html::landing(
        member.as_ref().map(|m| m.email.as_str()),
        member.as_ref().is_some_and(|m| m.admin),
        &app.cfg.admin_url,
    )
    .into_response()
}

pub async fn login_page(State(app): State<App>, headers: HeaderMap) -> Html<String> {
    if is_admin_request(&app, &headers) {
        html::admin_login(&app.cfg.app_url)
    } else {
        html::login()
    }
}

pub async fn desk_page(State(app): State<App>, headers: HeaderMap) -> Response {
    if is_admin_request(&app, &headers) {
        return Redirect::to(&format!("{}/desk", app.cfg.app_url)).into_response();
    }
    match load_session(&app, &headers).await {
        Ok(m) => html::desk(
            &m.email,
            m.admin,
            &app.cfg.admin_url,
            m.admin || m.active(now_ms()),
        )
        .into_response(),
        Err(_) => Redirect::to("/login?next=/desk").into_response(),
    }
}

pub async fn dash_page(State(app): State<App>, headers: HeaderMap) -> Response {
    if is_admin_request(&app, &headers) {
        return Redirect::to(&format!("{}/dash", app.cfg.app_url)).into_response();
    }
    match load_session(&app, &headers).await {
        Ok(m) => html::dash(
            &m.email,
            m.admin,
            &app.cfg.admin_url,
            m.admin || m.active(now_ms()),
        )
        .into_response(),
        Err(_) => Redirect::to("/login?next=/dash").into_response(),
    }
}

pub async fn pnl_page(State(app): State<App>, headers: HeaderMap) -> Response {
    if is_admin_request(&app, &headers) {
        return Redirect::to(&format!("{}/pnl", app.cfg.app_url)).into_response();
    }
    match load_session(&app, &headers).await {
        Ok(m) => html::pnl(
            &m.email,
            m.admin,
            &app.cfg.admin_url,
            m.admin || m.active(now_ms()),
        )
        .into_response(),
        Err(_) => Redirect::to("/login?next=/pnl").into_response(),
    }
}

async fn admin_page_inner(app: &App, headers: &HeaderMap) -> Response {
    match require_admin(app, headers).await {
        Ok(m) => html::admin(&m.email, &app.cfg.app_url).into_response(),
        Err(AppError::Unauthorized) => admin_login_response(app, headers),
        Err(_) => admin_login_response(app, headers),
    }
}

pub async fn admin_page(State(app): State<App>, headers: HeaderMap) -> Response {
    admin_page_inner(&app, &headers).await
}

pub async fn app_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], html::css())
}
pub async fn desk_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        html::desk_js(),
    )
}
pub async fn admin_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        html::admin_js(),
    )
}
pub async fn dash_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], html::dash_css())
}
pub async fn dash_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        html::dash_js(),
    )
}
pub async fn theme_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        html::theme_js(),
    )
}
pub async fn i18n_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        html::i18n_js(),
    )
}
pub async fn logo() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], html::logo())
}

pub async fn healthz() -> impl IntoResponse {
    Json(json!({ "ok": true, "service": "ohioism-trade" }))
}

#[derive(Deserialize)]
pub struct AuthBody {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub referral: String,
}

pub async fn signup(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<AuthBody>,
) -> AppResult<Response> {
    if is_admin_request(&app, &headers) {
        return Err(AppError::Forbidden("Create accounts on the trade site.".into()));
    }
    if !app
        .limiter
        .allow(&format!("signup:{}", client_ip(&headers)), 8, Duration::from_secs(60))
    {
        return Err(AppError::Rate);
    }
    let email = normalize_email(&body.email);
    if !valid_email(&email) || body.password.len() < 8 {
        return Err(AppError::BadRequest(
            "Enter a valid email and a password of at least 8 characters.".into(),
        ));
    }
    if app.find_member(&email).await?.is_some() {
        return Err(AppError::Conflict("An account with that email already exists.".into()));
    }
    let password = body.password.clone();
    let password_hash = tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;
    let admin = app.cfg.is_admin_email(&email);
    let member = Member {
        id: None,
        email: email.clone(),
        password_hash,
        admin,
        plan: if admin { "month".into() } else { "none".into() },
        access_until: if admin {
            Some(BsonDateTime::from_millis(
                (Utc::now() + ChronoDuration::days(3650)).timestamp_millis(),
            ))
        } else {
            None
        },
        blocked: false,
        halted: !admin,
        live: false,
        paper: admin,
        hedge: false,
        base_amount: HOUSE_BASE,
        max_net: HOUSE_NET,
        paper_cash_usd: Some(PAPER_CASH),
        deficit: 0,
        last_alloc: String::new(),
        trade_referral: normalize_email(&body.referral),
        trade_paid_usd: 0.0,
        keys: None,
        created_at: Some(BsonDateTime::from_millis(now_ms())),
        session_started_at: None,
        session_mode: String::new(),
        session_start_balance: None,
    };
    app.insert_member(&member).await?;
    let token = sign_session(&app.cfg, &email, admin)?;
    Ok(cookie_response(
        json!({ "member": app.public(&member) }),
        session_cookie(&app.cfg, &token),
    ))
}

pub async fn login(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<AuthBody>,
) -> AppResult<Response> {
    if !app
        .limiter
        .allow(&format!("login:{}", client_ip(&headers)), 12, Duration::from_secs(60))
    {
        return Err(AppError::Rate);
    }
    let email = normalize_email(&body.email);
    let member = app
        .find_member(&email)
        .await?
        .ok_or_else(|| AppError::BadRequest("Invalid email or password.".into()))?;
    let password = body.password.clone();
    let hash = member.password_hash.clone();
    let ok = tokio::task::spawn_blocking(move || verify_password(&password, &hash))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !ok {
        return Err(AppError::BadRequest("Invalid email or password.".into()));
    }
    if member.blocked {
        return Err(AppError::Forbidden("This account is blocked.".into()));
    }
    if is_admin_request(&app, &headers) && !member.admin {
        return Err(AppError::Forbidden("Admin accounts only on this host.".into()));
    }
    let token = sign_session(&app.cfg, &email, member.admin)?;
    Ok(cookie_response(
        json!({ "member": app.public(&member) }),
        session_cookie(&app.cfg, &token),
    ))
}

pub async fn logout(State(app): State<App>) -> Response {
    cookie_response(json!({ "ok": true }), clear_cookie(&app.cfg))
}

pub async fn me(State(app): State<App>, headers: HeaderMap) -> AppResult<Json<Value>> {
    let m = load_session(&app, &headers).await?;
    Ok(Json(json!({ "member": app.public(&m) })))
}

#[derive(Deserialize)]
pub struct PayBody {
    pub plan: String,
}

pub async fn pay(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<PayBody>,
) -> AppResult<Json<Value>> {
    let member = load_session(&app, &headers).await?;
    if !app
        .limiter
        .allow(&format!("pay:{}", member.email), 6, Duration::from_secs(60))
    {
        return Err(AppError::Rate);
    }
    let sku = Sku::parse(&body.plan).ok_or_else(|| AppError::BadRequest("Choose test or month.".into()))?;
    if sku == Sku::Test && member.active(now_ms()) && member.plan == "month" {
        return Err(AppError::BadRequest("Monthly access is already active.".into()));
    }
    if sku == Sku::Test && member.active(now_ms()) && member.plan == "test" {
        return Err(AppError::BadRequest("Test window is already running.".into()));
    }
    let amount = match sku {
        Sku::Test => TEST_USD,
        Sku::Month => MONTH_USD,
    };
    let order_id = format!("tr_{}", Uuid::new_v4());
    let partner = (amount * app.cfg.partner_share * 100.0).round() / 100.0;
    let success = format!("{}/desk?paid=1&order={order_id}", app.cfg.app_url);
    let cancel = format!("{}/desk?canceled=1", app.cfg.app_url);
    let ipn = nowpayments::ipn_callback_url(&app.cfg.app_url);
    let invoice = nowpayments::create_invoice(
        &app.http,
        &app.cfg.nowpayments_api_key,
        amount,
        &order_id,
        &format!("OhioismQuant Trade {} — {}", sku.as_str(), member.email),
        &success,
        &cancel,
        &ipn,
    )
    .await?;
    let now = BsonDateTime::from_millis(now_ms());
    app.insert_order(&Order {
        id: None,
        order_id: order_id.clone(),
        email: member.email.clone(),
        plan: sku.as_str().into(),
        amount_usd: amount,
        partner_usd: partner,
        referral: member.trade_referral.clone(),
        nowpayments_invoice_id: invoice.id.clone(),
        invoice_url: invoice.url.clone(),
        payment_id: String::new(),
        status: "waiting".into(),
        pay_address: String::new(),
        pay_amount: String::new(),
        pay_currency: String::new(),
        created_at: Some(now),
        updated_at: Some(now),
        paid_at: None,
    })
    .await?;
    Ok(Json(json!({ "invoiceUrl": invoice.url, "orderId": order_id })))
}

pub async fn pay_ipn(State(app): State<App>, headers: HeaderMap, body: Bytes) -> AppResult<Json<Value>> {
    let raw = String::from_utf8_lossy(&body);
    let sig = headers
        .get("x-nowpayments-sig")
        .and_then(|v| v.to_str().ok());
    if !nowpayments::verify_ipn(&raw, sig, &app.cfg.nowpayments_ipn_secret) {
        return Err(AppError::Forbidden("Invalid signature".into()));
    }
    let payload: Value =
        serde_json::from_str(&raw).map_err(|_| AppError::BadRequest("bad json".into()))?;
    let order_id = payload
        .get("order_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if order_id.is_empty() {
        return Err(AppError::BadRequest("Missing order".into()));
    }
    let order = app
        .find_order(&order_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Unknown order".into()))?;
    let status = payload
        .get("payment_status")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    let first = app
        .mark_paid_if_new(
            &order_id,
            &status,
            &val_str(&payload, "payment_id"),
            &val_str(&payload, "pay_address"),
            &val_str(&payload, "pay_amount"),
            &val_str(&payload, "pay_currency"),
            &val_str(&payload, "invoice_id"),
        )
        .await?;
    if first && nowpayments::unlocks(&status) {
        if let Some(sku) = Sku::parse(&order.plan) {
            app.grant_sku(&order.email, sku, order.amount_usd).await?;
            telegram::notify(
                &app.http,
                &app.cfg.telegram_bot_token,
                &app.cfg.telegram_chat_id,
                &format!(
                    "Trade paid ${:.0} {} {}",
                    order.amount_usd,
                    sku.as_str(),
                    order.email
                ),
            )
            .await;
        }
    }
    Ok(Json(json!({ "ok": true })))
}

fn val_str(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string().trim_matches('"').to_string(),
        None => String::new(),
    }
}

pub async fn pay_sync(State(app): State<App>, headers: HeaderMap) -> AppResult<Json<Value>> {
    let member = load_session(&app, &headers).await?;
    let mut cur = app.orders().find(doc! { "email": &member.email }, None).await?;
    use futures_util::TryStreamExt;
    let mut granted = false;
    while let Some(order) = cur.try_next().await? {
        if order.paid_at.is_some() {
            continue;
        }
        let status = nowpayments::payment_status(
            &app.http,
            &app.cfg.nowpayments_api_key,
            &order.order_id,
            &order.nowpayments_invoice_id,
            &order.payment_id,
        )
        .await
        .ok()
        .flatten()
        .unwrap_or(order.status.clone());
        if !nowpayments::unlocks(&status) {
            continue;
        }
        let first = app
            .mark_paid_if_new(
                &order.order_id,
                &status,
                &order.payment_id,
                "",
                "",
                "",
                &order.nowpayments_invoice_id,
            )
            .await?;
        if first {
            if let Some(sku) = Sku::parse(&order.plan) {
                app.grant_sku(&order.email, sku, order.amount_usd).await?;
                granted = true;
            }
        }
    }
    let updated = app.require_member_email(&member.email).await?;
    Ok(Json(json!({ "granted": granted, "member": app.public(&updated) })))
}

#[derive(Deserialize)]
pub struct KeysBody {
    pub funder: String,
    #[serde(rename = "apiKey")]
    pub api_key: String,
    pub secret: String,
    #[serde(default)]
    pub passphrase: String,
    #[serde(default, rename = "privateKey")]
    pub private_key: String,
    #[serde(default, rename = "sigType")]
    pub sig_type: i32,
}

fn normalize_private_key(raw: &str) -> AppResult<String> {
    let hex = raw
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::BadRequest(
            "Wallet private key must be 64 hex characters.".into(),
        ));
    }
    Ok(format!("0x{}", hex.to_ascii_lowercase()))
}

pub async fn save_keys(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<KeysBody>,
) -> AppResult<Json<Value>> {
    let member = load_session(&app, &headers).await?;
    require_vip(&member)?;
    if app.cfg.key_wrap_secret.is_empty() {
        return Err(AppError::Internal("KEY_WRAP_SECRET is not set".into()));
    }
    if body.funder.trim().len() < 8 || body.api_key.trim().is_empty() || body.secret.trim().is_empty()
    {
        return Err(AppError::BadRequest("Funder, API key, and secret are required.".into()));
    }
    let key = wrap_key(&app.cfg.key_wrap_secret).map_err(|e| AppError::Internal(e.to_string()))?;
    let api_key_enc = seal(&key, body.api_key.trim()).map_err(|e| AppError::Internal(e.to_string()))?;
    let secret_enc = seal(&key, body.secret.trim()).map_err(|e| AppError::Internal(e.to_string()))?;
    let pass_enc = if body.passphrase.trim().is_empty() {
        member
            .keys
            .as_ref()
            .map(|k| k.pass_enc.clone())
            .unwrap_or_default()
    } else {
        seal(&key, body.passphrase.trim()).map_err(|e| AppError::Internal(e.to_string()))?
    };
    let key_enc = if body.private_key.trim().is_empty() {
        member
            .keys
            .as_ref()
            .map(|k| k.key_enc.clone())
            .unwrap_or_default()
    } else {
        let pk = normalize_private_key(&body.private_key)?;
        seal(&key, &pk).map_err(|e| AppError::Internal(e.to_string()))?
    };
    let keys = KeysDoc {
        funder: body.funder.trim().to_string(),
        api_key: String::new(),
        api_key_enc,
        secret_enc,
        pass_enc,
        key_enc,
        sig_type: body.sig_type,
        connected_at: Some(BsonDateTime::from_millis(now_ms())),
    };
    app.members()
        .update_one(
            doc! { "email": &member.email },
            doc! { "$set": { "keys": mongodb::bson::to_bson(&keys).map_err(|e| AppError::Internal(e.to_string()))? } },
            None,
        )
        .await?;
    let live_was_on = member.run_flags().live_running();
    let updated = if live_was_on {
        apply_run(&app, &member, "live", false).await?
    } else {
        app.require_member_email(&member.email).await?
    };
    Ok(Json(json!({
        "member": app.public(&updated),
        "restartLive": live_was_on,
    })))
}

fn run_set_doc(flags: crate::access::RunFlags) -> mongodb::bson::Document {
    doc! {
        "halted": flags.halted,
        "paper": flags.paper,
        "live": flags.live,
    }
}

async fn gate_live_start(app: &App, member: &crate::models::Member) -> AppResult<()> {
    if !member.active(now_ms()) {
        return Err(AppError::BadRequest("Subscribe first.".into()));
    }
    if !member.keys_connected() {
        return Err(AppError::BadRequest("Connect Polymarket API keys first.".into()));
    }
    let need = capital::fund_for_base(member.base_amount.max(MIN_BASE));
    let funder = member
        .keys
        .as_ref()
        .map(|k| k.funder.as_str())
        .unwrap_or("");
    let have = crate::poly::funder_value(&app.http, funder)
        .await
        .map_err(AppError::BadRequest)?;
    if have + 1e-9 < need {
        return Err(AppError::FundShort { need, have });
    }
    if !member
        .keys
        .as_ref()
        .is_some_and(|k| !k.key_enc.is_empty())
    {
        return Err(AppError::BadRequest(
            "Paste your wallet private key on the desk, then start live.".into(),
        ));
    }
    Ok(())
}

async fn apply_run(
    app: &App,
    member: &crate::models::Member,
    mode: &str,
    running: bool,
) -> AppResult<crate::models::Member> {
    if running {
        require_vip(member)?;
    }
    if mode == "live" && running {
        gate_live_start(app, member).await?;
    }
    let flags = next_run(mode, running).map_err(|e| AppError::BadRequest(e.into()))?;
    let mut set = run_set_doc(flags);
    if running {
        set.insert(
            "sessionStartedAt",
            BsonDateTime::from_millis(now_ms()),
        );
        set.insert("sessionMode", if mode == "live" { "live" } else { "paper" });
        if mode == "live" {
            if let Some(funder) = member
                .keys
                .as_ref()
                .map(|k| k.funder.clone())
                .filter(|s| !s.is_empty())
            {
                if let Ok(book) = crate::poly::funder_book(&app.http, &funder).await {
                    set.insert("sessionStartBalance", book.balance);
                }
            }
        } else {
            set.insert("sessionStartBalance", mongodb::bson::Bson::Null);
        }
    }
    app.members()
        .update_one(doc! { "email": &member.email }, doc! { "$set": set }, None)
        .await?;
    app.require_member_email(&member.email).await
}

#[derive(Deserialize)]
pub struct LiveBody {
    pub live: bool,
}

pub async fn set_live(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<LiveBody>,
) -> AppResult<Json<Value>> {
    let member = load_session(&app, &headers).await?;
    let updated = if body.live {
        apply_run(&app, &member, "live", true).await?
    } else {
        apply_run(&app, &member, "paper", true).await?
    };
    Ok(Json(json!({ "member": app.public(&updated) })))
}

#[derive(Deserialize)]
pub struct HaltBody {
    pub halted: bool,
}

pub async fn set_halt(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<HaltBody>,
) -> AppResult<Json<Value>> {
    let member = load_session(&app, &headers).await?;
    let updated = if body.halted {
        apply_run(&app, &member, "paper", false).await?
    } else {
        apply_run(&app, &member, "paper", true).await?
    };
    Ok(Json(json!({ "member": app.public(&updated) })))
}

#[derive(Deserialize)]
pub struct RunBody {
    pub mode: String,
    pub running: bool,
}

pub async fn set_run(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<RunBody>,
) -> AppResult<Json<Value>> {
    let member = load_session(&app, &headers).await?;
    let updated = apply_run(&app, &member, &body.mode, body.running).await?;
    Ok(Json(json!({ "member": app.public(&updated) })))
}

#[derive(Deserialize)]
pub struct SettingsBody {
    #[serde(default, rename = "baseAmount")]
    pub base_amount: Option<f64>,
    #[serde(default, rename = "maxNet")]
    pub max_net: Option<f64>,
    #[serde(default)]
    pub hedge: Option<bool>,
    #[serde(default, rename = "houseBest")]
    pub house_best: bool,
}

pub async fn save_settings(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<SettingsBody>,
) -> AppResult<Json<Value>> {
    let member = load_session(&app, &headers).await?;
    require_vip(&member)?;
    let _ = body.house_best;
    let raw = body.base_amount.filter(|b| b.is_finite() && *b > 0.0).unwrap_or(MIN_BASE);
    let base = raw.max(MIN_BASE);
    let max_net = capital::net_for_base(&member.plan, base);
    let (base, max_net, hedge) = capital::clamp_settings(
        &member.plan,
        base,
        max_net,
        body.hedge.unwrap_or(member.hedge),
    )
    .map_err(AppError::BadRequest)?;
    app.members()
        .update_one(
            doc! { "email": &member.email },
            doc! { "$set": {
                "baseAmount": base,
                "maxNet": max_net,
                "hedge": hedge,
            }},
            None,
        )
        .await?;
    let updated = app.require_member_email(&member.email).await?;
    let house = app.house_snap().await.unwrap_or_default();
    Ok(Json(json!({
        "member": app.public(&updated),
        "guide": desk_guide(&updated, &house),
    })))
}

fn desk_guide(member: &crate::models::Member, house: &crate::models::Snap) -> Value {
    let caps = capital::caps_for_plan(&member.plan);
    let best = house_best(&member.plan);
    let sheet = capital::sheet(
        member.base_amount,
        member.max_net,
        member.hedge,
        house.mark_pnl,
        caps.base,
    );
    let ladder = capital::ladder(
        &member.plan,
        member.base_amount,
        member.max_net,
        member.hedge,
        house.mark_pnl,
    );
    json!({
        "caps": caps,
        "houseBest": best,
        "capital": sheet,
        "ladder": ladder,
    })
}

pub async fn desk(State(app): State<App>, headers: HeaderMap) -> AppResult<Json<Value>> {
    let member = load_session(&app, &headers).await?;
    let snap = app.snap_for(&member.email).await?;
    let house = app.house_snap().await?;
    let fills = app.recent_fills(&member.email, 20).await.unwrap_or_default();
    Ok(Json(json!({
        "member": app.public(&member),
        "snap": snap,
        "house": house,
        "fills": fills,
        "guide": desk_guide(&member, &house),
    })))
}

pub async fn desk_stream(
    State(app): State<App>,
    headers: HeaderMap,
) -> AppResult<Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>>> {
    let member = load_session(&app, &headers).await?;
    let email = member.email.clone();
    let stream = unfold((app, email), |(app, email)| async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let member = app.find_member(&email).await.ok().flatten();
        let snap = app.snap_for(&email).await.ok();
        let house = app.house_snap().await.ok();
        let payload = json!({
            "member": member.as_ref().map(|m| app.public(m)),
            "snap": snap,
            "house": house,
            "guide": member.as_ref().zip(house.as_ref()).map(|(m, h)| desk_guide(m, h)),
        });
        let ev = SseEvent::default().json_data(payload).unwrap_or_default();
        Some((Ok(ev), (app, email)))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}

fn round2(n: f64) -> f64 {
    (n * 100.0).round() / 100.0
}

fn tick_json(t: &crate::models::Tick) -> Value {
    json!({
        "slug": t.slug,
        "ts": t.ts_ms,
        "binance": t.binance,
        "chainlink": t.chainlink,
        "twap": t.twap,
        "s_hat": t.s_hat,
        "yes_bid": t.yes_bid,
        "yes_ask": t.yes_ask,
        "no_bid": t.no_bid,
        "no_ask": t.no_ask,
        "tau_s": t.tau_s,
        "quote": t.quote,
        "regime": t.regime,
        "k_t": t.k_t,
        "fair": t.fair,
        "utc0_ms": t.utc0_ms,
        "utc1_ms": t.utc1_ms,
    })
}

fn snap_tick(s: &crate::models::Snap) -> Value {
    let no_bid = if s.no_bid > 0.0 {
        s.no_bid
    } else if s.yes_ask > 0.0 {
        (1.0 - s.yes_ask).max(0.0)
    } else {
        0.0
    };
    let no_ask = if s.no_ask > 0.0 {
        s.no_ask
    } else if s.yes_bid > 0.0 {
        (1.0 - s.yes_bid).max(0.0)
    } else {
        0.0
    };
    json!({
        "slug": s.slug,
        "ts": s.ts_ms,
        "binance": s.binance,
        "chainlink": s.chainlink,
        "twap": s.twap,
        "s_hat": s.s_hat,
        "yes_bid": s.yes_bid,
        "yes_ask": s.yes_ask,
        "no_bid": no_bid,
        "no_ask": no_ask,
        "tau_s": s.tau_s,
        "quote": s.quote,
        "regime": s.regime,
        "k_t": s.k_t,
        "fair": s.fair,
        "utc0_ms": s.utc0_ms,
        "utc1_ms": s.utc1_ms,
        "net": s.net,
        "mark_pnl": s.mark_pnl,
    })
}

#[derive(Deserialize)]
pub struct HistoryQ {
    pub slug: Option<String>,
    pub limit: Option<i64>,
}

pub async fn api_history(
    State(app): State<App>,
    headers: HeaderMap,
    Query(q): Query<HistoryQ>,
) -> AppResult<Json<Value>> {
    let member = load_session(&app, &headers).await?;
    require_vip(&member)?;
    let mut slug = q.slug.unwrap_or_default();
    if slug.is_empty() {
        if let Ok(m) = crate::quotes::current_market("btc", 15).await {
            slug = m.slug;
        } else if let Ok(Some(t)) = app.last_tick().await {
            slug = t.slug;
        } else if let Ok(h) = app.house_snap().await {
            slug = h.slug;
        }
    }
    let limit = q.limit.unwrap_or(900).clamp(1, 2000);
    let mut ticks = app.recent_ticks(&slug, limit).await?;
    if ticks.len() < 8 {
        let all = app.recent_ticks("", limit).await?;
        if all.len() > ticks.len() {
            ticks = all;
        }
    }
    Ok(Json(json!({
        "slug": slug,
        "ticks": ticks.iter().map(tick_json).collect::<Vec<_>>(),
    })))
}

pub async fn api_last(State(app): State<App>, headers: HeaderMap) -> AppResult<Json<Value>> {
    let member = load_session(&app, &headers).await?;
    require_vip(&member)?;
    let snap = app.snap_for(&member.email).await.unwrap_or_default();
    let mut tick = match app.last_tick().await {
        Ok(Some(t)) => tick_json(&t),
        Ok(None) => snap_tick(&app.house_snap().await.unwrap_or_default()),
        Err(_) => serde_json::json!({}),
    };
    if let Ok(o) = crate::quotes::cached_overlay(&app.http, "btc", 15, "btcusdt").await {
        crate::quotes::merge_tick(&mut tick, &o);
    }
    let fills = app.recent_fills(&member.email, 12).await.unwrap_or_default();
    Ok(Json(json!({
        "tick": tick,
        "snap": snap,
        "fills": fills,
        "member": app.public(&member),
    })))
}

fn signer_on_file(member: &crate::models::Member) -> bool {
    member.keys.as_ref().is_some_and(|k| !k.key_enc.is_empty())
}

/// Live PnL only when the coordinator actually posts live. Live-without-signer is paper.
fn pnl_session_mode(member: &crate::models::Member) -> String {
    let flags = member.run_flags();
    let live_ok = signer_on_file(member);
    if flags.live_running() && live_ok {
        "live".into()
    } else if flags.paper_running() || flags.live_running() {
        "paper".into()
    } else if member.session_mode == "live" && live_ok {
        "live".into()
    } else if member.session_mode == "live" || member.session_mode == "paper" {
        member.session_mode.clone()
    } else {
        "paper".into()
    }
}

async fn session_bounds(_app: &crate::db::App, member: &crate::models::Member) -> (String, i64) {
    (pnl_session_mode(member), member.session_started_ms())
}

pub async fn api_pnl(State(app): State<App>, headers: HeaderMap) -> AppResult<Json<Value>> {
    let member = load_session(&app, &headers).await?;
    require_vip(&member)?;
    let (session_mode, session_start) = session_bounds(&app, &member).await;
    let all = app.recent_results(&member.email, 800).await.unwrap_or_default();
    let results: Vec<_> = all
        .into_iter()
        .filter(|r| r.ts_ms >= session_start)
        .filter(|r| if session_mode == "live" { !r.paper } else { r.paper })
        .collect();
    let snap = app.snap_for(&member.email).await.unwrap_or_default();
    let mut days: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut equity = Vec::new();
    let mut realized = 0.0;
    let mut chron: Vec<_> = results.iter().collect();
    chron.sort_by_key(|r| r.ts_ms);
    if session_start > 0 {
        equity.push(json!({ "ts": session_start, "pnl": 0.0, "cum": 0.0, "slug": "" }));
    }
    for r in &chron {
        realized += r.pnl;
        let day = chrono::DateTime::from_timestamp_millis(r.ts_ms)
            .map(|t| t.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "unknown".into());
        let slot = days.entry(day.clone()).or_insert_with(|| json!({ "pnl": 0.0, "n": 0 }));
        if let Some(obj) = slot.as_object_mut() {
            let pnl = obj.get("pnl").and_then(|x| x.as_f64()).unwrap_or(0.0) + r.pnl;
            let n = obj.get("n").and_then(|x| x.as_i64()).unwrap_or(0) + 1;
            obj.insert("pnl".into(), json!(round2(pnl)));
            obj.insert("n".into(), json!(n));
        }
        equity.push(json!({
            "ts": r.ts_ms,
            "pnl": round2(r.pnl),
            "cum": round2(realized),
            "slug": r.slug,
        }));
    }
    let mark = snap.mark_pnl;
    let total = realized + mark;
    let flags = member.run_flags();
    if flags.paper_running() || flags.live_running() {
        equity.push(json!({
            "ts": now_ms(),
            "pnl": round2(mark),
            "cum": round2(total),
            "slug": snap.slug,
            "mark": true,
        }));
    }
    let fills = if session_mode == "live" {
        app.recent_fills(&member.email, 80)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|f| f.ts_ms >= session_start && !f.paper)
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    Ok(Json(json!({
        "mode": session_mode,
        "sessionStartedAt": session_start,
        "realizedPnl": round2(realized),
        "markPnl": round2(mark),
        "pnl": round2(total),
        "results": results,
        "fills": fills,
        "days": days,
        "equity": equity,
    })))
}

pub async fn api_balance(State(app): State<App>, headers: HeaderMap) -> AppResult<Json<Value>> {
    let member = load_session(&app, &headers).await?;
    require_vip(&member)?;
    let snap = app.snap_for(&member.email).await.unwrap_or_default();
    let mark = snap.mark_pnl;
    let (session_mode, session_start) = session_bounds(&app, &member).await;
    let realized = app
        .recent_results(&member.email, 800)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.ts_ms >= session_start)
        .filter(|r| if session_mode == "live" { !r.paper } else { r.paper })
        .map(|r| r.pnl)
        .sum::<f64>();
    let pnl = round2(realized + mark);
    let need = capital::fund_for_base(member.base_amount.max(MIN_BASE));
    if session_mode == "live" && member.keys_connected() {
        let funder = member.keys.as_ref().map(|k| k.funder.clone()).unwrap_or_default();
        match crate::poly::funder_book(&app.http, &funder).await {
            Ok(book) => {
                let start_bal = member.session_start_balance.unwrap_or(0.0);
                let wallet_pnl = if start_bal > 0.0 {
                    Some(round2(book.balance - start_bal))
                } else {
                    None
                };
                return Ok(Json(json!({
                    "paper": false,
                    "mode": "live",
                    "cash": round2(book.cash),
                    "balance": round2(book.balance),
                    "portfolio": round2(book.balance),
                    "pnl": pnl,
                    "realizedPnl": round2(realized),
                    "markPnl": round2(mark),
                    "walletPnl": wallet_pnl,
                    "need": need,
                })));
            }
            Err(e) => tracing::warn!(error = %e, "funder book"),
        }
    }
    Ok(Json(json!({
        "paper": true,
        "mode": "paper",
        "cash": None::<f64>,
        "balance": None::<f64>,
        "portfolio": None::<f64>,
        "pnl": pnl,
        "realizedPnl": round2(realized),
        "markPnl": round2(mark),
        "need": need,
    })))
}

pub async fn admin_overview(State(app): State<App>, headers: HeaderMap) -> AppResult<Json<Value>> {
    require_admin(&app, &headers).await?;
    let now = BsonDateTime::from_millis(now_ms());
    let soon = BsonDateTime::from_millis(now_ms() + 48 * 3600 * 1000);
    let members = app.members();
    let active = members
        .count_documents(doc! { "accessUntil": { "$gt": now }, "admin": { "$ne": true } }, None)
        .await?;
    let test = members
        .count_documents(
            doc! { "plan": "test", "accessUntil": { "$gt": now }, "admin": { "$ne": true } },
            None,
        )
        .await?;
    let month = members
        .count_documents(
            doc! { "plan": "month", "accessUntil": { "$gt": now }, "admin": { "$ne": true } },
            None,
        )
        .await?;
    let live = members
        .count_documents(
            doc! { "live": true, "accessUntil": { "$gt": now }, "admin": { "$ne": true } },
            None,
        )
        .await?;
    let paper = members
        .count_documents(
            doc! { "paper": true, "live": { "$ne": true }, "accessUntil": { "$gt": now }, "admin": { "$ne": true } },
            None,
        )
        .await?;
    let halted = members
        .count_documents(doc! { "halted": true, "accessUntil": { "$gt": now }, "admin": { "$ne": true } }, None)
        .await?;
    let keys = members
        .count_documents(
            doc! { "keys.secretEnc": { "$exists": true, "$ne": "" }, "admin": { "$ne": true } },
            None,
        )
        .await?;
    let expiring = members
        .count_documents(
            doc! { "accessUntil": { "$gt": now, "$lt": soon }, "admin": { "$ne": true } },
            None,
        )
        .await?;
    use futures_util::TryStreamExt;
    let mut paid = app
        .orders()
        .find(doc! { "paidAt": { "$ne": null } }, None)
        .await?;
    let mut gross = 0.0;
    let mut partner = 0.0;
    let mut gross_test = 0.0;
    let mut partner_test = 0.0;
    let mut gross_month = 0.0;
    let mut partner_month = 0.0;
    let mut paid_n = 0u64;
    while let Some(o) = paid.try_next().await? {
        paid_n += 1;
        gross += o.amount_usd;
        partner += o.partner_usd;
        if o.plan == "test" {
            gross_test += o.amount_usd;
            partner_test += o.partner_usd;
        } else if o.plan == "month" {
            gross_month += o.amount_usd;
            partner_month += o.partner_usd;
        }
    }
    let waiting = app
        .orders()
        .count_documents(doc! { "paidAt": { "$eq": null } }, None)
        .await?;
    let house = app.house_snap().await.unwrap_or_default();
    Ok(Json(json!({
        "active": active,
        "test": test,
        "month": month,
        "live": live,
        "paper": paper,
        "halted": halted,
        "keysConnected": keys,
        "expiring48h": expiring,
        "grossUsd": round2(gross),
        "partnerUsd": round2(partner),
        "grossTestUsd": round2(gross_test),
        "partnerTestUsd": round2(partner_test),
        "grossMonthUsd": round2(gross_month),
        "partnerMonthUsd": round2(partner_month),
        "paidOrders": paid_n,
        "waitingOrders": waiting,
        "partnerShare": app.cfg.partner_share,
        "house": house,
    })))
}

pub async fn admin_orders(State(app): State<App>, headers: HeaderMap) -> AppResult<Json<Value>> {
    require_admin(&app, &headers).await?;
    use futures_util::TryStreamExt;
    let opts = mongodb::options::FindOptions::builder()
        .sort(doc! { "createdAt": -1 })
        .limit(250)
        .build();
    let mut cur = app.orders().find(doc! {}, opts).await?;
    let mut out = Vec::new();
    while let Some(o) = cur.try_next().await? {
        out.push(PublicOrder::from_order(&o));
        if out.len() >= 250 {
            break;
        }
    }
    Ok(Json(json!({ "orders": out })))
}

pub async fn admin_members(State(app): State<App>, headers: HeaderMap) -> AppResult<Json<Value>> {
    require_admin(&app, &headers).await?;
    use futures_util::TryStreamExt;
    let mut cur = app.members().find(doc! {}, None).await?;
    let mut out = Vec::new();
    while let Some(m) = cur.try_next().await? {
        if !m.admin {
            out.push(app.public(&m));
        }
        if out.len() >= 400 {
            break;
        }
    }
    Ok(Json(json!({ "members": out })))
}

#[derive(Deserialize)]
pub struct GiftBody {
    pub email: String,
    pub days: i64,
}

pub async fn admin_gift(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<GiftBody>,
) -> AppResult<Json<Value>> {
    require_admin(&app, &headers).await?;
    if body.days < 1 || body.days > 365 {
        return Err(AppError::BadRequest("days 1–365".into()));
    }
    let email = normalize_email(&body.email);
    let member = app
        .find_member(&email)
        .await?
        .ok_or_else(|| AppError::NotFound("member".into()))?;
    let now = Utc::now();
    let current = member
        .access_until
        .and_then(|d| Utc.timestamp_millis_opt(d.timestamp_millis()).single())
        .filter(|u| *u > now)
        .unwrap_or(now);
    let until = current + ChronoDuration::days(body.days);
    let plan = if member.plan == "month" || member.plan == "test" {
        member.plan.clone()
    } else {
        "month".into()
    };
    app.members()
        .update_one(
            doc! { "email": &email },
            doc! { "$set": {
                "plan": plan,
                "accessUntil": BsonDateTime::from_millis(until.timestamp_millis()),
                "paper": true,
                "live": false,
                "halted": false,
                "baseAmount": HOUSE_BASE,
                "maxNet": HOUSE_NET,
                "hedge": false,
                "paperCashUsd": PAPER_CASH,
            }},
            None,
        )
        .await?;
    let updated = app.require_member_email(&email).await?;
    Ok(Json(json!({ "member": app.public(&updated) })))
}

pub async fn admin_halt_all(State(app): State<App>, headers: HeaderMap) -> AppResult<Json<Value>> {
    require_admin(&app, &headers).await?;
    let r = app
        .members()
        .update_many(
            doc! { "admin": { "$ne": true } },
            doc! { "$set": run_set_doc(crate::access::RunFlags::stopped()) },
            None,
        )
        .await?;
    telegram::notify(
        &app.http,
        &app.cfg.telegram_bot_token,
        &app.cfg.telegram_chat_id,
        &format!("HALT ALL traders matched={}", r.matched_count),
    )
    .await;
    Ok(Json(json!({ "ok": true, "matched": r.matched_count })))
}

#[derive(Deserialize)]
pub struct AdminHaltBody {
    pub email: String,
    pub halted: bool,
}

pub async fn admin_halt_one(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<AdminHaltBody>,
) -> AppResult<Json<Value>> {
    require_admin(&app, &headers).await?;
    let email = normalize_email(&body.email);
    let flags = if body.halted {
        crate::access::RunFlags::stopped()
    } else {
        crate::access::RunFlags::paper()
    };
    let set = run_set_doc(flags);
    app.members()
        .update_one(
            doc! { "email": email, "admin": { "$ne": true } },
            doc! { "$set": set },
            None,
        )
        .await?;
    Ok(Json(json!({ "ok": true })))
}

pub fn router(app: App) -> axum::Router {
    axum::Router::new()
        .route("/", axum::routing::get(landing))
        .route("/login", axum::routing::get(login_page))
        .route("/desk", axum::routing::get(desk_page))
        .route("/dash", axum::routing::get(dash_page))
        .route("/dashboard", axum::routing::get(dash_page))
        .route("/pnl", axum::routing::get(pnl_page))
        .route("/admin", axum::routing::get(admin_page))
        .route("/app.css", axum::routing::get(app_css))
        .route("/dash.css", axum::routing::get(dash_css))
        .route("/desk.js", axum::routing::get(desk_js))
        .route("/dash.js", axum::routing::get(dash_js))
        .route("/theme.js", axum::routing::get(theme_js))
        .route("/i18n.js", axum::routing::get(i18n_js))
        .route("/admin.js", axum::routing::get(admin_js))
        .route("/ohioismquant-logo.png", axum::routing::get(logo))
        .route("/ohioismquant-logo.svg", axum::routing::get(logo))
        .route("/healthz", axum::routing::get(healthz))
        .route("/api/auth/signup", axum::routing::post(signup))
        .route("/api/auth/login", axum::routing::post(login))
        .route("/api/auth/logout", axum::routing::post(logout))
        .route("/api/me", axum::routing::get(me))
        .route("/api/pay", axum::routing::post(pay))
        .route("/api/pay/ipn", axum::routing::post(pay_ipn))
        .route("/api/pay/sync", axum::routing::post(pay_sync))
        .route("/api/keys", axum::routing::post(save_keys))
        .route("/api/live", axum::routing::post(set_live))
        .route("/api/halt", axum::routing::post(set_halt))
        .route("/api/run", axum::routing::post(set_run))
        .route("/api/settings", axum::routing::post(save_settings))
        .route("/api/desk", axum::routing::get(desk))
        .route("/api/desk/stream", axum::routing::get(desk_stream))
        .route("/api/history", axum::routing::get(api_history))
        .route("/api/last", axum::routing::get(api_last))
        .route("/api/pnl", axum::routing::get(api_pnl))
        .route("/api/balance", axum::routing::get(api_balance))
        .route("/api/admin/overview", axum::routing::get(admin_overview))
        .route("/api/admin/members", axum::routing::get(admin_members))
        .route("/api/admin/orders", axum::routing::get(admin_orders))
        .route("/api/admin/gift", axum::routing::post(admin_gift))
        .route("/api/admin/halt-all", axum::routing::post(admin_halt_all))
        .route("/api/admin/halt", axum::routing::post(admin_halt_one))
        .with_state(app)
}
