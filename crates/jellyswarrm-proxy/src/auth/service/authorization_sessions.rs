//! Persist, encrypt, rebind, and revoke backend authorization sessions.
use super::*;

const SESSIONS_SELECT: &str = r#"
    SELECT
        auth.id as auth_id,
        auth.user_id as auth_user_id,
        auth.mapping_id as auth_mapping_id,
        sm.server_url as auth_server_url,
        auth.client,
        auth.device,
        auth.device_id,
        auth.version,
        auth.jellyfin_token,
        auth.token_format,
        auth.original_user_id,
        auth.expires_at,
        auth.created_at as auth_created_at,
        auth.updated_at as auth_updated_at,

        s.id as server_id,
        s.name as server_name,
        s.url as server_url_full,
        s.priority,
        s.created_at as server_created_at,
        s.updated_at as server_updated_at
    FROM authorization_sessions auth
    JOIN server_mappings sm ON auth.mapping_id = sm.id
    JOIN servers s ON sm.server_id = s.id"#;

const SESSIONS_FILTER: &str =
    "AND (auth.expires_at IS NULL OR auth.expires_at > ?) ORDER BY s.priority DESC, s.name ASC";

impl UserAuthorizationService {
    /// Encrypt old session tokens before accepting requests. Legacy rows cannot
    /// be left in the DB indefinitely: a Quick Connect session holds the same
    /// durable backend token as its mapping.
    pub(crate) async fn enable_session_encryption(
        &mut self,
        secret: &[u8],
    ) -> Result<(), sqlx::Error> {
        let key = MappingEncryptionKey::for_authorization_sessions(secret)
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(
            "SELECT id, jellyfin_token FROM authorization_sessions WHERE token_format = 'legacy'",
        )
        .fetch_all(&mut *tx)
        .await?;
        for row in rows {
            let id: i64 = row.try_get("id")?;
            let Some(token): Option<String> = row.try_get("jellyfin_token")? else {
                // Very old rows may have no upstream session at all.
                continue;
            };
            let encrypted = key
                .encrypt(&Password::from(token))
                .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
            sqlx::query("UPDATE authorization_sessions SET jellyfin_token = ?, token_format = 'session_v1' WHERE id = ?")
                .bind(encrypted.as_str()).bind(id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.session_key = Some(key);
        Ok(())
    }

    /// Store authorization session
    #[cfg(not(test))]
    pub(crate) async fn store_authorization_session(
        &self,
        user_id: &str,
        server: &Server,
        authorization: &Authorization,
        jellyfin_token: String,
        original_user_id: String,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<i64, sqlx::Error> {
        self.store_authorization_session_by_id(
            user_id,
            server.id,
            server.url.as_str(),
            authorization,
            jellyfin_token,
            original_user_id,
            expires_at,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn store_authorization_session<'a, S>(
        &self,
        user_id: &str,
        server: S,
        authorization: &Authorization,
        jellyfin_token: String,
        original_user_id: String,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<i64, sqlx::Error>
    where
        S: Into<ServerReference<'a>>,
    {
        let (server_id, server_url) = self.resolve_server_reference(server.into()).await?;
        self.store_authorization_session_by_id(
            user_id,
            server_id,
            &server_url,
            authorization,
            jellyfin_token,
            original_user_id,
            expires_at,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn store_authorization_session_by_id(
        &self,
        user_id: &str,
        server_id: ServerId,
        server_url: &str,
        authorization: &Authorization,
        jellyfin_token: String,
        original_user_id: String,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<i64, sqlx::Error> {
        let now = chrono::Utc::now();
        #[cfg(not(test))]
        if self.session_key.is_none() {
            return Err(sqlx::Error::Protocol(
                "session encryption key is required".into(),
            ));
        }
        let (jellyfin_token, token_format) = if let Some(key) = &self.session_key {
            (
                key.encrypt(&Password::from(jellyfin_token))
                    .map_err(|e| sqlx::Error::Protocol(e.to_string()))?
                    .into_inner(),
                "session_v1",
            )
        } else {
            (jellyfin_token, "legacy")
        };

        // Find mapping to obtain mapping_id (required for referential integrity & cascade deletes)
        let mapping = self
            .get_server_mapping_by_server_id(user_id, server_id)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?;

        // The session belongs to the device row of this user and device id (created here if the
        // device did not go through the regular login).
        let client_device = self.device_for_session(user_id, authorization).await?;

        let session_id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO authorization_sessions
            (user_id, mapping_id, server_url, client, device, device_id, version, jellyfin_token, token_format, original_user_id, expires_at, created_at, updated_at, client_device_id)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(user_id, mapping_id, device_id) DO UPDATE SET
                client_device_id = excluded.client_device_id,
                server_url = excluded.server_url,
                client = excluded.client,
                device = excluded.device,
                version = excluded.version,
                jellyfin_token = excluded.jellyfin_token,
                token_format = excluded.token_format,
                original_user_id = excluded.original_user_id,
                expires_at = excluded.expires_at,
                updated_at = excluded.updated_at
            RETURNING id
            "#,
        )
        .bind(user_id)
        .bind(mapping.id)
        .bind(server_url)
        .bind(&authorization.client)
        .bind(&authorization.device)
        .bind(&authorization.device_id)
        .bind(&authorization.version)
        .bind(jellyfin_token)
        .bind(token_format)
        .bind(original_user_id)
        .bind(expires_at)
        .bind(now)
        .bind(now)
        .bind(client_device.id)
        .fetch_one(&self.pool)
        .await?;

        info!(
            "Stored authorization session for user {} on server {}",
            user_id, server_url
        );
        Ok(session_id)
    }

    /// Get authorization sessions and servers for a user by user ID
    pub(crate) async fn get_user_sessions_by_user_id(
        &self,
        user_id: &str,
    ) -> Result<Option<(LocalUser, Vec<(AuthorizationSession, Server)>)>, sqlx::Error> {
        let Some(user) = self.get_user_by_id(user_id).await? else {
            return Ok(None);
        };

        let sessions = self.get_user_sessions(&user.id, None).await?;
        Ok(Some((user, sessions)))
    }

    /// Every non-expired session of a user, across all devices, with its server.
    /// `device` (legacy header matching) narrows the list; the per-device lookup below is the
    /// way requests find their sessions.
    pub(crate) async fn get_user_sessions(
        &self,
        user_id: &str,
        device: Option<Device>,
    ) -> Result<Vec<(AuthorizationSession, Server)>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "{SESSIONS_SELECT} WHERE auth.user_id = ? {SESSIONS_FILTER}"
        ))
        .bind(user_id)
        .bind(chrono::Utc::now())
        .fetch_all(&self.pool)
        .await?;
        let sessions = self.decode_session_rows(rows)?;
        debug!("Found {} sessions for user_id: {}", sessions.len(), user_id);

        let sessions = if let Some(device) = device {
            debug!("Filtering sessions for device: {:?}", device);
            sessions
                .into_iter()
                .filter(|(session, _)| device.matches(&session.device))
                .collect()
        } else {
            sessions
        };

        Ok(sessions)
    }

    /// The sessions of one device row: what a request authenticated by that device's token may
    /// use. No header strings are compared.
    pub(crate) async fn get_device_sessions(
        &self,
        client_device_id: i64,
    ) -> Result<Vec<(AuthorizationSession, Server)>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "{SESSIONS_SELECT} WHERE auth.client_device_id = ? {SESSIONS_FILTER}"
        ))
        .bind(client_device_id)
        .bind(chrono::Utc::now())
        .fetch_all(&self.pool)
        .await?;
        self.decode_session_rows(rows)
    }

    /// The stored session of `server` that holds this backend token, if any. Tokens are encrypted
    /// at rest, so the sessions of the server are decrypted and compared (a handful of rows).
    pub(crate) async fn find_session_by_backend_token(
        &self,
        server_id: ServerId,
        token: &str,
    ) -> Result<Option<AuthorizationSession>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "{SESSIONS_SELECT} WHERE s.id = ? {SESSIONS_FILTER}"
        ))
        .bind(server_id.as_i64())
        .bind(chrono::Utc::now())
        .fetch_all(&self.pool)
        .await?;
        Ok(self
            .decode_session_rows(rows)?
            .into_iter()
            .map(|(session, _)| session)
            .find(|session| session.jellyfin_token == token))
    }

    fn decode_session_rows(
        &self,
        rows: Vec<sqlx::sqlite::SqliteRow>,
    ) -> Result<Vec<(AuthorizationSession, Server)>, sqlx::Error> {
        rows.into_iter()
            .map(|row| {
                let mut session = AuthorizationSession::from_user_sessions_row(&row)?;
                if row.try_get::<String, _>("token_format")? == "session_v1" {
                    let key = self.session_key.as_ref().ok_or_else(|| {
                        sqlx::Error::Protocol("session encryption key unavailable".into())
                    })?;
                    session.jellyfin_token = key
                        .decrypt(&EncryptedPassword::from_raw(session.jellyfin_token))
                        .map_err(|e| sqlx::Error::Protocol(e.to_string()))?
                        .into_inner();
                }
                Ok((session, Server::from_session_join_row(&row)?))
            })
            .collect()
    }

    /// Get counts of authorization sessions per server for a user.
    pub(crate) async fn session_counts_by_server(
        &self,
        user_id: &str,
    ) -> Result<Vec<(String, i64)>, sqlx::Error> {
        let rows = sqlx::query(
            r#"SELECT s.url as url_norm, COUNT(*) as cnt
                FROM authorization_sessions auth
                JOIN server_mappings sm ON auth.mapping_id = sm.id
                JOIN servers s ON sm.server_id = s.id
                WHERE auth.user_id = ?
                GROUP BY sm.server_id"#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get::<String, _>("url_norm"), r.get::<i64, _>("cnt")))
            .collect())
    }

    /// Delete all authorization sessions for a given user.
    pub(crate) async fn delete_all_sessions_for_user(
        &self,
        user_id: &str,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query("DELETE FROM authorization_sessions WHERE user_id = ?")
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    /// Delete authorization sessions for a specific mapping.
    #[cfg(test)]
    pub(crate) async fn delete_sessions_for_mapping(
        &self,
        mapping_id: i64,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query("DELETE FROM authorization_sessions WHERE mapping_id = ?")
            .bind(mapping_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }
}
