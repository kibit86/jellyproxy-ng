//! Startup resource inputs versus the explicit set of dynamically publishable
//! values. AppConfig remains the persisted/backwards-compatible file envelope.
use super::{AppConfig, DebugUser, PreconfiguredServer, UrlSegment};
use crate::domain::security::Password;

/// Immutable, scoped administrator credential (the configured password: the key
/// protecting data saved before administrators became real users), without
/// configuration mutation.
#[derive(Clone)]
pub(crate) struct AdminCredentials {
    password: Password,
}
impl AdminCredentials {
    pub(crate) fn password(&self) -> Password {
        self.password.clone()
    }
}

// Deliberately no Debug: session keys must never appear in diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct StartupConfig {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) timeout: u64,
    pub(crate) ui_route: UrlSegment,
    pub(crate) url_prefix: Option<UrlSegment>,
    pub(crate) session_key: Vec<u8>,
    pub(crate) server_background_check_interval_secs: u64,
    username: String,
    password: Password,
    preconfigured_servers: Vec<PreconfiguredServer>,
    debug_user: Option<DebugUser>,
}

#[derive(Clone)]
pub(crate) struct RuntimeSettings {
    pub(crate) server_id: String,
    pub(crate) public_address: String,
    pub(crate) server_name: String,
    pub(crate) include_server_name_in_media: bool,
    pub(crate) auto_create_users_on_login: bool,
    pub(crate) merge_libraries: bool,
    pub(crate) deduplicate_media: bool,
    pub(crate) autodiscovery: bool,
}

/// Read-only capability for request workflows. No administrator password,
/// session key, listener binding or configured backend bootstrap input is
/// reachable through this view. Reads return an owned, internally consistent
/// snapshot and never retain a configuration lock across upstream I/O.
#[derive(Clone)]
pub(crate) struct SettingsView(std::sync::Arc<tokio::sync::RwLock<AppConfig>>);

pub(crate) struct RequestSettings {
    runtime: RuntimeSettings,
    pub(crate) timeout: u64,
    pub(crate) url_prefix: Option<UrlSegment>,
}
impl std::ops::Deref for RequestSettings {
    type Target = RuntimeSettings;
    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}
impl SettingsView {
    pub(crate) fn new(config: std::sync::Arc<tokio::sync::RwLock<AppConfig>>) -> Self {
        Self(config)
    }
    pub(crate) async fn read(&self) -> RequestSettings {
        let config = self.0.read().await;
        RequestSettings {
            runtime: config.runtime_settings(),
            timeout: config.timeout,
            url_prefix: config.url_prefix.clone(),
        }
    }
}

impl AppConfig {
    pub(crate) fn admin_credentials(&self) -> AdminCredentials {
        AdminCredentials {
            password: self.password.clone(),
        }
    }
    pub(crate) fn startup_config(&self) -> StartupConfig {
        StartupConfig {
            host: self.host.clone(),
            port: self.port,
            timeout: self.timeout,
            ui_route: self.ui_route.clone(),
            url_prefix: self.url_prefix.clone(),
            session_key: self.session_key.clone(),
            server_background_check_interval_secs: self.server_background_check_interval_secs,
            username: self.username.clone(),
            password: self.password.clone(),
            preconfigured_servers: self.preconfigured_servers.clone(),
            debug_user: self.debug_user.clone(),
        }
    }

    pub(crate) fn runtime_settings(&self) -> RuntimeSettings {
        RuntimeSettings {
            server_id: self.server_id.clone(),
            public_address: self.public_address.clone(),
            server_name: self.server_name.clone(),
            include_server_name_in_media: self.include_server_name_in_media,
            auto_create_users_on_login: self.auto_create_users_on_login,
            merge_libraries: self.merge_libraries,
            deduplicate_media: self.deduplicate_media,
            autodiscovery: self.autodiscovery,
        }
    }

    pub(super) fn publish_runtime_settings(&mut self, settings: RuntimeSettings) {
        self.server_id = settings.server_id;
        self.public_address = settings.public_address;
        self.server_name = settings.server_name;
        self.include_server_name_in_media = settings.include_server_name_in_media;
        self.auto_create_users_on_login = settings.auto_create_users_on_login;
        self.merge_libraries = settings.merge_libraries;
        self.deduplicate_media = settings.deduplicate_media;
        self.autodiscovery = settings.autodiscovery;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn scoped_credentials_are_immutable_and_settings_remain_secret_free() {
        let config = std::sync::Arc::new(tokio::sync::RwLock::new(AppConfig::default()));
        let credentials = config.read().await.admin_credentials();
        let original_password = credentials.password();
        let settings = SettingsView::new(config.clone());
        {
            let mut config = config.write().await;
            config.password = "different test password".into();
            config.server_name = "published name".into();
        }
        assert_eq!(credentials.password(), original_password);
        assert_eq!(settings.read().await.server_name, "published name");
    }

    #[tokio::test]
    async fn request_view_owns_a_coherent_snapshot_without_holding_the_config_lock() {
        let config = std::sync::Arc::new(tokio::sync::RwLock::new(AppConfig::default()));
        let view = SettingsView::new(config.clone());
        let before = view.read().await;
        let original_name = before.server_name.clone();
        // A retained request snapshot cannot block configuration publication.
        let mut updated = config
            .try_write()
            .expect("view retained a configuration lock");
        updated.server_name = "new identity".into();
        updated.deduplicate_media = !before.deduplicate_media;
        drop(updated);
        let after = view.read().await;
        assert_eq!(before.server_name, original_name);
        assert_eq!(after.server_name, "new identity");
        assert_ne!(before.deduplicate_media, after.deduplicate_media);
        assert_eq!(before.url_prefix, after.url_prefix);
    }
}
