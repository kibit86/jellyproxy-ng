//! Manage the current user’s backend server mappings and authentication methods.
use crate::auth::quick_connect::pending::PendingConnect;
use askama::Template;
use axum::{
    extract::{Path, State},
    response::{Html, IntoResponse},
    Form, Json,
};
use hyper::{header::HeaderValue, StatusCode};
use jellyfin_api::JellyfinClient;
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use uuid::Uuid;

use crate::{
    domain::ids::ServerId,
    domain::security::Password,
    http::state::ManagementState,
    servers::storage::Server,
    ui::{auth::AuthenticatedUser, user::authenticate_user_on_server},
};

#[derive(Template)]
#[template(path = "user/user_server_list.html")]
pub(crate) struct UserServerListTemplate {
    pub(crate) username: String,
    pub(crate) servers: Vec<Server>,
    pub(crate) unmapped_servers: Vec<Server>,
    /// IDs of the listed servers where the user is an administrator.
    pub(crate) administrator_ids: Vec<i64>,
    /// The proxy administrator may open a server that has no mapped account yet, to sign in
    /// on its own page.
    pub(crate) is_admin: bool,
    /// IDs of the listed (not yet connected) servers that were never set up: their
    /// first-run wizard opens through the proxy ("Set up" instead of "Open").
    pub(crate) setup_ids: Vec<i64>,
    pub(crate) ui_route: String,
}

#[derive(Deserialize)]
pub(crate) struct ConnectServerForm {
    pub(crate) username: String,
    pub(crate) password: Password,
}

#[derive(Serialize)]
pub(crate) struct QuickConnectProgress {
    status: &'static str,
    code: Option<String>,
    request_id: Option<String>,
}

pub(crate) async fn initiate_quick_connect(
    State(state): State<ManagementState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(server_id): Path<ServerId>,
) -> Result<Json<QuickConnectProgress>, StatusCode> {
    // Only existing proxy users (administrators included) can own mappings.
    if state
        .user_authorization
        .get_user_by_id(&user.id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .is_none()
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let server = state
        .server_storage
        .get_server_by_id(server_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let client = JellyfinClient::new(
        server.url.as_str(),
        jellyfin_api::ClientInfo {
            device_id: crate::auth::mapping_authentication::mapping_device_id(&server, &user.id),
            ..crate::config::CLIENT_INFO.clone()
        },
    )
    .map_err(|_| StatusCode::BAD_GATEWAY)?;
    if !client
        .quick_connect_enabled()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?
    {
        return Err(StatusCode::CONFLICT);
    }
    let result = client
        .initiate_quick_connect()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let request_id = Uuid::new_v4().to_string();
    state
        .quick_connect
        .pending
        .insert(
            request_id.clone(),
            PendingConnect {
                user_id: user.id,
                server_id,
                secret: result.secret,
            },
        )
        .await;
    Ok(Json(QuickConnectProgress {
        status: "pending",
        code: Some(result.code),
        request_id: Some(request_id),
    }))
}

pub(crate) async fn finish_quick_connect(
    State(state): State<ManagementState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path((server_id, request_id)): Path<(ServerId, String)>,
) -> Result<Json<QuickConnectProgress>, StatusCode> {
    let secret = state
        .quick_connect
        .pending
        .get(&request_id, &user.id, server_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let server = state
        .server_storage
        .get_server_by_id(server_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let client = JellyfinClient::new(
        server.url.as_str(),
        jellyfin_api::ClientInfo {
            device_id: crate::auth::mapping_authentication::mapping_device_id(&server, &user.id),
            ..crate::config::CLIENT_INFO.clone()
        },
    )
    .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let progress = client
        .quick_connect_state(&secret)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    if !progress.authenticated {
        return Ok(Json(QuickConnectProgress {
            status: "pending",
            code: None,
            request_id: None,
        }));
    }
    // The request ID is one-use, including when redemption fails. Retrying requires
    // a new approval rather than racing two token writes for the same mapping.
    if !state.quick_connect.pending.claim(&request_id).await {
        return Err(StatusCode::NOT_FOUND);
    }
    let auth: jellyfin_api::models::AuthResponse = client
        .authenticate_with_quick_connect(&secret)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    if auth.access_token.is_empty() || auth.user.id.is_empty() {
        return Err(StatusCode::BAD_GATEWAY);
    }
    let encryption_key: crate::domain::security::HashedPassword =
        (&state.credentials.password()).into();
    state
        .user_authorization
        .add_quick_connect_mapping(
            &user.id,
            &server,
            &auth.user.name,
            &auth.user.id,
            &auth.access_token,
            &encryption_key,
        )
        .await
        .map_err(|e| {
            error!("Failed to save Quick Connect mapping: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    Ok(Json(QuickConnectProgress {
        status: "connected",
        code: None,
        request_id: None,
    }))
}

#[derive(Template)]
#[template(path = "user/user_server_status.html")]
pub(crate) struct UserServerStatusTemplate {
    pub(crate) username: Option<String>,
    pub(crate) error_message: Option<String>,
    pub(crate) server_version: String,
}

pub(crate) async fn get_user_servers(
    State(state): State<ManagementState>,
    AuthenticatedUser(user): AuthenticatedUser,
) -> impl IntoResponse {
    let mapped_servers = match state.user_authorization.get_mapped_servers(&user.id).await {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to list mapped servers: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Database error").into_response();
        }
    };

    let all_servers = match state.server_storage.list_servers().await {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to list all servers: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "Database error").into_response();
        }
    };

    let unmapped_servers: Vec<Server> = all_servers
        .into_iter()
        .filter(|s| !mapped_servers.iter().any(|ms| ms.id == s.id))
        .collect();

    // The Administer button is for the servers where the account the user mapped is
    // an administrator. Signing in to each backend is cached, and one that is down
    // simply gets no button.
    let administrator_ids: Vec<i64> =
        futures_util::future::join_all(mapped_servers.iter().map(|server| async {
            state
                .panel_identities
                .get(&state, &user.id, server)
                .await
                .ok()
                .filter(|identity| identity.is_administrator)
                .map(|_| server.id.as_i64())
        }))
        .await
        .into_iter()
        .flatten()
        .collect();

    let setup_ids: Vec<i64> = if user.role == crate::ui::auth::UserRole::Admin {
        futures_util::future::join_all(unmapped_servers.iter().map(|server| async {
            state
                .panel_identities
                .setup_pending(&state, server)
                .await
                .then_some(server.id.as_i64())
        }))
        .await
        .into_iter()
        .flatten()
        .collect()
    } else {
        Vec::new()
    };

    let template = UserServerListTemplate {
        username: user.username,
        servers: mapped_servers,
        unmapped_servers,
        administrator_ids,
        is_admin: user.role == crate::ui::auth::UserRole::Admin,
        setup_ids,
        ui_route: state.paths.ui_route(),
    };

    match template.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            error!("Failed to render user server list template: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "Template error").into_response()
        }
    }
}

pub(crate) async fn connect_server(
    State(state): State<ManagementState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(server_id): Path<ServerId>,
    Form(form): Form<ConnectServerForm>,
) -> impl IntoResponse {
    // Get server details
    let server = match state.server_storage.get_server_by_id(server_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Html("<span style=\"color: #dc3545;\">Server not found</span>"),
            )
                .into_response()
        }
        Err(e) => {
            error!("Failed to get server: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("<span style=\"color: #dc3545;\">Database error</span>"),
            )
                .into_response();
        }
    };

    // Verify credentials with upstream Jellyfin
    let server_url = server.url.clone();

    let client_info = crate::config::CLIENT_INFO.clone();

    let client = match JellyfinClient::new(server_url.as_str(), client_info) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to create jellyfin client: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("<span style=\"color: #dc3545;\">Client error</span>"),
            )
                .into_response();
        }
    };

    match client.authenticate_by_name(&form.username, form.password.as_str()).await {
        Ok(_) => {
            // Credentials valid. The check was only a check: close its session.
            if let Err(error) = client.logout().await {
                tracing::debug!(%error, "Could not close the verification session");
            }
            // Create the mapping
            let mapping_key = user.local_credential.mapping_key();
            match state
                .user_authorization
                .add_server_mapping(
                    &user.id,
                    &server,
                    &form.username,
                    &form.password,
                    Some(&mapping_key),
                )
                .await
            {
                Ok(_) => {
                    state.panel_identities.forget(&user.id, &server).await;
                    info!(
                        "Created mapping for user {} to server {}",
                        user.username, server.name
                    );

                    // Return HX-Redirect header for HTMX
                    let mut response = StatusCode::OK.into_response();
                    response.headers_mut().insert(
                        "HX-Redirect",
                        HeaderValue::from_str(&format!("/{}", state.paths.ui_route())).unwrap(),
                    );
                    response
                }
                Err(e) => {
                    error!("Failed to create mapping: {}", e);
                    (
                        StatusCode::OK,
                        Html("<div style=\"background-color: #e74c3c; color: white; padding: 0.75rem; border-radius: 0.25rem; margin-bottom: 1rem;\">Database error</div>"),
                    )
                        .into_response()
                }
            }
        }
        Err(jellyfin_api::error::Error::AuthenticationFailed(_)) => {
            (
                StatusCode::OK,
                Html("<div style=\"background-color: #e74c3c; color: white; padding: 0.75rem; border-radius: 0.25rem; margin-bottom: 1rem;\">Invalid credentials</div>"),
            )
                .into_response()
        }
        Err(e) => {
            error!("Failed to authenticate with upstream: {}", e);
            (
                StatusCode::OK,
                Html(format!("<div style=\"background-color: #e74c3c; color: white; padding: 0.75rem; border-radius: 0.25rem; margin-bottom: 1rem;\">Connection error: {}</div>", e)),
            )
                .into_response()
        }
    }
}

pub(crate) async fn delete_server_mapping(
    State(state): State<ManagementState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(server_id): Path<ServerId>,
) -> impl IntoResponse {
    // Get server details to find the URL
    let server = match state.server_storage.get_server_by_id(server_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            error!("Failed to get server: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Find the mapping
    let mappings = match state
        .user_authorization
        .list_server_mappings(&user.id)
        .await
    {
        Ok(m) => m,
        Err(e) => {
            error!("Failed to list mappings: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // Normalize URLs for comparison (remove trailing slashes)
    if let Some(mapping) = mappings.iter().find(|m| m.server_id == server.id) {
        match state
            .user_authorization
            .delete_server_mapping(mapping.id)
            .await
        {
            Ok(_) => {
                state.panel_identities.forget(&user.id, &server).await;
                info!(
                    "Deleted mapping for user {} to server {}",
                    user.username, server.name
                );
                // Return HX-Redirect header for HTMX
                let mut response = StatusCode::OK.into_response();
                response.headers_mut().insert(
                    "HX-Redirect",
                    HeaderValue::from_str(&format!("/{}", state.paths.ui_route())).unwrap(),
                );
                return response;
            }
            Err(e) => {
                error!("Failed to delete mapping: {}", e);
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    }

    StatusCode::NOT_FOUND.into_response()
}

pub(crate) async fn check_user_server_status(
    State(state): State<ManagementState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(server_id): Path<ServerId>,
) -> impl IntoResponse {
    // Get server details
    let server = match state.server_storage.get_server_by_id(server_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Html("<span style=\"color: #dc3545;\">Server not found</span>"),
            )
                .into_response()
        }
        Err(e) => {
            error!("Failed to get server: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("<span style=\"color: #dc3545;\">Database error</span>"),
            )
                .into_response();
        }
    };

    match authenticate_user_on_server(&state, &user, &server).await {
        Ok(authenticated) => {
            let template = UserServerStatusTemplate {
                username: Some(authenticated.user.name.clone()),
                error_message: None,
                server_version: authenticated
                    .system_info
                    .version
                    .clone()
                    .unwrap_or("unknown".to_string()),
            };
            authenticated.close().await;
            match template.render() {
                Ok(html) => Html(html).into_response(),
                Err(e) => {
                    error!("Failed to render user server status template: {}", e);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Html("<span style=\"color: #dc3545;\">Template error</span>"),
                    )
                        .into_response()
                }
            }
        }
        Err(e) => (
            StatusCode::OK,
            Html(format!("<span style=\"color: #dc3545;\">{}</span>", e)),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{domain::ids::ServerId, servers::url::ServerUrl};
    use askama::Template;

    fn server(id: i64, name: &str) -> Server {
        Server {
            id: ServerId::new(id),
            name: name.into(),
            url: ServerUrl::parse("http://backend.invalid").unwrap(),
            priority: 100,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    /// The "My servers" page: `connected` are the servers with a mapped account,
    /// `available` the ones without; `administrator_ids` where the mapped account
    /// administers, `setup_ids` the available servers never set up.
    fn page(
        is_admin: bool,
        connected: Vec<Server>,
        available: Vec<Server>,
        administrator_ids: Vec<i64>,
        setup_ids: Vec<i64>,
    ) -> String {
        UserServerListTemplate {
            username: "alice".into(),
            servers: connected,
            unmapped_servers: available,
            administrator_ids,
            is_admin,
            setup_ids,
            ui_route: "ui".into(),
        }
        .render()
        .unwrap()
    }

    fn count(html: &str, needle: &str) -> usize {
        html.matches(needle).count()
    }

    #[test]
    fn administer_is_offered_only_where_the_mapped_account_administers() {
        let html = page(
            false,
            vec![server(1, "Alpha"), server(2, "Beta")],
            vec![],
            vec![2],
            vec![],
        );
        assert_eq!(count(&html, "Administer"), 1, "{html}");
        assert!(html.contains(r#"href="/ui/backend/2/web/#/dashboard""#));
        assert!(!html.contains("/ui/backend/1/web/"));
        // Opens in a new tab, like every panel button.
        assert!(html.contains(r#"target="_blank""#));
    }

    #[test]
    fn a_simple_user_never_gets_open_or_set_up() {
        let html = page(false, vec![], vec![server(3, "Gamma")], vec![], vec![3]);
        assert!(!html.contains("Set up"), "{html}");
        assert!(
            !html.contains(">Open<") && !html.contains("Open\n"),
            "{html}"
        );
        assert!(!html.contains("/ui/backend/3/web/"), "{html}");
        // But they can still connect their own account.
        assert!(html.contains("connect-button"));
    }

    #[test]
    fn the_administrator_opens_unmapped_servers_and_sets_up_new_ones() {
        let html = page(
            true,
            vec![],
            vec![server(4, "Ready"), server(5, "Brand new")],
            vec![],
            vec![5],
        );
        // A set-up server: "Set up" for the new one, "Open" for the other.
        assert_eq!(count(&html, "Set up"), 1, "{html}");
        assert_eq!(count(&html, "/ui/backend/5/web/"), 1);
        assert_eq!(count(&html, "/ui/backend/4/web/"), 1);
        assert!(html.contains("Open"), "{html}");
        // Both are a new tab, neither is the dashboard.
        assert!(!html.contains("#/dashboard"));
        assert!(html.matches(r#"target="_blank" rel="noopener""#).count() >= 2);
    }

    #[test]
    fn the_administrator_gets_both_administer_and_open_on_their_page() {
        let html = page(
            true,
            vec![server(1, "Alpha")],
            vec![server(2, "Beta")],
            vec![1],
            vec![],
        );
        assert!(
            html.contains(r#"href="/ui/backend/1/web/#/dashboard""#),
            "{html}"
        );
        assert!(html.contains(r#"href="/ui/backend/2/web/""#), "{html}");
    }

    #[test]
    fn an_empty_list_says_so() {
        let html = page(true, vec![], vec![], vec![], vec![]);
        assert!(html.contains("No Servers Connected"), "{html}");
    }
}
