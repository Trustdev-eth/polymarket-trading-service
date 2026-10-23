use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("Sign in first.")]
    Unauthorized,
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NotFound(String),
    #[error("Too many attempts. Try again shortly.")]
    Rate,
    #[error("{0}")]
    Pay(String),
    #[error("Necessary fund ${need:.0}. Wallet shows ${have:.0}. Deposit more pUSD before live.")]
    FundShort { need: f64, have: f64 },
    #[error("{0}")]
    Internal(String),
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Internal(e.to_string())
    }
}

impl From<mongodb::error::Error> for AppError {
    fn from(e: mongodb::error::Error) -> Self {
        AppError::Internal(e.to_string())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::BadRequest(_) | AppError::FundShort { .. } => StatusCode::BAD_REQUEST,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::Rate => StatusCode::TOO_MANY_REQUESTS,
            AppError::Pay(_) => StatusCode::BAD_GATEWAY,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let msg = self.to_string();
        if matches!(self, AppError::Internal(_)) {
            tracing::error!(error = %msg, "internal");
        }
        let body = match &self {
            AppError::FundShort { need, have } => json!({ "error": msg, "need": need, "have": have }),
            _ => json!({ "error": msg }),
        };
        (status, Json(body)).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
