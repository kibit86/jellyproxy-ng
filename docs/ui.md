# JellyProxy-NG UI Documentation

The management UI is at `http://[JELLYSWARRM_HOST]:[JELLYSWARRM_PORT]/ui`. What you see depends on who signs in:

| | Menu |
|---|---|
| **Proxy administrator** | My servers · Servers · Users · Libraries · Settings · Profile |
| **Other users** | Servers (their own connections) · Media · Profile |

The account model behind it, and how the server panels and item management work, is in
[administration.md](administration.md).

## Signing in the first time

The proxy creates its **one administrator** when it starts for the first time, from `JELLYSWARRM_USERNAME` and
`JELLYSWARRM_PASSWORD`. From then on the password is in the database: change it from **Profile**. Changing the variable
later does nothing; it only matters again if the database is deleted. The administrator cannot be deleted.

## Adding servers (Servers, administrator)

<p align="center">
  <img src="../media/servers.png" alt="Server management" width="90%">
</p>

1. Open the **Servers** section.
2. Fill in the details:
   - **Server Name** – A friendly display name for the server.
   - **Jellyfin URL** – The full URL to your Jellyfin server (e.g. `http://jellyfin.example.com`). It is also stored in
     the mappings and sessions: to change it later, remove the server and add it again (mappings are redone).
   - **Priority** – Determines which server takes precedence for tasks not handled by the proxy (such as theming/styling). Higher numbers = higher priority.
3. Click **Add**.

All media always passes through JellyProxy-NG (there is no direct/redirect mode): clients never need to reach the Jellyfin servers. To remove a server, click **Delete**.
Servers can also be listed in the configuration file (`preconfigured_servers`, see [config.md](config.md)).

## My servers

<p align="center">
  <img src="../media/my_servers.png" alt="My servers" width="90%">
</p>

The administrator lands here after signing in; other users find it under **Servers**. The page lists the servers the user is
connected to, and the available ones.

- **Connect** – link the account you use *on that server*. Choose **Password** (name and password of the Jellyfin account
  there, which may differ from your proxy login) or **Quick Connect**: a code to approve in an already signed-in Jellyfin
  client on that *backend* server, where Quick Connect must be enabled. The code is temporary; the proxy saves the
  resulting backend access token for future logins. If the token is revoked, choose **Reconnect** and sign in again.
  Jellyfin does not provide a refresh token for this flow. The credentials are checked on the server before the mapping is
  saved. Apps already signed in to the proxy may need to sign in again to pick up a newly linked server.
- **Disconnect** – remove the link.
- **Open**, **Set up**, **Administer** (administrator only, each opens in a **new tab**, signed in through the proxy):
  - **Open** – the server's own web client, on a server with no mapped account yet: sign in there yourself.
  - **Set up** – the same, for a server that was never configured: its first-run wizard opens through the proxy, so you can
    create its first account.
  - **Administer** – appears on a connected server where the mapped account is a Jellyfin administrator: opens that server's
    dashboard already signed in, with no second login.

A user who is not the administrator never sees **Open** or **Set up**, and sees **Administer** only where their own mapped
account administers the server.

## Users (administrator)

<p align="center">
  <img src="./images/add_mapping.png" alt="A user with a server mapping, and the form to add another" width="90%">
</p>

Other users are optional; the administrator alone is enough for one person or one household using a single login.

- **Add User** – a username and a password for the proxy. Nothing is created on any Jellyfin server.
- **Server mappings** – for each user, which account to use on each server: choose the server, enter the **username** and
  **password** of the Jellyfin account there, **Add**. Under each mapping the card shows the server, the mapped user, the
  authentication method and **Sessions**: the number of backend sessions the proxy keeps open for it.
- **Reset Sessions** – closes those sessions; the next use signs in again with the saved credentials. Use it after fixing
  or recreating an account on a server.
- **Delete** a mapping to unlink one server, or **Delete User** to remove the user with its mappings and sessions. Neither
  touches a Jellyfin server.
- If `auto_create_users_on_login` is on (it is **off by default**), a login with an account that exists on a connected server
  creates a proxy user and its mappings. Leave it off unless you want that.

Mapping credentials and backend tokens in per-client authorization sessions are encrypted using separate purpose-specific
keys derived from the persistent `session_key` in `jellyswarrm.toml`, not from any user password, so changing a password
never invalidates them. Back up that configuration file together with the database: losing or changing `session_key` makes
encrypted mappings and sessions unreadable. New local user passwords are stored with salted Argon2id hashes. The initial
administrator password given in the environment is also kept in the configuration file: protect it (files written by the
proxy are owner-only on Unix).

Login-time migration also supports legacy mappings from older versions. Before changing a local password, the proxy validates
any remaining ambiguous legacy mappings directly with their servers; if validation fails the password stays unchanged and the
UI asks you to check connectivity or reconnect the affected accounts from **My servers**, then retry.

## Settings (administrator)

<p align="center">
  <img src="./images/settings.png" alt="Settings" width="90%">
</p>

The **Settings** section changes global options at runtime; edits persist to the TOML file (**Save**, **Reset**, **Reload
From Disk**):

- **Server ID** – read-only.
- **Public Address** – The external address reported to Jellyfin clients and announced by LAN discovery.
- **Server Name** – The display name shown to users when they log in.
- **Add Server Names to Media** – When enabled, the server name is appended to media titles: `Title [Server Name]`.
- **Auto Create Users On Login** – see **Users** above.
- **Merge Duplicate Movies & Shows Across Servers** – one item for the same title on several servers, with the copies as versions.
- **Answer Network Discovery (UDP 7359)** – let Jellyfin apps on the LAN find the proxy. Applies without a restart. Only one
  server per host may own the port (and a Docker bridge network does not forward the broadcast: use host networking).
