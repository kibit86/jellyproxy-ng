-- Token per dispositivo, come Jellyfin: una riga per (utente, DeviceId del client) con un solo token.
-- Un nuovo login dello stesso dispositivo sostituisce la riga (e, a cascata, le sue sessioni verso i server).
-- L'hash del token e' SHA-256 in esadecimale: il token in chiaro esiste solo nella risposta di login.
CREATE TABLE client_devices (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    client TEXT NOT NULL,
    device_name TEXT NOT NULL,
    version TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    last_activity TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE,
    UNIQUE (user_id, device_id)
);

-- Le sessioni esistenti non hanno un dispositivo e i client ricevevano il token unico per utente,
-- che non vale piu': tutte le app rifanno il login.
DELETE FROM authorization_sessions;

ALTER TABLE authorization_sessions
    ADD COLUMN client_device_id INTEGER REFERENCES client_devices (id) ON DELETE CASCADE;

CREATE UNIQUE INDEX idx_authorization_sessions_device_mapping
    ON authorization_sessions (client_device_id, mapping_id)
    WHERE client_device_id IS NOT NULL;
