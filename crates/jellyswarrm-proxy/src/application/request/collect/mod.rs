//! Collect the caller context for one client request: resolve the proxy user,
//! analyze the JSON body and load the authorized, healthy backend sessions.
//! Backend *selection* happens later, in `routing`.
use anyhow::{anyhow, Result};
use tracing::{debug, error, warn};

use crate::application::accounts::caller_for_credential;
use crate::application::{
    media_services::MediaServices,
    request::{context::RequestContext, input::OperationRequest},
};
use crate::auth::service::{AuthorizationSession, LocalUser};
use crate::libraries::service::VirtualLibraryAccessScope;
use crate::protocol::caller::{request_device, JellyfinAuthorization};
use crate::protocol::device::Device;
use crate::protocol::routes::PlaybackSessionAction;
use crate::servers::storage::{Server, ServerHealthStatus};
use crate::translation::analyze_json;
use crate::translation::request_analyzer::{RequestAnalysisContext, RequestBodyAnalysisResult};

#[cfg(test)]
mod tests;

/// `request` is the buffered client request (see `http::incoming`), still
/// addressed to the proxy with virtual IDs and proxy credentials.
pub(crate) async fn operation(
    request: reqwest::Request,
    state: &MediaServices,
    playback_session_action: Option<PlaybackSessionAction>,
) -> Result<OperationRequest> {
    // 1. Who is calling?
    let auth = JellyfinAuthorization::from_headers_url(request.headers(), request.url());
    debug!(authorization = ?auth, "Extracted authorization");
    let device = request_device(request.headers(), request.url(), auth.as_ref());
    let (mut user, client_device) =
        match caller_for_credential(auth.as_ref(), &state.user_authorization).await? {
            Some((user, device)) => (Some(user), device),
            None => (None, None),
        };
    if let Some(client_device) = &client_device {
        // Like Jellyfin: the header may refresh the device's name and version, never its identity.
        let seen = match &auth {
            Some(
                JellyfinAuthorization::Authorization(a)
                | JellyfinAuthorization::XEmbyAuthorization(a),
            ) => Some(a),
            _ => None,
        };
        if let Err(error) = state
            .user_authorization
            .touch_device(client_device, seen)
            .await
        {
            debug!("Could not refresh device activity: {error}");
        }
    }
    if let (Some(user), Some(token)) = (user.as_ref(), auth.as_ref().and_then(|a| a.token_ref())) {
        state
            .client_sessions
            .ensure(user, token, device.as_ref())
            .await;
    }

    // 2. Which IDs, users and play sessions does the JSON body reference?
    let body_analysis = analyze_body(
        &request,
        state,
        user.as_ref().map(|user| user.id.clone()),
        playback_session_action,
    )
    .await?;
    if let Some(found_user) = body_analysis.as_ref().and_then(|a| a.get_user()) {
        debug!(user_id = %found_user.id, "Found user in request body");
        // Legacy metadata only: an unauthenticated body never authenticates.
        if user.is_none() && auth.is_none() {
            user = Some(found_user);
        }
    }

    // 3. Which backends may this caller use, and which are usable right now?
    let (sessions, access_scope) = match (&auth, &user) {
        (Some(auth), Some(user)) => {
            let (sessions, scope) =
                backend_sessions(state, &request, auth, user, client_device.as_ref()).await?;
            (
                Some(sessions).filter(|sessions| !sessions.is_empty()),
                Some(scope),
            )
        }
        (_, user) => (
            None,
            user.as_ref()
                .map(|user| VirtualLibraryAccessScope::new(&user.id, [])),
        ),
    };

    let context = RequestContext::new(&request, auth, user, sessions, access_scope);
    Ok(OperationRequest {
        original_request: request,
        context,
        body_analysis,
    })
}

async fn analyze_body(
    request: &reqwest::Request,
    state: &MediaServices,
    authenticated_user_id: Option<crate::domain::user_id::UserId>,
    playback_session_action: Option<PlaybackSessionAction>,
) -> Result<Option<RequestBodyAnalysisResult>> {
    let Some(json) = body_to_json(request) else {
        if playback_session_action.is_some() {
            return Err(anyhow!("playback session report requires a JSON body"));
        }
        return Ok(None);
    };
    let is_authenticated = authenticated_user_id.is_some();
    let context = RequestAnalysisContext {
        authenticated_user_id,
        playback_session_action,
    };
    let analysis = analyze_json(
        &json,
        &state.translation.request_analyzer,
        &context,
        RequestBodyAnalysisResult::default(),
    )
    .await?;
    if playback_session_action.is_some()
        && (analysis.requested_play_item_id.is_none() || !is_authenticated)
    {
        return Err(anyhow!(
            "playback report requires an authenticated user and item"
        ));
    }
    if let Some(found_server) = analysis.get_server() {
        debug!("Found server in request body: {}", &found_server.name);
    }
    Ok(Some(analysis))
}

/// `client / device / ...tail` of a device, for logs: the last 8 characters of the device id
/// identify it without printing the whole identifier.
pub(crate) fn device_label(device: Option<&Device>) -> String {
    let Some(device) = device else {
        return "no device in the request".to_owned();
    };
    let tail: String = {
        let mut chars: Vec<char> = device.device_id.chars().rev().take(8).collect();
        chars.reverse();
        chars.into_iter().collect()
    };
    format!(
        "client='{}' device='{}' device_id=...{}",
        device.client, device.device, tail
    )
}

/// Why a caller with mapped servers has no usable backend session (see `backend_sessions`).
/// Never includes tokens: only names, counts and the health-check failure text.
pub(crate) fn no_usable_session_note(
    device: Option<&Device>,
    user_sessions: usize,
    matched_for_device: usize,
    mapped_servers: usize,
    unhealthy: &[String],
) -> String {
    let unhealthy = if unhealthy.is_empty() {
        "none".to_owned()
    } else {
        unhealthy
            .iter()
            .map(|entry| entry.chars().take(120).collect::<String>())
            .collect::<Vec<_>>()
            .join("; ")
    };
    format!(
        "no usable backend session: {}; the user has {user_sessions} session(s), {matched_for_device} match this device, \
         {mapped_servers} mapped server(s); servers marked unhealthy: {unhealthy}",
        device_label(device)
    )
}

/// Return the caller's healthy backend sessions for this device and the full
/// mapped access scope. Authorization and availability are distinct: mapped
/// servers stay in scope even when offline or missing a session for this device.
async fn backend_sessions(
    state: &MediaServices,
    request: &reqwest::Request,
    auth: &JellyfinAuthorization,
    user: &LocalUser,
    client_device: Option<&crate::auth::service::ClientDevice>,
) -> Result<(
    Vec<(AuthorizationSession, Server)>,
    VirtualLibraryAccessScope,
)> {
    // The device is the one the token belongs to, exactly as in Jellyfin: no header string is
    // compared with a stored one. A read-only API key has no device and may use any of the
    // user's sessions (token-only media delivery).
    let sessions = match client_device {
        Some(device) => {
            state
                .user_authorization
                .get_device_sessions(device.id)
                .await?
        }
        None => {
            state
                .user_authorization
                .get_user_sessions(&user.id, None)
                .await?
        }
    };
    // Only for the log when the list ends up empty.
    let upstream_device = auth.get_device(request.headers());

    let mapped_servers = state
        .user_authorization
        .get_mapped_servers(&user.id)
        .await?;
    let access_scope =
        VirtualLibraryAccessScope::new(&user.id, mapped_servers.iter().map(|server| server.id));

    let matched = sessions.len();
    let mut healthy = Vec::with_capacity(sessions.len());
    let mut unhealthy = Vec::new();
    for (session, server) in sessions {
        match state.server_storage.server_status(server.id).await {
            ServerHealthStatus::Healthy(_) => healthy.push((session, server)),
            ServerHealthStatus::Unhealthy(reason) => {
                unhealthy.push(format!("{}: {}", server.name, reason));
            }
        }
    }
    // A caller with mapped servers and no usable session is answered 503 ("selected owner
    // unavailable") without any upstream call. Say why, once per request: no session for this
    // device, or the matching ones sit on servers marked unhealthy.
    if healthy.is_empty() && !mapped_servers.is_empty() {
        let user_sessions = state
            .user_authorization
            .get_user_sessions(&user.id, None)
            .await
            .map_or(0, |all| all.len());
        warn!(
            "{}",
            no_usable_session_note(
                upstream_device.as_ref(),
                user_sessions,
                matched,
                mapped_servers.len(),
                &unhealthy,
            )
        );
    }
    Ok((healthy, access_scope))
}

/// Parse a non-empty `application/json` body; anything else is opaque.
fn body_to_json(request: &reqwest::Request) -> Option<serde_json::Value> {
    let content_type = request
        .headers()
        .get(reqwest::header::CONTENT_TYPE)?
        .to_str()
        .ok()?;
    if !content_type.contains("application/json") {
        return None;
    }
    let bytes = request.body()?.as_bytes().unwrap_or_default();
    if bytes.is_empty() {
        return None;
    }
    serde_json::from_slice(bytes)
        .inspect_err(|error| error!("Failed to parse JSON body: {error}"))
        .ok()
}

#[cfg(test)]
mod no_usable_session_note_tests {
    use super::*;

    fn device(id: &str) -> Device {
        Device {
            client: "Jellyfin Web".to_owned(),
            device: "LG Smart TV".to_owned(),
            device_id: id.to_owned(),
            version: "12.4.0".to_owned(),
        }
    }

    #[test]
    fn says_whether_the_device_has_no_session_or_the_servers_are_unhealthy() {
        let d = device("TW96aWxs|1794747421727");
        let no_session = no_usable_session_note(Some(&d), 6, 0, 2, &[]);
        assert!(
            no_session.contains("6 session(s), 0 match this device"),
            "{no_session}"
        );
        assert!(
            no_session.contains("client='Jellyfin Web' device='LG Smart TV' device_id=...47421727"),
            "{no_session}"
        );
        assert!(
            no_session.ends_with("servers marked unhealthy: none"),
            "{no_session}"
        );

        let down = no_usable_session_note(
            Some(&d),
            6,
            2,
            2,
            &[
                "Morlupo: Network error".to_owned(),
                "Texd86: timeout".to_owned(),
            ],
        );
        assert!(down.contains("2 match this device"), "{down}");
        assert!(
            down.contains("Morlupo: Network error; Texd86: timeout"),
            "{down}"
        );
    }

    #[test]
    fn never_prints_the_whole_device_id_and_copes_with_a_missing_device() {
        let d = device("SECRET-LONG-DEVICE-IDENTIFIER-1234567890");
        let note = no_usable_session_note(Some(&d), 1, 0, 1, &[]);
        assert!(note.contains("device_id=...34567890"), "{note}");
        assert!(!note.contains("SECRET-LONG"), "{note}");
        assert!(no_usable_session_note(None, 0, 0, 1, &[]).contains("no device in the request"));
    }

    #[test]
    fn a_very_long_health_failure_is_truncated() {
        let long = "x".repeat(500);
        let note = no_usable_session_note(None, 0, 0, 1, &[long]);
        assert!(note.len() < 400, "{}", note.len());
    }
}
