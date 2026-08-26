//! The HTTP surface: exactly the endpoints `docs/signing.md` specifies, plus
//! health and metrics.
//!
//! # Every response carries its request id
//!
//! Including refusals — especially refusals. An operator handed a
//! `SigningError` from an agent needs to find the matching audit record, and
//! the only thing the two have in common is this header.
//!
//! # Health endpoints are unauthenticated and say nothing
//!
//! A load balancer has to reach them, which means anyone can. They report
//! liveness and backend reachability and nothing about identities, keys or
//! policy.
//!
//! # Metrics are not exposed on the public listener
//!
//! `/metrics` describes signing volume and refusal patterns — useful to an
//! operator and equally useful to someone probing what a policy will accept.
//! It is bound separately; see [`metrics_router`].

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::audit::Operation;
use crate::auth::{bearer_token, UnixSeconds};
use crate::error::{RefusalReason, ServiceError};
use crate::metrics::Metrics;
use crate::protocol::{
    HealthResponse, SignAuthEntryRequest, SignTransactionRequest, API_PREFIX, REQUEST_ID_HEADER,
};
use crate::sign::SignerService;

/// Shared state for the request handlers.
#[derive(Clone)]
pub struct AppState {
    /// The service itself.
    pub service: Arc<SignerService>,
}

/// Build the public router.
pub fn router(state: AppState, max_body_bytes: usize, timeout: Duration) -> Router {
    Router::new()
        .route(&format!("{API_PREFIX}/public-key"), get(public_key))
        .route(
            &format!("{API_PREFIX}/sign/transaction"),
            post(sign_transaction),
        )
        .route(
            &format!("{API_PREFIX}/sign/auth-entry"),
            post(sign_auth_entry),
        )
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            timeout,
        ))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(
            max_body_bytes,
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

/// Build the metrics router, for a separate internal listener.
pub fn metrics_router(metrics: Arc<Metrics>) -> Router {
    Router::new().route(
        "/metrics",
        get(move || {
            let metrics = Arc::clone(&metrics);
            async move {
                (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "text/plain; version=0.0.4",
                    )],
                    metrics.render(),
                )
            }
        }),
    )
}

/// Seconds since the epoch. Panic-free: a clock before 1970 reports 0 rather
/// than taking the service down.
fn now() -> UnixSeconds {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// The caller's correlation id, or a fresh one.
fn request_id(headers: &HeaderMap) -> String {
    headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        // A caller-supplied id lands in the audit log, so it is bounded and
        // stripped of anything that is not safely printable.
        .map(|value| {
            value
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                .take(64)
                .collect::<String>()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(crate::auth::generate_token)
}

/// Attach the request id to a response.
fn with_request_id(mut response: Response, request_id: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}

/// Authenticate, or record and refuse.
fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    operation: Operation,
    request_id: &str,
    at: UnixSeconds,
) -> Result<crate::sign::Caller, ServiceError> {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer_token)
        .ok_or_else(|| {
            ServiceError::new(RefusalReason::Unauthenticated, "a bearer token is required")
        });

    let result = match presented {
        Ok(token) => state.service.authenticate(token, at),
        Err(error) => Err(error),
    };

    result.inspect_err(|error| {
        state
            .service
            .record_unauthenticated(operation, request_id, error, at);
    })
}

async fn public_key(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let id = request_id(&headers);
    let at = now();

    let response = match authenticate(&state, &headers, Operation::PublicKey, &id, at) {
        Ok(caller) => match state.service.public_key(&caller, &id, at).await {
            Ok(body) => Json(body).into_response(),
            Err(error) => log_and_render(error),
        },
        Err(error) => error.into_response(),
    };

    with_request_id(response, &id)
}

async fn sign_transaction(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let id = request_id(&headers);
    let at = now();

    let response = match authenticate(&state, &headers, Operation::SignTransaction, &id, at) {
        Ok(caller) => match parse_body::<SignTransactionRequest>(&body) {
            Ok(request) => match state
                .service
                .sign_transaction(&caller, &request, &id, at)
                .await
            {
                Ok(body) => Json(body).into_response(),
                Err(error) => log_and_render(error),
            },
            Err(error) => {
                state
                    .service
                    .record_unauthenticated(Operation::SignTransaction, &id, &error, at);
                error.into_response()
            }
        },
        Err(error) => error.into_response(),
    };

    with_request_id(response, &id)
}

async fn sign_auth_entry(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let id = request_id(&headers);
    let at = now();

    let response = match authenticate(&state, &headers, Operation::SignAuthEntry, &id, at) {
        Ok(caller) => match parse_body::<SignAuthEntryRequest>(&body) {
            Ok(request) => match state
                .service
                .sign_auth_entry(&caller, &request, &id, at)
                .await
            {
                Ok(body) => Json(body).into_response(),
                Err(error) => log_and_render(error),
            },
            Err(error) => {
                state
                    .service
                    .record_unauthenticated(Operation::SignAuthEntry, &id, &error, at);
                error.into_response()
            }
        },
        Err(error) => error.into_response(),
    };

    with_request_id(response, &id)
}

/// Parse a request body.
///
/// Hand-rolled rather than `Json<T>` as an extractor so a malformed body
/// becomes a [`RefusalReason::MalformedRequest`] with our error shape, rather
/// than axum's default plain-text rejection — a client parsing `{ error }` per
/// the protocol should get that for every failure, not just the ones we chose.
fn parse_body<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ServiceError> {
    serde_json::from_slice(body).map_err(|error| {
        ServiceError::new(
            RefusalReason::MalformedRequest,
            format!("the request body does not match the protocol: {error}"),
        )
    })
}

/// Log the internal detail, then render the caller-facing refusal.
fn log_and_render(error: ServiceError) -> Response {
    if let Some(detail) = error.internal_detail() {
        tracing::warn!(reason = %error.reason(), %detail, "request refused");
    } else {
        tracing::info!(reason = %error.reason(), message = %error.message(), "request refused");
    }
    error.into_response()
}

/// Liveness. Says nothing beyond "this process is answering".
async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

/// Readiness: whether the signing backends can be reached.
async fn readyz(State(state): State<AppState>) -> Response {
    match state.service.backends.health().await {
        Ok(()) => (StatusCode::OK, Json(HealthResponse { status: "ok" })).into_response(),
        Err(error) => {
            tracing::warn!(%error, "readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(HealthResponse { status: "degraded" }),
            )
                .into_response()
        }
    }
}

/// A catch-all for anything not routed, so a probe gets our error shape.
pub async fn not_found(_: Request) -> Response {
    ServiceError::new(RefusalReason::MalformedRequest, "no such endpoint").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with(name: &str, value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
        headers
    }

    #[test]
    fn a_caller_supplied_request_id_is_used() {
        let headers = headers_with(REQUEST_ID_HEADER, "req-abc-123");
        assert_eq!(request_id(&headers), "req-abc-123");
    }

    #[test]
    fn a_request_id_is_generated_when_absent() {
        let id = request_id(&HeaderMap::new());
        assert_eq!(id.len(), 64, "a generated id should be a full random token");
    }

    #[test]
    fn a_caller_supplied_request_id_cannot_inject_into_the_audit_log() {
        // It lands in a JSON log line and in a response header; neither is a
        // place for caller-controlled control characters.
        let headers = headers_with(REQUEST_ID_HEADER, "abc\"def<script>");
        let id = request_id(&headers);
        assert_eq!(id, "abcdefscript");
    }

    #[test]
    fn a_request_id_is_bounded_in_length() {
        let headers = headers_with(REQUEST_ID_HEADER, &"a".repeat(10_000));
        assert_eq!(request_id(&headers).len(), 64);
    }

    #[test]
    fn an_all_punctuation_request_id_falls_back_to_a_generated_one() {
        let headers = headers_with(REQUEST_ID_HEADER, "!!!@@@");
        assert_eq!(request_id(&headers).len(), 64);
    }

    #[test]
    fn a_malformed_body_becomes_our_error_shape_not_axums() {
        // A client parsing `{ error }` per the protocol should get that for
        // every failure.
        let error = parse_body::<SignTransactionRequest>(b"not json").unwrap_err();
        assert_eq!(error.reason(), RefusalReason::MalformedRequest);
        assert_eq!(error.reason().status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_body_with_an_unknown_field_is_rejected() {
        let body = br#"{"xdr":"A","networkPassphrase":"n","extra":1}"#;
        assert!(parse_body::<SignTransactionRequest>(body).is_err());
    }
}
