//! Per-device proxy tokens, modelled on Jellyfin's `Devices` table.
//!
//! * One row per (user, device id). The token belongs to the device.
//! * A new login from the same device **replaces** the row: the old token is revoked and its
//!   backend sessions are deleted with it (cascade), as Jellyfin does.
//! * A request is resolved from the token alone. The `DeviceId` header is never compared with the
//!   stored one; `Device`/`Version` in the header only refresh the stored metadata.
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::Row;

use super::UserAuthorizationService;
use crate::auth::models::LocalUser;
use crate::protocol::authorization::Authorization;

/// A device that has logged in to the proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientDevice {
    pub(crate) id: i64,
    pub(crate) user_id: String,
    /// The device id the client sent. Identity of the row, never used to find sessions.
    pub(crate) device_id: String,
    pub(crate) client: String,
    pub(crate) device_name: String,
    pub(crate) version: String,
    pub(crate) last_activity: DateTime<Utc>,
}

impl ClientDevice {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            user_id: row.try_get("user_id")?,
            device_id: row.try_get("device_id")?,
            client: row.try_get("client")?,
            device_name: row.try_get("device_name")?,
            version: row.try_get("version")?,
            last_activity: row.try_get("last_activity")?,
        })
    }
}

/// SHA-256 of the token, in hex. Only this is stored.
pub(crate) fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// A fresh opaque token: 32 random bytes in hex.
fn new_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// How often `last_activity` is rewritten, like Jellyfin's three minutes.
const ACTIVITY_REFRESH: chrono::Duration = chrono::Duration::minutes(3);

impl UserAuthorizationService {
    /// Log a device in: replace any row of the same user and device id (revoking its token and
    /// deleting its backend sessions) and return the new row with its token in clear.
    pub(crate) async fn login_device(
        &self,
        user_id: &str,
        authorization: &Authorization,
    ) -> Result<(ClientDevice, String), sqlx::Error> {
        let token = new_token();
        let hash = token_hash(&token);
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM client_devices WHERE user_id = ? AND device_id = ?")
            .bind(user_id)
            .bind(&authorization.device_id)
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query(
            r#"
            INSERT INTO client_devices
                (user_id, device_id, client, device_name, version, token_hash, created_at, last_activity)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            RETURNING id, user_id, device_id, client, device_name, version, last_activity
            "#,
        )
        .bind(user_id)
        .bind(&authorization.device_id)
        .bind(&authorization.client)
        .bind(&authorization.device)
        .bind(&authorization.version)
        .bind(&hash)
        .bind(now)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((ClientDevice::from_row(&row)?, token))
    }

    /// The device row for this user and device id, created with an unusable token if missing.
    /// Used when a backend session is stored for a device that did not log in through the
    /// regular login (administrative flows, fixtures): it never replaces an existing row.
    pub(crate) async fn device_for_session(
        &self,
        user_id: &str,
        authorization: &Authorization,
    ) -> Result<ClientDevice, sqlx::Error> {
        let now = Utc::now();
        let unusable = token_hash(&format!("unusable-{}", uuid::Uuid::new_v4()));
        let row = sqlx::query(
            r#"
            INSERT INTO client_devices
                (user_id, device_id, client, device_name, version, token_hash, created_at, last_activity)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(user_id, device_id) DO UPDATE SET
                client = excluded.client,
                device_name = excluded.device_name,
                version = excluded.version
            RETURNING id, user_id, device_id, client, device_name, version, last_activity
            "#,
        )
        .bind(user_id)
        .bind(&authorization.device_id)
        .bind(&authorization.client)
        .bind(&authorization.device)
        .bind(&authorization.version)
        .bind(unusable)
        .bind(now)
        .bind(now)
        .fetch_one(&self.pool)
        .await?;
        ClientDevice::from_row(&row)
    }

    /// Resolve a device token to its user and device. Nothing else identifies a device.
    pub(crate) async fn find_device_by_token(
        &self,
        token: &str,
    ) -> Result<Option<(LocalUser, ClientDevice)>, sqlx::Error> {
        let hash = token_hash(token);
        let Some(row) = sqlx::query(
            "SELECT id, user_id, device_id, client, device_name, version, last_activity \
             FROM client_devices WHERE token_hash = ?",
        )
        .bind(&hash)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };
        let device = ClientDevice::from_row(&row)?;
        let Some(user) = self.get_user_by_id(&device.user_id).await? else {
            return Ok(None);
        };
        Ok(Some((user, device)))
    }

    /// Refresh `last_activity` (at most every few minutes) and the metadata the client reports now.
    /// The device id and the token are never changed here.
    pub(crate) async fn touch_device(
        &self,
        device: &ClientDevice,
        seen: Option<&Authorization>,
    ) -> Result<(), sqlx::Error> {
        let now = Utc::now();
        let changed = seen.is_some_and(|a| {
            (!a.device.is_empty() && a.device != device.device_name)
                || (!a.version.is_empty() && a.version != device.version)
        });
        if !changed && now - device.last_activity < ACTIVITY_REFRESH {
            return Ok(());
        }
        let (name, version) = match seen {
            Some(a) => (
                if a.device.is_empty() {
                    &device.device_name
                } else {
                    &a.device
                },
                if a.version.is_empty() {
                    &device.version
                } else {
                    &a.version
                },
            ),
            None => (&device.device_name, &device.version),
        };
        sqlx::query(
            "UPDATE client_devices SET last_activity = ?, device_name = ?, version = ? WHERE id = ?",
        )
        .bind(now)
        .bind(name)
        .bind(version)
        .bind(device.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Remove a device (logout): its token stops working and its backend sessions go with it.
    pub(crate) async fn delete_device(&self, device_id: i64) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM client_devices WHERE id = ?")
            .bind(device_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}
