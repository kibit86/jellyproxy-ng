# JellyProxy-NG Configuration Documentation

JellyProxy-NG stores its configuration in a **TOML** file located at:
`./data/jellyswarrm.toml` (inside the container).

The SQLite database is stored at:
`./data/jellyswarrm.db`.

To persist your configuration and database across container restarts, mount a volume to the `./data` directory.

> The program keeps the upstream names for its files and settings (`jellyswarrm.toml`, `jellyswarrm.db`,
> `JELLYSWARRM_*`) on purpose, so that existing installations and merges from upstream keep working.

You can change the configuration in three ways:
1. The **Settings** page of the web UI (edits are written to the TOML file).
2. Your own `jellyswarrm.toml`, mounted into the container.
3. **Environment variables, for single-word keys only** (see below).

---

## Environment variables

Environment variables are read with the prefix `JELLYSWARRM` and `_` as the separator between nested levels. As a
result **only keys whose name is a single word are read from the environment**:

| Environment variable | Key |
|---|---|
| `JELLYSWARRM_USERNAME` | `username` (creates the administrator at first start only) |
| `JELLYSWARRM_PASSWORD` | `password` (the same: first start only) |
| `JELLYSWARRM_HOST` | `host` |
| `JELLYSWARRM_PORT` | `port` |
| `JELLYSWARRM_TIMEOUT` | `timeout` |
| `JELLYSWARRM_AUTODISCOVERY` | `autodiscovery` |

A key with an underscore in its name (`server_name`, `public_address`, `session_key`, …) is **not** picked up from a
variable such as `JELLYSWARRM_SERVER_NAME`: set it in `jellyswarrm.toml` or in the Settings page instead.
Two variables are read directly and always work: `JELLYSWARRM_DATA_DIR` (the data directory, `./data` by default)
and `RUST_LOG` (log level, for example `jellyswarrm_proxy=debug`).

---

## Configuration Options

| Key | Default Value | Environment | Description |
|-----|---------------|-------------|-------------|
| `server_id` | *Generated UUID (32 hex chars)* | – | Unique identifier of the proxy server instance. |
| `public_address` | `localhost:3000` | – | Public address clients should use to reach the proxy; also what LAN discovery announces. |
| `server_name` | `JellyProxy-NG` | – | Display name of the proxy server. |
| `host` | `0.0.0.0` | `JELLYSWARRM_HOST` | Host address the server binds to. |
| `port` | `3000` | `JELLYSWARRM_PORT` | Port number of the proxy server. |
| `include_server_name_in_media` | `true` | – | Append the server name to media titles in responses. |
| `username` | `admin` | `JELLYSWARRM_USERNAME` | Name of the proxy administrator, **created at first start** if none exists. |
| `password` | `jellyswarrm` | `JELLYSWARRM_PASSWORD` | Password of that administrator at creation (⚠️ change it). Afterwards the database owns the password: change it from **Profile**. |
| `session_key` | *Generated 64-byte key* | – | Base64-encoded key from which mapping credentials and sessions are encrypted. Losing or changing it makes them unreadable. |
| `timeout` | `20` | `JELLYSWARRM_TIMEOUT` | Request timeout in seconds. |
| `preconfigured_servers` | `[]` | – | Optional list of Jellyfin servers (`url`, `name`, `priority`) added at start. They are matched **by URL** on every start. |
| `ui_route` | `ui` | – | URL path segment of the web UI (for example `/ui`). |
| `url_prefix` | *(none)* | – | Optional URL prefix for all routes (reverse proxy setups). |
| `server_background_check_interval_secs` | `30` | – | Interval in seconds of the background server health checks. |
| `auto_create_users_on_login` | `false` | – | When `true`, a login with an account that exists on a connected server creates a proxy user and its mappings. **Off by default in this fork** (accounts are mapped explicitly). |
| `merge_libraries` | `true` | – | Merge libraries with matching names across servers into virtual libraries. |
| `deduplicate_media` | `true` | – | Collapse the same movie or show (series/season/episode) on multiple servers into one item whose versions are served by the different hosts (Jellyfin-style versions). |
| `autodiscovery` | `true` | `JELLYSWARRM_AUTODISCOVERY` | Answer Jellyfin's UDP 7359 LAN broadcast. Also a switch in **Settings**, no restart needed. |
| `debug_user` | *(none)* | – | **Debug builds only**: a local user and its server mappings created at start (see `data/jellyswarrm.dev.toml`). |

---

### Notes
- The `session_key` is generated as a secure 64-byte key if not specified, and is stored in the config file for reuse.
- Media always goes through the proxy. The old `media_streaming_mode` key (global or per preconfigured server) is no longer used: if an old file still contains it, it is ignored.
- Configuration files are resolved from the data directory (`./data` by default), which can be overridden with `JELLYSWARRM_DATA_DIR`.
- The file is written with owner-only permissions and also contains the administrator password given at first start:
  protect it.
