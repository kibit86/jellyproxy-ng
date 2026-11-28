> [!NOTE]
> **JellyProxy-NG is a vibe-coded fork** of [LLukas22/Jellyswarrm](https://github.com/LLukas22/Jellyswarrm), made for one homelab and shared as is. With an AI coding assistant (Claude) it changes how accounts work and adds a few things on top of upstream v1.0.0: **one proxy account** with **one mapped account per Jellyfin server** (names and passwords may differ; apps sign in to the proxy), **each server's own dashboard opened already signed in** from the proxy (and the first-run setup of a brand-new server done through it), media management on the servers where the mapped account is an administrator, and Jellyfin LAN discovery (UDP 7359). See [docs/administration.md](docs/administration.md). Images are published as `ghcr.io/kibit86/jellyproxy-ng` (up to 1.1.0 the same builds were published as `ghcr.io/kibit86/jellyswarrm`, which is no longer updated). The program, its settings (`JELLYSWARRM_*`), its files (`jellyswarrm.toml`, `jellyswarrm.db`) and its Rust crates keep the original names on purpose, so nothing has to be migrated and upstream changes stay easy to merge (SemVer tags; how releases work: [docs/releasing.md](docs/releasing.md)). Everything else is upstream's work. Use at your own risk, with no support: the original project remains the reference, and problems found in this fork should not be reported to upstream.

<h1 align="center">JellyProxy-NG</h1>

<h3 align="center">Many servers. Single experience.</h3>

<p align="center">
<img alt="Logo Banner" src="./media/banner.svg"/>
<br/>
<br/>
<a href="#license">
<img alt="License: GPL v2 (repository), MIT or Apache-2.0 (Rust crates)" src="https://img.shields.io/badge/License-GPL_v2-blue.svg"/>
</a>
<a href="https://github.com/kibit86/jellyproxy-ng/releases">
<img alt="Current Release" src="https://img.shields.io/github/release/kibit86/jellyproxy-ng.svg"/>
</a>
</p>

JellyProxy-NG is a reverse proxy that lets you combine multiple Jellyfin servers into one place. If you’ve got libraries spread across different locations or just want everything together, JellyProxy-NG makes it easy to access all your media from a single interface.

---

<p align="center">
  <!-- Full-width library view -->
  <img src="./media/library.png" alt="Library" width="90%">
</p>

<p align="center">
  <!-- Side-by-side smaller views, same height -->
  <img src="./media/my_servers.png" alt="My servers: connect an account on each server, open or administer it" height="250px" style="margin-right:10px;">
  <img src="./media/servers.png" alt="Server management" height="250px" style="margin-right:10px;">
  <img src="./media/users.png" alt="Users and their server mappings" height="250px">
</p>

## Features

> [!WARNING]
> JellyProxy-NG is still in **development**. If you run into issues, please report them on the [GitHub Issues page of this fork](https://github.com/kibit86/jellyproxy-ng/issues).

### ✅ Working

* **Unified Library Access** – Browse media from multiple Jellyfin servers in one place.
* **Always Proxied Playback** – Media is always relayed through JellyProxy-NG, so clients never need to reach your Jellyfin servers (a per-server "redirect" mode existed up to 2.0.0 and was removed). Transcoding always happens on the server that owns the media.
* **User Mapping** – Link accounts across servers for a consistent user experience.
* **API Compatibility** – Appears as a normal Jellyfin server, so existing apps and tools still work.
* **My servers, Media and Profile** – Each user connects their own account on every server (password or Quick Connect), browses their media and changes their password.
* **QuickConnect** – Sign in on one device by approving the code from another authenticated device.
* **Websocket Support** – Needed for real-time features like SyncPlay 
* **One Account, Mapped Per Server** *(this fork)* – A single proxy administrator; for each server you choose the account to use there. No accounts are created from backend logins.
* **Server Panels** *(this fork)* – Open a server's own web client and dashboard already signed in with the mapped account, or run the first-time setup of a new server, through the proxy ([details](docs/administration.md)).
* **Per-Server Item Management** *(this fork)* – Edit, refresh and delete items only where the mapped account is an administrator and Jellyfin allows it. Native apps that read the "can delete" flag (for example Android TV) offer **Delete** too (since 1.3.0); see [docs/administration.md](docs/administration.md).
* **Network Discovery** *(this fork)* – Jellyfin apps on the LAN find the proxy automatically (UDP 7359); can be switched off.


### 🚫 Not in this fork
* **Server Federation** – Upstream can create a user on every connected server and sync accounts. Here it is switched off and hidden: accounts are mapped explicitly, and nothing is created on a backend. (`auto_create_users_on_login` is off by default.)

### ⚠️ In Progress
* **Automatic Bitrate Adjustment** – Stream quality based on network conditions isn’t supported yet.
* **Media Management** – Adding or deleting *libraries* through JellyProxy-NG is not implemented (item editing and deletion are, in this fork, where you are an administrator).

---

## Deployment

The easiest way to run JellyProxy-NG is with the prebuilt [Docker images](https://github.com/kibit86?tab=packages&repo_name=jellyproxy-ng).
Here’s a minimal `docker-compose.yml` example to get started:

```yaml
services:
  jellyproxy-ng:
    image: ghcr.io/kibit86/jellyproxy-ng:1.3.0   # pin an exact version, see "Image tags" below
    container_name: jellyproxy-ng
    restart: unless-stopped
    ports:
      - 3000:3000
    volumes:
      - ./data:/app/data
    environment:
      - JELLYSWARRM_USERNAME=admin
      - JELLYSWARRM_PASSWORD=change-me # ⚠️ Change this in production!
```

> The program and its settings keep the upstream names on purpose (`JELLYSWARRM_*`, `jellyswarrm.toml`, `jellyswarrm.db`).
> Only variables whose name is a single word work as environment variables; everything else is set in
> `jellyswarrm.toml` or in the **Settings** page. See [docs/config.md](docs/config.md).

**LAN discovery.** Jellyfin apps find servers with a UDP broadcast on port 7359. Docker's default bridge network does
not forward broadcasts, so for discovery run the container with `network_mode: host` (then `ports:` is not used;
set the port with `JELLYSWARRM_PORT`, default 3000). Only one server per host may own UDP 7359.

### First start

1. Open **`http://[JELLYSWARRM_HOST]:[JELLYSWARRM_PORT]/ui`** and sign in with `JELLYSWARRM_USERNAME` /
   `JELLYSWARRM_PASSWORD`. These create the **one proxy administrator** the first time the program starts. After that the
   password lives in the database: change it from **Profile**; changing the environment variable does nothing.
2. **Servers** → add your Jellyfin servers.
3. **My servers** → **Connect** on each server with the account you want to use *there* (password or Quick Connect;
   the names and passwords may differ). **Open**, **Set up** (a server never configured) and **Administer**
   open the server's own page through the proxy, in a new tab, already signed in.
4. Point your Jellyfin apps at the proxy and sign in with the proxy account.
   The bundled web client is at `http://[JELLYSWARRM_HOST]:[JELLYSWARRM_PORT]`.

Other users are optional (**Users**): each gets their own mapped accounts, see [docs/administration.md](docs/administration.md).

### Image tags

| Tag | Meaning |
|---|---|
| `X.Y.Z` | One release. **Use this**, and update it deliberately. |
| `X.Y`, `X` | Latest release of that minor / major. |
| `latest` | Latest release. |
| `edge`, `sha-…` | Snapshots of `main`. Not for deployment. |

A database migration cannot be undone: an image that has migrated a database cannot be rolled back without restoring
the data first. For advanced configuration options, check out the [ui](./docs/ui.md) and
[configuration](./docs/config.md) documentation.

For `/health`, `/ready`, and Docker health check details, see the [health monitoring documentation](./docs/health-monitoring.md).

## Contributing and releases

Everything reaches `main` through a pull request whose checks (format, clippy, tests, advisories, a Docker integration
test and a browser test) pass; merging builds the image, and raising the version in `Cargo.toml` makes it a release
(tag, GitHub Release and the `X.Y.Z` images are created by the pipeline). How it works, how the tests are organised and
what to review before a push is in [`CLAUDE.md`](CLAUDE.md) and [docs/releasing.md](docs/releasing.md).

---


## Local Development
### Getting Started
To get started with development, you'll need to clone the repository along with its submodules. This ensures you have all the necessary components for a complete build:

```bash
git clone --recurse-submodules https://github.com/kibit86/jellyproxy-ng.git
```

If you've already cloned the repository, you can initialize the submodules separately:

```bash
git submodule init
git submodule update
```


<details open>
<summary><strong>Docker</strong></summary>

The quickest way to get JellyProxy-NG up and running is with Docker. Simply use the provided [docker-compose](./docker-compose.yml) configuration:

```bash
docker compose up -d
```

This will build and start the application with all necessary dependencies, perfect for both development and production deployments.

</details>

<details>
<summary><strong>Native Build</strong></summary>

For a native build with the embedded web client, install Rust (the version in
`rust-toolchain.toml`), Node.js 24, and [just](https://just.systems/). Initialize
the UI submodule as shown above, then run:

```bash
just ui
cargo build --locked --release
```

`just ui` runs `npm ci`, builds the production web client, and replaces the
embedded assets in `crates/jellyswarrm-proxy/static/`, including UI version and
commit metadata. Run it again after updating the UI submodule or editing the UI.
Cargo only builds Rust; it never invokes npm or git.

For Rust-only development, `cargo check`, `cargo clippy`, and `cargo test` work
without Node.js or a UI build. A missing UI gets a development placeholder.
Release builds reject this placeholder: build the real UI with `just ui` first.
For deliberately UI-less release checks only, set
`JELLYSWARRM_ALLOW_PLACEHOLDER_UI=1` (not for deployment).

To run the dependency policy locally, install `cargo-deny` and run `just audit`.
Security advisories and unapproved licenses fail the check. Duplicate versions
and unmaintained transitive dependencies are reported as warnings.

### Local Test Servers

To test JellyProxy-NG against six preconfigured Jellyfin instances (two each for
Movies, TV Shows, and Music) and Seerr, run:

```bash
just setup
```

See the [development environment guide](dev/README.md) for URLs, credentials,
commands, Seerr compatibility status, and media licenses. Debug builds
automatically register all six local servers from `data/jellyswarrm.dev.toml`.

### HTTP Contract Tests

Run the web-client API contracts without Docker, a browser, or a UI build:

```bash
just http-test          # Both scenarios, focused regressions and route inventory checks
just http-test-normal   # Separate libraries on connected servers
just http-test-merged   # Merged libraries with deduplicated media
```

The suite follows movies, episodes and audio from library discovery through
media details, playback negotiation, and direct/HLS delivery through the proxy. See the [HTTP test guide](crates/jellyswarrm-proxy/src/http_tests/README.md)
for coverage and fixture details. These mock-backed tests are separate from
`just integration-test` and the browser integration commands, which use real
Jellyfin servers and media.

</details>

## FAQ  

1. **Why not just add multiple servers directly in the Jellyfin app?**  
   Some Jellyfin apps do support multiple servers, but switching between them can be inconvenient. JellyProxy-NG brings everything together in one place and also merges features like *Next Up* and *Recently Added* across all servers. This way, you can easily see what’s new in your own libraries or what your friends have added.  

2. **Will JellyProxy-NG work with my existing Jellyfin apps?**  
   Most likely! JellyProxy-NG presents itself as a standard Jellyfin server, so most clients should work out of the box. That said, not every Jellyfin client has been tested, so a few may have issues.  

3. **Why use JellyProxy-NG instead of mounting a remote library via e.g. SMB?**  
   JellyProxy-NG is built to **connect your servers with your friends’ servers** across different networks. Setting up SMB in these cases can be complicated, and performance is often worse. With JellyProxy-NG, the heavy lifting (like transcoding) happens where the media actually lives; in `Redirect` mode the client even streams straight from that server, in `Proxy` mode the media passes through JellyProxy-NG.

## License

The repository is licensed under the GNU GPL v2 (see [LICENSE](LICENSE)). The Rust crates under `crates/` are
`MIT OR Apache-2.0` (see the `LICENSE-MIT` / `LICENSE-APACHE` files in each crate). The bundled web client is
[jellyfin-web](https://github.com/jellyfin/jellyfin-web) (GPL v2) with the patch in `patches/`.
