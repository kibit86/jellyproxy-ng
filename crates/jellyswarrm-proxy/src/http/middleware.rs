//! Enforce read-only API-key permissions before incoming requests reach handlers.
use crate::{auth::UserAuthorizationService, protocol::caller::JellyfinAuthorization};
use axum::{
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use tracing::{error, warn};

/// Log every 5xx with the method, the path (never the query: it can hold tokens), the status
/// and, when a handler returned an `ApplicationError`, why. The access log alone only says
/// "503, 2 ms", which cannot tell an empty session list from an incomplete catalog.
/// Liveness/readiness probes are skipped: their 503 is the signal, not an event.
pub(crate) async fn log_server_errors(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let response = next.run(request).await;
    let status = response.status();
    if status.is_server_error() && !matches!(path.as_str(), "/health" | "/ready") {
        let reason = response
            .extensions()
            .get::<crate::http::error::FailureReason>()
            .map_or("not reported by the handler", |reason| reason.0.as_str());
        warn!(%method, %path, status = status.as_u16(), reason, "request failed");
    }
    response
}
pub(crate) async fn enforce_read_only_api_keys(
    State(state): State<Arc<UserAuthorizationService>>,
    request: Request,
    next: Next,
) -> Response {
    if matches!(*request.method(), Method::GET | Method::HEAD) {
        return next.run(request).await;
    }

    // Only the credential matters here; resolving the caller's user would cost a lookup.
    let url = match crate::http::incoming::request_url(request.uri()) {
        Ok(url) => url,
        Err(error) => {
            error!("Failed to enforce API key access: {error}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let auth = JellyfinAuthorization::from_headers_url(request.headers(), &url);
    let Some(token) = auth.as_ref().and_then(JellyfinAuthorization::token_ref) else {
        return next.run(request).await;
    };

    match state.is_read_only_api_key(token).await {
        Ok(true) => StatusCode::FORBIDDEN.into_response(),
        Ok(false) => next.run(request).await,
        Err(error) => {
            error!("Failed to check API key access: {error}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod log_server_errors_tests {
    use super::*;
    use crate::{application::error::ApplicationError, http::error::FailureReason};
    use axum::{body::Body, middleware, routing::get, Router};
    use std::sync::Mutex;
    use tower::ServiceExt;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Captured {
        type Writer = Captured;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    fn app() -> Router {
        async fn owner() -> Result<&'static str, ApplicationError> {
            Err(ApplicationError::UnavailableOwner)
        }
        async fn denied() -> Result<&'static str, ApplicationError> {
            Err(ApplicationError::AccessDenied)
        }
        async fn transport() -> Result<&'static str, ApplicationError> {
            let error = reqwest::Client::new()
                .get("http://127.0.0.1:1/Items?api_key=SECRET-TOKEN")
                .send()
                .await
                .unwrap_err();
            Err(ApplicationError::UpstreamTransport(error))
        }
        async fn plain() -> axum::http::StatusCode {
            axum::http::StatusCode::BAD_GATEWAY
        }
        async fn ready() -> axum::http::StatusCode {
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        }
        Router::new()
            .route("/Items", get(owner))
            .route("/Denied", get(denied))
            .route("/Transport", get(transport))
            .route("/Plain", get(plain))
            .route("/ready", get(ready))
            .layer(middleware::from_fn(log_server_errors))
    }

    async fn call(uri: &str) -> (axum::http::StatusCode, bool, String) {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let response = app()
            .oneshot(
                axum::http::Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let has_reason = response.extensions().get::<FailureReason>().is_some();
        (response.status(), has_reason, captured.text())
    }

    #[tokio::test]
    async fn a_503_is_logged_with_method_path_and_reason_but_not_the_query() {
        let (status, _, log) = call("/Items?api_key=SECRET-TOKEN&UserId=abc").await;
        assert_eq!(status, 503, "the client-visible status must not change");
        assert!(log.contains("request failed"), "{log}");
        assert!(log.contains("GET"), "{log}");
        assert!(log.contains("/Items"), "{log}");
        assert!(log.contains("503"), "{log}");
        assert!(log.contains("selected owner unavailable"), "{log}");
        assert!(!log.contains("SECRET-TOKEN"), "query leaked: {log}");
        assert!(!log.contains("UserId"), "query leaked: {log}");
    }

    #[tokio::test]
    async fn a_transport_failure_is_named_but_its_url_is_never_logged() {
        let (status, _, log) = call("/Transport").await;
        assert_eq!(status, 502);
        assert!(log.contains("upstream transport failure"), "{log}");
        assert!(!log.contains("SECRET-TOKEN"), "upstream URL leaked: {log}");
        assert!(!log.contains("api_key"), "upstream URL leaked: {log}");
    }

    #[tokio::test]
    async fn a_5xx_without_a_reason_is_still_logged() {
        let (status, has_reason, log) = call("/Plain").await;
        assert_eq!(status, 502);
        assert!(!has_reason);
        assert!(log.contains("not reported by the handler"), "{log}");
    }

    #[tokio::test]
    async fn client_errors_and_probes_are_not_logged() {
        let (status, has_reason, log) = call("/Denied").await;
        assert_eq!(status, 403);
        assert!(!has_reason, "only 5xx carry a reason");
        assert!(!log.contains("request failed"), "{log}");

        let (status, _, log) = call("/ready").await;
        assert_eq!(status, 503);
        assert!(!log.contains("request failed"), "{log}");
    }
}
