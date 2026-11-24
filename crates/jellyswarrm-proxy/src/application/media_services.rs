//! Media capabilities shared by catalog, playback, routing, and generic forwarding.
//! Every handle is reference counted: workflows borrow `&MediaServices`, and
//! bounded source tasks clone it. Accounts, administration, SyncPlay, and Quick
//! Connect keep their own state and are not reachable from here.
use crate::{
    application::{
        catalog::snapshots::CatalogSnapshots,
        error::ApplicationError,
        request::{response, UpstreamRequestBuilder},
    },
    auth::UserAuthorizationService,
    client_sessions::ClientSessionService,
    config::SettingsView,
    libraries::VirtualLibraryService,
    media::MediaStorageService,
    playback::PlaybackRegistry,
    servers::{Server, ServerStorageService},
    translation::{
        response_processor::{ResponseProcessingContext, ResponseProcessingProfile},
        ProxyProcessors,
    },
};
use std::sync::Arc;

use super::request::selection::UrlSelection;

#[derive(Clone)]
pub(crate) struct MediaServices {
    pub(crate) reqwest_client: reqwest::Client,
    pub(crate) streaming_reqwest_client: reqwest::Client,
    pub(crate) user_authorization: Arc<UserAuthorizationService>,
    pub(crate) server_storage: Arc<ServerStorageService>,
    pub(crate) media_storage: Arc<MediaStorageService>,
    pub(crate) virtual_library_service: Arc<VirtualLibraryService>,
    pub(crate) playback_registry: Arc<PlaybackRegistry>,
    pub(crate) config: SettingsView,
    pub(crate) url_selection: UrlSelection,
    pub(crate) translation: Arc<ProxyProcessors>,
    pub(crate) client_sessions: Arc<ClientSessionService>,
    pub(crate) catalog_snapshots: Arc<CatalogSnapshots>,
    pub(crate) admin_rights: Arc<super::admin_rights::AdminRights>,
    /// Logs a device in again on a backend that refused its stored token.
    pub(crate) session_recovery: Arc<super::session_recovery::SessionRecovery>,
    /// Fixed at startup, like the router mount it mirrors.
    pub(crate) url_prefix: Option<String>,
}

impl MediaServices {
    pub(crate) fn url_prefix(&self) -> Option<&str> {
        self.url_prefix.as_deref()
    }

    pub(crate) fn remove_prefix<'a>(&self, path: &'a str) -> &'a str {
        crate::protocol::url::strip_url_prefix(path, self.url_prefix())
    }

    pub(crate) fn request_builder(&self) -> UpstreamRequestBuilder<'_> {
        UpstreamRequestBuilder::new(&self.translation.url_processor, self.url_prefix())
    }

    pub(crate) async fn deduplicate_media_enabled(&self) -> bool {
        self.config.read().await.deduplicate_media
    }

    pub(crate) async fn merge_libraries_enabled(&self) -> bool {
        self.config.read().await.merge_libraries
    }

    pub(crate) async fn process_response_json(
        &self,
        payload: &mut serde_json::Value,
        server: &Server,
        profile: ResponseProcessingProfile,
        token: Option<&str>,
    ) -> Result<bool, ApplicationError> {
        // Whether the viewer may manage this backend's items. Only from what is
        // already known: a response never waits for a lookup. When nothing is
        // known yet (first request after a restart, or an expired entry) the
        // lookup runs in the background so the next response can say.
        let can_manage = match token {
            Some(token) if profile != ResponseProcessingProfile::Disabled => {
                let known = self
                    .admin_rights
                    .known_administrator_for_token(&self.user_authorization, token, server)
                    .await;
                if known.is_none() {
                    self.refresh_administrator_in_background(token, server);
                }
                known
            }
            _ => None,
        };
        let context = ResponseProcessingContext {
            server: server.clone(),
            proxy_server_id: self.config.read().await.server_id.clone(),
            proxy_api_key: token.map(str::to_owned),
            profile,
            viewer_can_manage: can_manage == Some(true),
        };
        let modified = self
            .translation
            .process_response_json(payload, &context)
            .await?;
        // Tell the web client which items its viewer may manage.
        let annotated = match can_manage {
            Some(can_manage) => super::admin_rights::annotate_can_manage(payload, can_manage),
            None => false,
        };
        Ok(modified || annotated)
    }

    /// Look up the viewer's administrator flag on `server` without delaying the
    /// response being built. The result lands in the shared cache.
    fn refresh_administrator_in_background(&self, token: &str, server: &Server) {
        if !self.admin_rights.background_refresh_enabled() {
            return;
        }
        let services = self.clone();
        let token = token.to_owned();
        let server = server.clone();
        tokio::spawn(async move {
            services
                .admin_rights
                .refresh_for_token(
                    &services.reqwest_client,
                    &services.user_authorization,
                    &token,
                    &server,
                )
                .await;
        });
    }

    /// Run `execute` on `request`; if the backend answers 401 for the stored session token the
    /// request carried, log that device in again once and run it once more with the new token.
    /// Any other outcome, and a 401 that cannot be repaired, is returned as is.
    pub(crate) async fn with_recovery<T, F, Fut>(
        &self,
        server: &Server,
        request: reqwest::Request,
        execute: F,
    ) -> Result<T, ApplicationError>
    where
        F: Fn(reqwest::Request) -> Fut,
        Fut: std::future::Future<Output = Result<T, ApplicationError>>,
    {
        let spare = request.try_clone();
        let first = execute(request).await;
        let (
            Err(ApplicationError::UpstreamStatus(hyper::StatusCode::UNAUTHORIZED)),
            Some(mut again),
        ) = (&first, spare)
        else {
            return first;
        };
        let Some(stale) = super::session_recovery::request_token(&again) else {
            return first;
        };
        match self
            .session_recovery
            .recover_by_token(&self.user_authorization, server, &stale)
            .await
        {
            Some(new_token) => {
                super::session_recovery::swap_token(&mut again, &stale, &new_token);
                execute(again).await
            }
            None => first,
        }
    }

    pub(crate) async fn execute_processed_json(
        &self,
        request: reqwest::Request,
        server: &Server,
        profile: ResponseProcessingProfile,
        token: Option<&str>,
    ) -> Result<serde_json::Value, ApplicationError> {
        let mut payload = self
            .with_recovery(server, request, |request| async {
                response::execute_json_request::<serde_json::Value>(&self.reqwest_client, request)
                    .await
            })
            .await?;
        self.process_response_json(&mut payload, server, profile, token)
            .await?;
        Ok(payload)
    }
}
