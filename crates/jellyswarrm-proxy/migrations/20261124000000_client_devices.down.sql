DROP INDEX IF EXISTS idx_authorization_sessions_device_mapping;
ALTER TABLE authorization_sessions DROP COLUMN client_device_id;
DROP TABLE IF EXISTS client_devices;
