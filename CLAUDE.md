# CLAUDE.md — working on this repository

This is **JellyProxy-NG**, a **public, AI-assisted fork** of [LLukas22/Jellyswarrm](https://github.com/LLukas22/Jellyswarrm)
(a proxy that presents several Jellyfin servers as one). It is made for one homelab and published as
`ghcr.io/kibit86/jellyproxy-ng`. Read the [README](README.md) first; this file is the operating manual for
anyone — human or agent — who changes the code or ships a release.

- Default branch: **`main`**. Remotes: `fork` = `kibit86/jellyproxy-ng` (push here; the repository was renamed from `kibit86/Jellyswarrm`, GitHub redirects the old URL), `origin` = upstream (read only).
- Rust toolchain is pinned in `rust-toolchain.toml`. The web client is the `ui` submodule (jellyfin-web) plus
  `patches/jellyfin-web/` (see "Fork features").

## Naming: product name vs internal names

The product is **JellyProxy-NG** (repository `kibit86/jellyproxy-ng`, image `ghcr.io/kibit86/jellyproxy-ng`; up to
1.1.0 it was published as `ghcr.io/kibit86/jellyswarrm`, which is no longer updated). Use that name in anything a
person reads: UI text, docs, log and error messages, the name shown to Jellyfin servers.

**Do not rename the internal names**, on purpose: the crates (`jellyswarrm-proxy`, `jellyswarrm-macros`), Rust
module paths, the binary, the `JELLYSWARRM_*` environment variables, `jellyswarrm.toml` / `jellyswarrm.db`, the
device ids (`jellyswarrm-panel-…`), the `JellyswarrmCanManage` field read by the patched web client, and HTML ids.
Existing installations depend on them (configuration, data directory, secrets) and every rename would conflict
with each merge from upstream. Never edit an existing migration file (not even a comment): sqlx checksums them and a changed one stops
every existing database from starting. If a rename of these is ever wanted, it is its own breaking release with a
migration, not a side effect of a text change.

## Golden rules

1. **Everything reaches `main` through a pull request.** Never push to `main` directly: a push to `main` is
   a publish (see below), and it skips the PR checks.
2. **Never create or push a tag by hand.** Tags `vX.Y.Z` are created by the release workflow, after the
   images exist. There is no workflow on tags, so a hand-made tag publishes nothing and only confuses things.
3. **The version lives in one place:** `version` under `[workspace.package]` in the root `Cargo.toml` (plus the
   three matching entries in `Cargo.lock`). Raising it in a merged PR *is* the release.
4. **Never put `[skip ci]` in a commit that is meant to be merged**: it would skip the publish run.
5. **Published versions are immutable.** If a release is bad, do not reuse its number: fix and release the next
   patch (`X.Y.Z+1`). You may delete the broken GitHub Release/tag afterwards.
6. Database migrations (`crates/jellyswarrm-proxy/migrations`) are **one-way in practice**: an image that has
   migrated a database cannot be rolled back to an older image without restoring the data. Treat a migration
   change as a decision for the maintainer, not a detail.
7. The GHCR package visibility (public) is **not** controlled by workflows or branches; it can only be changed
   in the GitHub UI. Do not assume you can make an image private/public from here.

## How code reaches production

| Event | What runs |
|---|---|
| Commit on a branch | Nothing. |
| Pull request to `main` | `ci.yml`: the checks below. **No image is built.** |
| Push (merge) to `main` | `release.yml`: **no tests** (they ran on the PR); builds and publishes the image as `edge` and `sha-<commit>`. |
| Push to `main` with a version in `Cargo.toml` that has **no tag yet** | The same, plus a **release**: the image is also published as `X.Y.Z`, `X.Y`, `X`, `latest`; then the tag `vX.Y.Z` and the GitHub Release are created on that commit (last step, so a tag only exists if all images do). |

Two workflows with one job each way: `.github/workflows/ci.yml` (PR only: `checks`, `dependencies`, `integration`) and
`.github/workflows/release.yml` (push to `main` only: `version`, `build-and-push`, `create-manifest`). Because the merge
does not re-run the tests, **`main` is protected**: only a PR with green, up-to-date checks `Checks (fmt, clippy, tests)`,
`Dependency advisories and licenses` and `Docker integration test` can be merged; never push to `main` to get around it
(a Dockerfile problem is found at merge time and publishes nothing). Safeguards in `release.yml` (job `version`): the version must be exactly `X.Y.Z`; a version **lower** than the
latest tag fails the run; if the tag already exists the run is a snapshot only (re-running is safe); one run per
branch at a time (`concurrency`), so two quick merges cannot race for the same tag. A failing CI publishes
nothing and creates no tag.

`edge` and `sha-…` are snapshots of `main` and are **not** meant to be deployed. Deployment follows only the
SemVer tags (`X.Y.Z`): the repository that deploys the image tracks them with Renovate.

### Shipping a change

1. Branch from `main` (`feat/…`, `fix/…`), commit, open a PR to `main`. Wait for CI to be green.
2. Merge. That publishes `edge` only, unless the PR also changed the version.
3. To release, raise the version in a PR (together with the change or on its own): edit `Cargo.toml` and the
   three entries of our crates in `Cargo.lock` (`jellyfin-api`, `jellyswarrm-macros`, `jellyswarrm-proxy`);
   check with `cargo metadata --locked`. Merge it. The run on `main` does the rest.
4. After the run: check the Actions run is green, the GitHub Release `vX.Y.Z` exists, and the tags
   `X.Y.Z`, `X.Y`, `X`, `latest` are pullable (anonymously, the package is public, and for both architectures:
   `docker manifest inspect ghcr.io/kibit86/jellyproxy-ng:X.Y.Z`).

Version numbers: patch for fixes, minor for features, major for breaking changes (including a database change
that cannot be undone). The fork follows its own numbering, independent of upstream's.

### If something goes wrong

- **Release run red before the images are pushed:** nothing was published, no tag exists. Fix on a branch and
  merge again (same version is fine).
- **Images pushed but the tag/Release step failed:** re-run the failed `create-manifest` job from the Actions
  page (it is idempotent); do not tag by hand.
- **A bad release is out:** raise the patch version with the fix. Do not move or delete published tags that
  something deploys from.
- **The run fails in `version` with "lower than the latest tag":** the version in `Cargo.toml` is wrong
  (or was not raised); correct it.

## What CI checks (`ci.yml`)

Runs on every PR (`ci.yml`), not on the push to `main`. All of it must be green to merge.

| Job | Commands |
|---|---|
| `checks` | `python3 scripts/test-ui-build.py`; `cargo check --locked --workspace --all-targets --all-features`; `cargo test --locked --workspace`; UI build (`just ui`); `node scripts/scan-client-routes.cjs --check`; **`cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`**; **`cargo fmt --all -- --check`** |
| `dependencies` | `cargo-deny` advisories and licenses |
| `integration` | `just integration-test` (real Jellyfin servers in Docker Compose) and the containerized browser test |

Run before every push at least: `cargo fmt --all`, the clippy command above, and
`cargo test --locked -p jellyswarrm-proxy --lib` (the in-process HTTP contracts, no Docker needed).
Warnings are errors in CI (`-D warnings`): an unused import fails the build.

### Running things locally

- Unit and HTTP contract tests need nothing but Rust: `cargo test --locked -p jellyswarrm-proxy --lib`
  (or `just http-test`). The embedded web client is replaced by a placeholder when `ui/` is not built.
- If Rust is not installed, use a container, e.g. `docker run --rm -v "$PWD":/src -w /src rust:1.99.1-alpine …`
  with `apk add musl-dev` (and `rustup component add clippy rustfmt`). Mount a volume on `/src/target`
  to keep the build cache. Files written by a root container are owned by root: `chown` them back.
- **Races hide on a fast machine.** Work that runs in the background (a spawned task, a cache refresh) makes a test
  that counts requests depend on timing: it passes locally and fails on CI, which is slower. Keep such work **off by
  default in the test harness** (see `AdminRights::background_refresh`) and turn it on only in the test of the
  feature itself. To see a race before CI does, run the suite on one core:
  `docker run --cpus=1 …  cargo test --locked -p jellyswarrm-proxy --lib` (about 6 minutes).
- `just integration-test` needs Docker, Git LFS media (`just media`) and a built UI (`just ui`, Node). It takes
  about 5 minutes per test; when you cannot run it locally, **let the PR run it and say so**.
- `dev/e2e-fresh-install/` is a Docker + Playwright end-to-end check of the whole fresh-install flow
  (two Jellyfin servers, the proxy, Chrome); see its README. It is not part of CI.

## Tests are part of the change

A change is not done until its tests are. **Write or update the tests in the same PR, as you write the code**
(for a bug: first a test that fails, then the fix). CI is the final check, not the place where you find out
which tests you forgot: a PR that is fixed one red job at a time was not reviewed.

### The test layers

| Layer | Where | Runs | Needs |
|---|---|---|---|
| Unit and HTTP contract tests (mock backends, in-memory DB) | next to the code and `crates/jellyswarrm-proxy/src/http_tests/` | locally and in CI (`cargo test`) | Rust only |
| Real-server integration test (two Jellyfin stacks, proxy as a process) | `crates/jellyswarrm-proxy/tests/server_integration.rs` | CI (`just integration-test`) | Docker, LFS media, built UI |
| Browser tests (Chrome drives the proxy UI and the Jellyfin web client) | `crates/jellyswarrm-proxy/tests/browser/` | CI (`just browser-integration-test-docker`) | the above plus Chrome in containers |
| Fresh-install end-to-end (whole account model, signed-in panels) | `dev/e2e-fresh-install/` | by hand, not in CI | Docker + Playwright |

The last three are slow and are the ones that break unnoticed, because they live outside `src/`.

### Before every push: review the tests against what you changed

1. List what changed: `git diff --stat origin/main...` and look at **templates, config defaults, routes,
   auth/account behaviour, migrations**.
2. For each kind of change, look at the tests that touch it:

| You changed | Review (grep for it) |
|---|---|
| A UI template: ids, classes, `hx-*`, form fields, the default page of a role | `tests/browser/*.rs` selectors (`grep -n 'locator\|wait_for_function\|hx-' tests/browser`), `dev/e2e-fresh-install/browser_e2e.py` |
| A config default or a config key (`auto_create_users_on_login`, admin `username`/`password`, ports, discovery) | `write_proxy_config` in `tests/server_integration.rs`, `data/jellyswarrm.dev.toml`, `dev/README.md`, `docs/config.md` (only single-word keys can be set from the environment: say so, and keep the table's Environment column true) |
| Who can log in, admin bootstrap, mapped accounts, admin rights | `tests/server_integration.rs` (logins, `IsAdministrator`), `tests/browser/login.rs` and `server_mapping.rs`, `http_tests/access.rs`, `admin_rights.rs` |
| A route, a header, the pass-through | `http_tests/` (the matching file), `node scripts/scan-client-routes.cjs --check` |
| A migration | tests that open a database; remember rule 6 above |
| A name used by fixtures | The backends in the test stack have the users `test` and `admin`. The proxy's own administrator must **not** be named like a fixture user: the two collided once and the login failed with 401. |

3. Update the tests that the change makes wrong, and **add** the ones for new behaviour (a new button, a new
   role rule, a new setting). A removed UI element means removing the steps that clicked it.
4. Run what you can locally: `cargo fmt --all`, the clippy command above, `cargo test --locked -p jellyswarrm-proxy --lib`.
   Compile the external tests too (clippy with `--all-targets --all-features` does).
5. In the PR description say which layers you ran and which you left to CI (integration, browser). Never imply a
   layer passed that you did not run.
6. If CI still finds a failure, do not just fix that one line: ask which **class** of test you skipped in step 2,
   and review the whole class before pushing again.

The proxy-admin landing page ("My servers") and the **Administer / Set up / Open** buttons are covered by rendering
tests in `ui/user/servers.rs` (who sees which button), by `tests/browser/server_mapping.rs`
(`administrator_sees_their_servers_and_opens_the_signed_in_panels`, and the simple-user checks in the mapping test)
and by the by-hand end-to-end in `dev/e2e-fresh-install/`. The "Set up" button needs a server whose first-run wizard is
open, which the Docker fixtures never have: only the rendering test covers it.

## Where things are

- `crates/jellyswarrm-proxy/ARCHITECTURE.md` (module map and where a feature's code goes) and `DATA_FLOW.md`
  (how a request moves) — read these before changing structure.
- `crates/jellyswarrm-proxy/src/http_tests/README.md`: contract tests (mock backends with Wiremock, in-memory
  SQLite, no Docker). New behaviour needs a test here or next to the code.
- `docs/`: `administration.md` (how the fork's account model works for a user), `releasing.md` (this flow, short),
  `config.md`, `ui.md`, `health-monitoring.md`. Update docs in the same PR as the behaviour.

## Fork features (what differs from upstream v1.0.0)

Know these before touching the areas; each has tests under `src/http_tests/` or next to the code.

- **One proxy admin account, a mapped account per server.** `users.role` (`user`|`admin`, migration
  `20261115000000_add_user_role`); the admin is created at first start by `ensure_admin_user`
  (`auth/service/users.rs`, called from `startup/bootstrap.rs`) from `JELLYSWARRM_USERNAME` / `_PASSWORD`; after that
  the database owns the password. Management-UI login authenticates database users only (`ui/auth`). The admin
  cannot be deleted. `auto_create_users_on_login` is `false` by default; federation is hidden/off (code kept).
- **Device tokens, as in Jellyfin** (`client_devices`, migration `20261124000000`; `auth/service/client_devices.rs`,
  `application/session_recovery.rs`): a login creates/replaces the row of (user, device id) and returns **its own token**
  (the per-user `virtual_key` is no longer a credential); requests resolve user and device **from the token**
  (`get_caller_by_token`, `get_device_sessions`), never by comparing headers. Backend sessions belong to the device row
  and are logged in with a proxy-owned device id (`backend_device_id`). A 401 from a backend for a stored token is repaired
  once by `MediaServices::with_recovery` (single flight per session). Jellyfin facts this relies on (verified on 12.4.0):
  a new login of the same (user, DeviceId) revokes the old token; the token alone identifies the caller (any `DeviceId`
  header works); a `userId` of another user is 403; `POST /Sessions/Logout` revokes. Tests: `http_tests/session_recovery.rs`
  models a Jellyfin-like backend. In unit tests, tokens are deterministic: `auth::service::test_token(user_id, device_id)`.
- **Server panels / pass-through** (`ui/backend_passthrough.rs`, `ui/backend_identity.rs`): `/ui/backend/{id}/…`
  proxies a server's own web client, signed in with the mapped account (`PanelIdentities`), or in "manual" mode for
  a server with no mapped account (first-run setup wizard). The start page gets a script injected that isolates
  `localStorage` per backend and seeds `jellyfin_credentials`. Cookies never cross; hop-by-hop headers go through
  the shared helpers (`prepare_request_headers`, `remove_hop_by_hop_headers`).
- **Per-item admin rights** (`application/admin_rights.rs`, `patches/jellyfin-web/0001-per-item-management.patch`):
  admin status is tracked per backend token (cached 6 hours; when unknown, a lookup runs in the background and the
  response is never delayed); web clients are told `IsAdministrator=false`, item-management calls are routed to the
  servers where the mapped account is an admin; global admin endpoints on the merged API are 403. Native apps read
  `CanDelete`: `translation/response_processor.rs` forces it to `false` (inherited from upstream) except for playlists
  and, since 1.3.0, for items of a server where the viewer administers, where the backend's own value is kept.
  Jellyfin 12 reads only the `Authorization` header (not `X-Emby-Authorization`).
- **LAN discovery** (`discovery.rs`): answers Jellyfin's UDP 7359 broadcast; configurable (`autodiscovery`) and
  never fatal if the port is busy.

### Merging upstream

`git fetch origin`, merge `origin/main` into a branch (not into `main`), resolve in favour of keeping the fork
features above, run the full set of checks, open a PR. Upstream refactors of header handling and the auth flow
are the usual conflict areas.

## Conventions

- Keep PRs focused; describe what was and was **not** tested (for example "integration test left to CI"), see
  "Tests are part of the change".
- Commits end with the usual attribution trailers when an AI assistant wrote them.
- Do not commit secrets, local data (`/data`, `dev/data`) or build output (`target`, `ui/dist`).
