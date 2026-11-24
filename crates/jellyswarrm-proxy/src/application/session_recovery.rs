//! Re-login when a backend refuses the token of a stored session.
//!
//! Jellyfin keeps one token per (user, device id) and revokes it on logout or when that device
//! logs in again, and an administrator can revoke it too. A client of Jellyfin simply logs in
//! again when it gets a 401; the proxy does the same on behalf of its devices, so a revoked
//! backend token never leaves a server missing until the app is reinstalled.
use std::{collections::HashMap, sync::Arc};

use tracing::{info, warn};

use crate::{
    auth::{
        mapping_authentication::{backend_device_id, MappingAuthenticator},
        service::{AuthorizationSession, MappingAuth, UserAuthorizationService},
    },
    servers::storage::Server,
};

/// One re-login at a time per stored session: parallel requests that all got the same 401 wait
/// for the first one and reuse its token instead of each revoking the other's.
pub(crate) struct SessionRecovery {
    authenticator: MappingAuthenticator,
    locks: tokio::sync::Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
}

impl SessionRecovery {
    pub(crate) fn new(authenticator: MappingAuthenticator) -> Self {
        Self {
            authenticator,
            locks: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// `server` refused the token a request carried: find the stored session that holds it and
    /// repair that. `None` when no stored session has this token (a passthrough or foreign token).
    pub(crate) async fn recover_by_token(
        &self,
        users: &UserAuthorizationService,
        server: &Server,
        stale_token: &str,
    ) -> Option<String> {
        let session = users
            .find_session_by_backend_token(server.id, stale_token)
            .await
            .ok()??;
        self.recover(users, &session, server, stale_token).await
    }

    /// `server` refused `stale_token`, the token of `session`. Returns the token to retry with,
    /// or `None` when the session cannot be repaired here (a Quick Connect mapping has no
    /// password to log in with; the device was logged out meanwhile; the backend refuses).
    pub(crate) async fn recover(
        &self,
        users: &UserAuthorizationService,
        session: &AuthorizationSession,
        server: &Server,
        stale_token: &str,
    ) -> Option<String> {
        let lock = {
            let mut locks = self.locks.lock().await;
            locks.entry(session.id).or_default().clone()
        };
        let _one_at_a_time = lock.lock().await;

        // Someone may have repaired it while we waited.
        let current = users
            .get_user_sessions(&session.user_id, None)
            .await
            .ok()?
            .into_iter()
            .find(|(candidate, _)| candidate.id == session.id)?
            .0;
        if current.jellyfin_token != stale_token {
            return Some(current.jellyfin_token);
        }

        let user = users.get_user_by_id(&session.user_id).await.ok()??;
        let mapping = users.get_server_mapping(&user.id, server).await.ok()??;
        if matches!(mapping.auth, MappingAuth::QuickConnect { .. }) {
            warn!(
                "{} refused the saved Quick Connect token of '{}': reconnect the account in My servers",
                server.name,
                mapping.auth.username()
            );
            return None;
        }
        let client_info = jellyfin_api::ClientInfo {
            client: session.device.client.clone(),
            device: session.device.device.clone(),
            device_id: backend_device_id(
                server,
                &user.original_username,
                &session.device.device_id,
            ),
            version: session.device.version.clone(),
        };
        let login = match self
            .authenticator
            .authenticate_login(&user, server, &mapping, None, client_info)
            .await
        {
            Ok(login) => login,
            Err(error) => {
                warn!("Could not log in again on {}: {error}", server.name);
                return None;
            }
        };
        let new_token = login.response.access_token.clone();
        let mut authorization = session.to_authorization();
        authorization.token = Some(new_token.clone());
        if let Err(error) = users
            .store_authorization_session(
                &user.id,
                server,
                &authorization,
                new_token.clone(),
                login.response.user.id.clone(),
                None,
            )
            .await
        {
            warn!(
                "Could not store the new session on {}: {error}",
                server.name
            );
            return None;
        }
        info!(
            "re-authenticated on {} for {} after 401",
            server.name,
            crate::application::request::collect::device_label(Some(&session.device))
        );
        Some(new_token)
    }
}

/// The session token a request carries: the `Token` of an `Authorization` / `X-Emby-Authorization`
/// header, an `X-MediaBrowser-Token` / `X-Emby-Token` header, or the `ApiKey` / `api_key` parameter.
pub(crate) fn request_token(request: &reqwest::Request) -> Option<String> {
    for name in ["authorization", "x-emby-authorization"] {
        if let Some(value) = request.headers().get(name).and_then(|v| v.to_str().ok()) {
            if let Some(start) = value.find("Token=\"") {
                let rest = &value[start + 7..];
                if let Some(end) = rest.find('"') {
                    return Some(rest[..end].to_owned());
                }
            }
        }
    }
    for name in ["x-mediabrowser-token", "x-emby-token"] {
        if let Some(value) = request.headers().get(name).and_then(|v| v.to_str().ok()) {
            return Some(value.to_owned());
        }
    }
    request
        .url()
        .query_pairs()
        .find(|(key, _)| key.eq_ignore_ascii_case("apikey") || key.eq_ignore_ascii_case("api_key"))
        .map(|(_, value)| value.into_owned())
}

/// Replace `old` by `new` wherever a request carries the session token: the Jellyfin
/// authorization headers and the `ApiKey` / `api_key` query parameters.
pub(crate) fn swap_token(request: &mut reqwest::Request, old: &str, new: &str) {
    for name in [
        "authorization",
        "x-emby-authorization",
        "x-mediabrowser-token",
        "x-emby-token",
    ] {
        let replaced = request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .filter(|value| value.contains(old))
            .and_then(|value| {
                reqwest::header::HeaderValue::from_str(&value.replace(old, new)).ok()
            });
        if let Some(value) = replaced {
            request.headers_mut().insert(name, value);
        }
    }
    let pairs: Vec<(String, String)> = request
        .url()
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    if pairs.iter().any(|(_, value)| value == old) {
        request
            .url_mut()
            .query_pairs_mut()
            .clear()
            .extend_pairs(pairs.iter().map(|(key, value)| {
                (
                    key.as_str(),
                    if value == old { new } else { value.as_str() },
                )
            }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(url: &str, authorization: &str) -> reqwest::Request {
        reqwest::Client::new()
            .get(url)
            .header("Authorization", authorization)
            .build()
            .unwrap()
    }

    #[test]
    fn the_token_is_replaced_in_headers_and_query_but_nothing_else() {
        let mut r = request(
            "http://backend/Items?ApiKey=old-token&Limit=5&x=old-token-not-equal",
            "MediaBrowser Client=\"c\", Device=\"d\", DeviceId=\"old-token-id\", Token=\"old-token\"",
        );
        swap_token(&mut r, "old-token", "new-token");
        let header = r.headers()["authorization"].to_str().unwrap();
        assert!(header.contains("Token=\"new-token\""), "{header}");
        assert!(
            header.contains("DeviceId=\"new-token-id\""),
            "a plain substring replace: {header}"
        );
        let query: Vec<_> = r
            .url()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert!(query.contains(&("ApiKey".into(), "new-token".into())));
        assert!(query.contains(&("Limit".into(), "5".into())));
        assert!(
            query.contains(&("x".into(), "old-token-not-equal".into())),
            "only whole values"
        );
    }

    #[test]
    fn the_token_is_read_from_the_header_or_the_query() {
        let r = request(
            "http://backend/Items",
            "MediaBrowser Client=\"c\", Token=\"abc\"",
        );
        assert_eq!(request_token(&r).as_deref(), Some("abc"));
        let r = reqwest::Client::new()
            .get("http://backend/x?ApiKey=key1")
            .build()
            .unwrap();
        assert_eq!(request_token(&r).as_deref(), Some("key1"));
        let r = reqwest::Client::new()
            .get("http://backend/x")
            .build()
            .unwrap();
        assert_eq!(request_token(&r), None);
    }

    #[test]
    fn a_request_without_the_token_is_left_alone() {
        let mut r = request(
            "http://backend/Items?Limit=5",
            "MediaBrowser Token=\"other\"",
        );
        swap_token(&mut r, "old-token", "new-token");
        assert_eq!(r.url().as_str(), "http://backend/Items?Limit=5");
        assert_eq!(r.headers()["authorization"], "MediaBrowser Token=\"other\"");
    }
}
