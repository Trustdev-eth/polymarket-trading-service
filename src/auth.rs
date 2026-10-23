use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::http::{header, HeaderMap};
use cookie::time::Duration as CookieDuration;
use cookie::{Cookie, SameSite};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::config::{Config, COOKIE_NAME};
use crate::error::{AppError, AppResult};

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    admin: bool,
    exp: usize,
}

pub fn hash_password(password: &str) -> AppResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Internal(e.to_string()))
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

pub fn sign_session(cfg: &Config, email: &str, admin: bool) -> AppResult<String> {
    let exp = jsonwebtoken::get_current_timestamp() as usize + 14 * 24 * 3600;
    let claims = Claims {
        sub: email.to_string(),
        admin,
        exp,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(cfg.auth_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(e.to_string()))
}

pub fn email_from_headers(cfg: &Config, headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    let token = raw.split(';').find_map(|part| {
        let part = part.trim();
        part.strip_prefix(&format!("{COOKIE_NAME}="))
            .map(|v| v.to_string())
    })?;
    let data = decode::<Claims>(
        &token,
        &DecodingKey::from_secret(cfg.auth_secret.as_bytes()),
        &Validation::default(),
    )
    .ok()?;
    Some(data.claims.sub)
}

pub fn request_host(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

pub fn session_cookie(cfg: &Config, token: &str) -> String {
    let mut builder = Cookie::build((COOKIE_NAME, token))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .secure(cfg.cookie_secure)
        .max_age(CookieDuration::days(14));
    if !cfg.cookie_domain.is_empty() {
        builder = builder.domain(cfg.cookie_domain.clone());
    }
    builder.to_string()
}

pub fn clear_cookie(cfg: &Config) -> String {
    let mut builder = Cookie::build((COOKIE_NAME, ""))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .secure(cfg.cookie_secure)
        .max_age(CookieDuration::ZERO);
    if !cfg.cookie_domain.is_empty() {
        builder = builder.domain(cfg.cookie_domain.clone());
    }
    builder.to_string()
}

pub fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local".into())
}

pub struct RateLimit {
    inner: Mutex<HashMap<String, Vec<Instant>>>,
}

impl RateLimit {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn allow(&self, key: &str, n: usize, window: Duration) -> bool {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap();
        let hits = map.entry(key.to_string()).or_default();
        hits.retain(|t| now.duration_since(*t) < window);
        if hits.len() >= n {
            return false;
        }
        hits.push(now);
        true
    }
}

pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

pub fn valid_email(email: &str) -> bool {
    let e = email.trim();
    e.contains('@') && e.contains('.') && !e.contains(' ') && e.len() < 200
}
