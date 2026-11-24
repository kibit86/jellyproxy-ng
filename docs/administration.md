# Accounts, administration and the server panels

This fork changes how accounts work. **A proxy account is not a backend account.**
You have one administrator account on the proxy, and for each Jellyfin server you
choose which account on *that* server the proxy uses. Names and passwords may
differ everywhere.

## The one proxy account

- On the first start with an empty database the proxy creates the administrator
  from the configuration (`username` / `password`, i.e. `JELLYSWARRM_USERNAME` /
  `JELLYSWARRM_PASSWORD`). From then on **the database owns it**: change the
  password from **Profile** in the web UI. Changing the configuration later does
  nothing. If you are locked out, delete the user's row (or the database) and
  restart.
- Jellyfin apps sign in to the proxy with this account. The proxy never creates
  accounts from a backend login: `auto_create_users_on_login` is **off** by default.
- The administrator cannot be deleted from the Users page. Federation (creating
  users on the backends) and the per-server administrator accounts it needed are
  switched off and hidden.

## Mapping an account to each server

In the web UI (`/ui`, the main page), **My servers**: press **Connect** next to a
server and enter the username and password of the account to use there. That
account decides what you see and what you may do on that server (libraries, watch
state, favourites, and whether you are an administrator). Reconnect to change it.

Apps see the merged libraries of all the servers you mapped.

## Opening a server's own dashboard

Next to every server where the mapped account is an administrator, **Administer**
opens that server's own web client and dashboard, **already signed in with the
mapped account**, through the proxy (`/ui/backend/<id>/web/#/dashboard`). The
server is not exposed anywhere else.

- The page's browser storage is kept apart per server (and from the proxy's own
  web client), so the servers and the proxy never mix up their sessions.
- The proxy signs in on the server as the device `jellyswarrm-panel-<server>-<user>`;
  the server reuses one session however often you open the dashboard.
- No button, or a 403, means the mapped account is not an administrator there.
- Everything is forwarded unchanged (HTTP and WebSocket) except the web client's
  start page, which gets a small script that sets up the storage and the session.

## Managing items from apps and the web client

Editing metadata and images, identifying, refreshing and deleting items work from
the usual client menus, on the servers where the mapped account is an administrator.

- Requests that only an administrator may make (`/Items/{id}/Refresh`, `/Images`,
  `/MetadataEditor`, `/RemoteSearch`, `/ExternalIdInfos`, `/ContentType`, subtitle
  and lyric writes) go **only** to servers where the mapped account is an
  administrator; elsewhere they get 403 without reaching a server.
- `POST|DELETE /Items/{id}` is also how users edit or delete their own playlists
  and collections, so it is only steered to an administrator server when one owns
  the item; otherwise the owning server decides with the account's own rights.
- For a movie present on several servers an edit or delete acts on one copy: the one
  on the highest-priority server where the account is an administrator.
- Server-wide administration (`System/Configuration`, `Library/VirtualFolders`,
  `ScheduledTasks`, `Plugins`, `Devices`, ...) is refused (403) on the merged API:
  it has no single target. Use the server's panel instead.

How clients learn what to show:

- **Native apps** see `IsAdministrator = true` when the mapped account is an
  administrator on at least one server, and get a 403 on items elsewhere. They read
  `CanDelete` on every item to decide whether to offer "Delete": the proxy passes on
  the backend's own answer for items of a server where the mapped account is an
  administrator (so the Jellyfin permission "allow media deletion" still applies), and
  tells `false` everywhere else, for library views, and for downloads. Deleting a title
  that exists on two servers removes the copy on the server where the account administers.
- **The bundled web client** is told `false` and reads `JellyswarrmCanManage` on
  every item (needs `patches/jellyfin-web/*.patch`, applied by the `Dockerfile`).

## Network discovery

Jellyfin apps find servers with a UDP broadcast on port 7359; the proxy answers it
like a Jellyfin server. Controlled by `autodiscovery` (default `true`, environment
`JELLYSWARRM_AUTODISCOVERY`) and a switch in **Settings**; no restart needed. Only one
server per host may own the port: if another holds it, the proxy logs one error, keeps
running without discovery and retries every few seconds.

## Trying it from scratch

`dev/e2e-fresh-install/` holds a Docker + Chrome test that starts two brand-new
Jellyfin servers and an empty proxy, creates the users everywhere and walks through
all of the above.

## Tokens and devices (from 2.0.0)

The proxy follows Jellyfin's model. Each **device** that logs in (one row per user and device id) gets
its own proxy token; logging the same device in again replaces that token (the old one stops working)
and signing out removes the device. Other devices of the same user are not affected.

- **After upgrading to 2.0.0 every app must log in again** (Quick Connect works): the old
  per-user token is no longer accepted and the stored backend sessions are dropped.
- A server that refuses a stored session token (for example because the account was signed out there)
  is logged in again automatically with the saved credentials. A server connected with **Quick Connect**
  cannot be repaired this way: reconnect it in *My servers*.
- Read-only API keys are separate credentials and are not devices.

