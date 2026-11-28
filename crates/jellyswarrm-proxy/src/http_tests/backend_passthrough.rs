//! `/<ui>/backend/{server_id}/...`: one backend served unchanged, only to users
//! who are administrators on that backend.
use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};
use wiremock::{
    matchers::{body_string, header_exists, method, path},
    Mock, ResponseTemplate,
};

fn plain_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// Sign in to the management UI; returns the session cookie (`name=value`).
async fn ui_login(f: &Fixture, username: &str, password: &str) -> String {
    let response = plain_client()
        .post(format!("{}/ui/login", f.url))
        .form(&[("username", username), ("password", password)])
        .send()
        .await
        .unwrap();
    assert!(response.status().is_redirection(), "{}", response.status());
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|value| value.split(';').next().unwrap().to_owned())
        .collect::<Vec<_>>()
        .join("; ")
}

async fn server_id(f: &Fixture, index: usize) -> i64 {
    f.state
        .server_storage
        .list_servers()
        .await
        .unwrap()
        .into_iter()
        .find(|server| server.url.as_str().starts_with(&f.upstreams[index].uri()))
        .unwrap()
        .id
        .as_i64()
}

/// The backend accepts the mapped account `alice` and says whether it administers it.
async fn backend_login(f: &Fixture, index: usize, administrator: bool) {
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "User": {"Id": format!("backend-user-{index}"), "Name": "alice", "ServerId": format!("backend-{index}"),
                     "Policy": {"IsAdministrator": administrator, "SyncPlayAccess": "None"}},
            "AccessToken": format!("panel-token-{index}"), "ServerId": format!("backend-{index}"),
            "SessionInfo": {"UserId": format!("backend-user-{index}"), "UserName": "alice", "ServerId": format!("backend-{index}")}
        })))
        .mount(&f.upstreams[index])
        .await;
}

fn backend(f: &Fixture, id: i64, rest: &str) -> String {
    format!("{}/ui/backend/{id}/{rest}", f.url)
}

#[tokio::test]
async fn signed_out_visitors_never_reach_a_backend() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    let id = server_id(&f, 1).await;
    let response = plain_client()
        .get(backend(&f, id, "web/"))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_redirection() || response.status() == StatusCode::UNAUTHORIZED,
        "{}",
        response.status()
    );
    f.no_upstream_requests().await;
}

#[tokio::test]
async fn only_administrators_of_that_backend_get_in() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    backend_login(&f, 0, false).await;
    backend_login(&f, 1, true).await;
    for index in 0..2 {
        Mock::given(method("GET"))
            .and(path("/web/index.html"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!("backend {index}")))
            .mount(&f.upstreams[index])
            .await;
    }
    let cookie = ui_login(&f, "alice", "password").await;
    let (not_admin, admin) = (server_id(&f, 0).await, server_id(&f, 1).await);

    let refused = plain_client()
        .get(backend(&f, not_admin, "web/index.html"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert!(f.requests_without_admin_lookup(0).await.is_empty());

    let allowed = plain_client()
        .get(backend(&f, admin, "web/index.html"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(allowed.text().await.unwrap(), "backend 1");

    let unknown = plain_client()
        .get(backend(&f, 9999, "web/index.html"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    // Someone with no linked account on any backend is refused too.
    let other_cookie = ui_login(&f, "bob", "password").await;
    let stranger = plain_client()
        .get(backend(&f, admin, "web/index.html"))
        .header("cookie", &other_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(stranger.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn requests_and_responses_pass_unchanged_except_cookies_and_redirects() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    backend_login(&f, 1, true).await;
    let backend_mock = &f.upstreams[1];
    Mock::given(method("POST"))
        .and(path("/Items/some-item"))
        .and(body_string(r#"{"Name":"New"}"#))
        .and(header_exists("authorization"))
        .respond_with(
            ResponseTemplate::new(204)
                .insert_header("set-cookie", "backend=1; Path=/")
                .insert_header("x-backend", "yes"),
        )
        .expect(1)
        .mount(backend_mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/old"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/web/"))
        .mount(backend_mock)
        .await;
    Mock::given(method("GET"))
        .and(path("/System/Info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"Id": "real-server-id"})))
        .mount(backend_mock)
        .await;

    let cookie = ui_login(&f, "alice", "password").await;
    let id = server_id(&f, 1).await;

    let response = plain_client()
        .post(backend(&f, id, "Items/some-item"))
        .header("cookie", &cookie)
        .header("authorization", r#"MediaBrowser Token="backend-session""#)
        .header("content-type", "application/json")
        .body(r#"{"Name":"New"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()["x-backend"], "yes");
    assert!(
        response.headers().get("set-cookie").is_none(),
        "the backend's cookies must not reach the browser"
    );
    let seen = f.requests_without_admin_lookup(1).await;
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].headers.get("cookie").is_none(),
        "the proxy's session cookie must not reach the backend"
    );
    assert_eq!(
        seen[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        r#"MediaBrowser Token="backend-session""#
    );

    // Redirects stay inside the mount.
    let redirect = plain_client()
        .get(backend(&f, id, "old"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(redirect.status(), StatusCode::FOUND);
    assert_eq!(
        redirect.headers()["location"],
        format!("/proxy/ui/backend/{id}/web/")
    );

    // The bare address is a shortcut to the backend's web client.
    let bare = plain_client()
        .get(backend(&f, id, ""))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(bare.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        bare.headers()["location"],
        format!("/proxy/ui/backend/{id}/web/")
    );

    // Not translated: the backend's own identity, not the proxy's.
    let info: Value = plain_client()
        .get(backend(&f, id, "System/Info"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["Id"], "real-server-id");
}

/// A backend that answers the panel's HTTP sign-in and speaks WebSocket (echo, with a
/// greeting) on one port.
async fn http_and_websocket_backend() -> std::net::SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut head = [0u8; 4];
                stream.peek(&mut head).await.unwrap();
                if &head == b"POST" {
                    // Read the whole sign-in request, then answer it.
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    loop {
                        let n = stream.read(&mut chunk).await.unwrap();
                        request.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&request).to_lowercase();
                        if let Some(end) = text.find("\r\n\r\n") {
                            let length = text
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:"))
                                .and_then(|v| v.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            if request.len() >= end + 4 + length || n == 0 {
                                break;
                            }
                        }
                    }
                    let body = json!({
                        "User": {"Id": "ws-user", "Name": "alice", "ServerId": "ws-backend",
                                 "Policy": {"IsAdministrator": true, "SyncPlayAccess": "None"}},
                        "AccessToken": "ws-token", "ServerId": "ws-backend",
                        "SessionInfo": {"UserId": "ws-user", "UserName": "alice", "ServerId": "ws-backend"}
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                    return;
                }
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                socket
                    .send(Message::text("hello from backend"))
                    .await
                    .unwrap();
                while let Some(Ok(message)) = socket.next().await {
                    if message.is_text() {
                        socket.send(message).await.unwrap();
                    } else if message.is_close() {
                        break;
                    }
                }
            });
        }
    });
    addr
}

#[tokio::test]
async fn websocket_is_relayed_both_ways() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    let backend_addr = http_and_websocket_backend().await;

    let server = {
        let id = f
            .state
            .server_storage
            .add_server("WebSocket backend", &format!("http://{backend_addr}"), 100)
            .await
            .unwrap();
        f.state
            .server_storage
            .get_server_by_id(id)
            .await
            .unwrap()
            .unwrap()
    };
    f.state
        .user_authorization
        .add_server_mapping(&f.user.id, &server, "alice", &"password".into(), None)
        .await
        .unwrap();

    let cookie = ui_login(&f, "alice", "password").await;
    let url = format!(
        "ws://{}/ui/backend/{}/socket?api_key=x",
        f.url.trim_start_matches("http://"),
        server.id
    );
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("cookie", cookie.parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();

    let greeting = socket.next().await.unwrap().unwrap();
    assert_eq!(greeting.into_text().unwrap().as_str(), "hello from backend");
    socket
        .send(Message::text("ping from client"))
        .await
        .unwrap();
    let echoed = socket.next().await.unwrap().unwrap();
    assert_eq!(echoed.into_text().unwrap().as_str(), "ping from client");
}

#[tokio::test]
async fn websocket_without_a_session_is_refused() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    let id = server_id(&f, 1).await;
    let url = format!(
        "ws://{}/ui/backend/{id}/socket",
        f.url.trim_start_matches("http://")
    );
    assert!(tokio_tungstenite::connect_async(url).await.is_err());
}

const START_PAGE: &str =
    "<!doctype html><html><head><title>Jellyfin</title></head><body>app</body></html>";

async fn mount_start_page(f: &Fixture, index: usize) {
    Mock::given(method("GET"))
        .and(path("/web/"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"upstream-etag\"")
                .insert_header("cache-control", "max-age=3600")
                .set_body_raw(START_PAGE, "text/html"),
        )
        .mount(&f.upstreams[index])
        .await;
    Mock::given(method("GET"))
        .and(path("/web/main.bundle.js"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("console.log(1)", "text/javascript"))
        .mount(&f.upstreams[index])
        .await;
}

#[tokio::test]
async fn the_start_page_gets_its_own_storage_and_the_signed_in_session() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    backend_login(&f, 1, true).await;
    mount_start_page(&f, 1).await;
    let cookie = ui_login(&f, "alice", "password").await;
    let id = server_id(&f, 1).await;

    let page = plain_client()
        .get(backend(&f, id, "web/"))
        .header("cookie", &cookie)
        // What a browser sends: the page must still come back editable and fresh.
        .header("accept-encoding", "gzip, br")
        .header("if-none-match", "\"upstream-etag\"")
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()["cache-control"], "no-store");
    assert!(
        page.headers().get("etag").is_none(),
        "the page depends on who asks"
    );
    let declared_length: usize = page.headers()["content-length"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let html = page.text().await.unwrap();
    assert_eq!(declared_length, html.len());
    assert!(
        html.contains("<head><script>"),
        "the script comes first in <head>: {html}"
    );
    assert!(
        html.contains(&format!("jsw-backend-{id}:")),
        "storage is scoped per backend"
    );
    assert!(
        html.contains("panel-token-1"),
        "the session of the mapped account is handed over"
    );
    assert!(
        html.contains("\"Id\":\"backend-1\""),
        "stored under the backend's own server ID"
    );
    assert!(html.contains("\"UserId\":\"backend-user-1\""));
    assert!(html.contains("<title>Jellyfin</title>") && html.contains("<body>app</body>"));
    assert!(
        !html.contains("alicepw") && !html.contains("\"password\""),
        "no password reaches the browser"
    );

    let upstream_page: Vec<_> = f
        .requests_without_admin_lookup(1)
        .await
        .into_iter()
        .filter(|request| request.url.path() == "/web/")
        .collect();
    assert_eq!(upstream_page.len(), 1);
    // The browser's own negotiation is not forwarded: the proxy's HTTP client asks for
    // (and transparently decodes) its own encoding, so the page arrives editable.
    assert_ne!(
        upstream_page[0]
            .headers
            .get("accept-encoding")
            .map(|v| v.to_str().unwrap()),
        Some("gzip, br")
    );
    assert!(upstream_page[0].headers.get("if-none-match").is_none());

    // Everything else is passed through untouched.
    let asset = plain_client()
        .get(backend(&f, id, "web/main.bundle.js"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(asset.headers()["content-type"], "text/javascript");
    assert_eq!(asset.text().await.unwrap(), "console.log(1)");
}

#[tokio::test]
async fn one_sign_in_serves_a_whole_page_load() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    backend_login(&f, 1, true).await;
    mount_start_page(&f, 1).await;
    let cookie = ui_login(&f, "alice", "password").await;
    let id = server_id(&f, 1).await;

    let loads = (0..6).map(|i| {
        let rest = if i == 0 { "web/" } else { "web/main.bundle.js" };
        plain_client()
            .get(backend(&f, id, rest))
            .header("cookie", &cookie)
            .send()
    });
    for response in futures_util::future::join_all(loads).await {
        assert_eq!(response.unwrap().status(), StatusCode::OK);
    }
    let sign_ins = f.upstreams[1]
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/Users/AuthenticateByName")
        .count();
    assert_eq!(sign_ins, 1, "concurrent requests share one backend sign-in");
}

#[tokio::test]
async fn a_backend_that_refuses_the_mapped_account_is_not_opened() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&f.upstreams[1])
        .await;
    mount_start_page(&f, 1).await;
    let cookie = ui_login(&f, "alice", "password").await;
    let id = server_id(&f, 1).await;

    let response = plain_client()
        .get(backend(&f, id, "web/"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(f.requests_without_admin_lookup(1).await.is_empty());
}

// ---- Servers with no mapped account: the proxy administrator signs in on the server ----
// (this is also how a brand-new server gets its first account, through its wizard)

/// A proxy administrator with no account on any server; returns their UI cookie.
async fn administrator_without_accounts(f: &Fixture) -> String {
    let admin = f
        .state
        .user_authorization
        .create_user("root", &"password".into())
        .await
        .unwrap();
    sqlx::query("UPDATE users SET role = 'admin' WHERE id = ?")
        .bind(&admin.id)
        .execute(&f.pool)
        .await
        .unwrap();
    ui_login(f, "root", "password").await
}

async fn wizard_state(f: &Fixture, index: usize, completed: bool) {
    Mock::given(method("GET"))
        .and(path("/System/Info/Public"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ServerName": "fresh", "Id": "fresh-id", "StartupWizardCompleted": completed
        })))
        .mount(&f.upstreams[index])
        .await;
}

async fn servers_page(f: &Fixture, cookie: String) -> String {
    plain_client()
        .get(format!("{}/ui/user/servers", f.url))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_server_never_set_up_opens_for_the_proxy_administrator_without_an_account() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    wizard_state(&f, 1, false).await;
    mount_start_page(&f, 1).await;
    Mock::given(method("POST"))
        .and(path("/Startup/User"))
        .and(body_string(r#"{"Name":"first","Password":"secret"}"#))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&f.upstreams[1])
        .await;
    let cookie = administrator_without_accounts(&f).await;
    let id = server_id(&f, 1).await;

    let page = plain_client()
        .get(backend(&f, id, "web/"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let html = page.text().await.unwrap();
    assert!(
        html.contains(&format!("jsw-backend-{id}:")),
        "storage is still scoped"
    );
    assert!(
        html.contains("SEED = null"),
        "no session is handed over: there is none yet"
    );
    assert!(!html.contains("AccessToken"));

    // The wizard's own calls go through, unchanged.
    let created = plain_client()
        .post(backend(&f, id, "Startup/User"))
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .body(r#"{"Name":"first","Password":"secret"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::NO_CONTENT);

    let sign_ins = f.upstreams[1]
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/Users/AuthenticateByName")
        .count();
    assert_eq!(sign_ins, 0, "nothing signs in: there is no account yet");
}

#[tokio::test]
async fn any_server_with_no_mapped_account_opens_for_the_proxy_administrator_to_sign_in_there() {
    // After the wizard (or on a server set up long ago) the page must keep working: its
    // own login page is how the account just created is used for the first time.
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    wizard_state(&f, 1, true).await;
    mount_start_page(&f, 1).await;
    let cookie = administrator_without_accounts(&f).await;
    let id = server_id(&f, 1).await;

    let response = plain_client()
        .get(backend(&f, id, "web/"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.text().await.unwrap().contains("SEED = null"));
}

#[tokio::test]
async fn an_ordinary_user_with_no_mapped_account_is_never_let_in() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    wizard_state(&f, 1, false).await;
    mount_start_page(&f, 1).await;
    // bob has no account on any server, and is not the proxy administrator.
    let cookie = ui_login(&f, "bob", "password").await;
    let id = server_id(&f, 1).await;

    let response = plain_client()
        .get(backend(&f, id, "web/"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_proxy_administrator_is_still_bound_by_the_account_they_mapped() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    backend_login(&f, 0, false).await; // mapped, but not an administrator there
    backend_login(&f, 1, true).await;
    mount_start_page(&f, 0).await;
    mount_start_page(&f, 1).await;
    // alice has an account mapped on both servers; make her the proxy administrator.
    sqlx::query("UPDATE users SET role = 'admin' WHERE id = ?")
        .bind(&f.user.id)
        .execute(&f.pool)
        .await
        .unwrap();
    let cookie = ui_login(&f, "alice", "password").await;
    let (refused, allowed) = (server_id(&f, 0).await, server_id(&f, 1).await);

    let no = plain_client()
        .get(backend(&f, refused, "web/"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(
        no.status(),
        StatusCode::FORBIDDEN,
        "a mapped non-admin account is not bypassed"
    );
    let yes = plain_client()
        .get(backend(&f, allowed, "web/"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(yes.status(), StatusCode::OK);
    assert!(
        yes.text().await.unwrap().contains("panel-token-1"),
        "signed in with the mapped account"
    );
}

#[tokio::test]
async fn the_administrator_is_offered_set_up_for_a_new_server_and_open_for_the_others() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    wizard_state(&f, 0, true).await; // already set up
    wizard_state(&f, 1, false).await; // brand new
    let admin = administrator_without_accounts(&f).await;
    let other = ui_login(&f, "bob", "password").await;
    let (new_server, old_server) = (server_id(&f, 1).await, server_id(&f, 0).await);

    let for_admin = servers_page(&f, admin).await;
    let set_up = for_admin
        .find(&format!("/ui/backend/{new_server}/web/"))
        .expect("link to the new server");
    let open = for_admin
        .find(&format!("/ui/backend/{old_server}/web/"))
        .expect("link to the configured server");
    assert!(for_admin[set_up..].contains("Set up"));
    assert!(for_admin[open..].contains("Open"));
    assert_eq!(
        for_admin.matches("Set up").count(),
        1,
        "only the new server says Set up"
    );

    let for_user = servers_page(&f, other).await;
    assert!(
        !for_user.contains("Set up") && !for_user.contains("/web/"),
        "an ordinary user is offered neither"
    );
}

#[tokio::test]
async fn completing_the_wizard_turns_set_up_into_open_at_once() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    // The first probe finds the wizard open; every later one finds it completed.
    Mock::given(method("GET"))
        .and(path("/System/Info/Public"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"StartupWizardCompleted": false})),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&f.upstreams[1])
        .await;
    wizard_state(&f, 1, true).await;
    Mock::given(method("POST"))
        .and(path("/Startup/Complete"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&f.upstreams[1])
        .await;
    wizard_state(&f, 0, true).await;
    let cookie = administrator_without_accounts(&f).await;
    let id = server_id(&f, 1).await;

    assert!(
        servers_page(&f, cookie.clone()).await.contains("Set up"),
        "the wizard is open"
    );
    let finished = plain_client()
        .post(backend(&f, id, "Startup/Complete"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(finished.status(), StatusCode::NO_CONTENT);
    // Not "within ten seconds": immediately, and the page keeps working for the next step.
    assert!(!servers_page(&f, cookie.clone()).await.contains("Set up"));
    mount_start_page(&f, 1).await;
    let page = plain_client()
        .get(backend(&f, id, "web/"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
}
