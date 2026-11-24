//! A backend that revokes a stored token (Jellyfin does it whenever the same device logs in again,
//! and an administrator can do it too) must not leave its server missing: the proxy logs the
//! device in again, once, and retries. The backend here behaves like Jellyfin: one token per
//! device id, a new login with the same device id revokes the previous token, an unknown token is
//! a 401.
use super::*;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
use wiremock::{
    matchers::{method, path},
    Mock, Request, Respond, ResponseTemplate,
};

#[derive(Default)]
struct JellyfinState {
    /// device id → its current token
    devices: HashMap<String, String>,
    valid: HashSet<String>,
    logins: usize,
    next: usize,
    reject_logins: bool,
    /// device ids seen at login, in order
    login_devices: Vec<String>,
}

#[derive(Clone, Default)]
struct FakeJellyfin(Arc<Mutex<JellyfinState>>);

fn header_param(request: &Request, name: &str) -> Option<String> {
    let value = request
        .headers
        .get("authorization")?
        .to_str()
        .ok()?
        .to_owned();
    let key = format!("{name}=\"");
    let start = value.find(&key)? + key.len();
    let rest = &value[start..];
    Some(rest[..rest.find('"')?].to_owned())
}

struct Login(FakeJellyfin);
impl Respond for Login {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut state = (self.0).0.lock().unwrap();
        if state.reject_logins {
            return ResponseTemplate::new(401);
        }
        let device = header_param(request, "DeviceId").unwrap_or_default();
        state.login_devices.push(device.clone());
        state.logins += 1;
        state.next += 1;
        let token = format!("fake-token-{}", state.next);
        // Same device id, same user: the previous token is revoked.
        if let Some(old) = state.devices.insert(device, token.clone()) {
            state.valid.remove(&old);
        }
        state.valid.insert(token.clone());
        ResponseTemplate::new(200).set_body_json(json!({
            "User":{"Id":"backend-user-0","Name":"alice","ServerId":"backend",
                "Policy":{"IsAdministrator":false,"SyncPlayAccess":"None"}},
            "SessionInfo":{"UserId":"backend-user-0","UserName":"alice","ServerId":"backend"},
            "AccessToken":token,"ServerId":"backend"
        }))
    }
}

struct Item(FakeJellyfin, String);
impl Respond for Item {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let state = (self.0).0.lock().unwrap();
        match header_param(request, "Token") {
            Some(token) if state.valid.contains(&token) => ResponseTemplate::new(200)
                .set_body_json(json!({"Id":self.1,"Name":"Movie","Type":"Movie"})),
            _ => ResponseTemplate::new(401).set_body_string("Invalid token."),
        }
    }
}

async fn setup() -> (Fixture, FakeJellyfin, String) {
    let f = Fixture::new().await;
    f.healthy().await;
    let backend = FakeJellyfin::default();
    let original = f.original(0).await;
    Mock::given(method("POST"))
        .and(path("/Users/AuthenticateByName"))
        .respond_with(Login(backend.clone()))
        .mount(&f.upstreams[0])
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/Users/backend-user-0/Items/{original}")))
        .respond_with(Item(backend.clone(), original.clone()))
        .mount(&f.upstreams[0])
        .await;
    let url = format!("/Users/{}/Items/{}", f.user.id, f.items[0]);
    (f, backend, url)
}

async fn stored_token(f: &Fixture) -> String {
    f.state
        .user_authorization
        .get_user_sessions(&f.user.id, None)
        .await
        .unwrap()
        .into_iter()
        .find(|(_, server)| server.name == "Server 0")
        .unwrap()
        .0
        .jellyfin_token
}

#[tokio::test]
async fn a_revoked_backend_token_is_replaced_by_one_login_and_the_request_succeeds() {
    let (f, backend, url) = setup().await;
    // The stored token ("backend-token-0") is unknown to the backend, as if it had been revoked.
    let response = f
        .request(reqwest::Method::GET, &url, &f.user, "tv")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(backend.0.lock().unwrap().logins, 1, "exactly one re-login");
    let token = stored_token(&f).await;
    assert!(
        token.starts_with("fake-token-"),
        "the new token is stored: {token}"
    );

    // It is reused: no further login.
    let response = f
        .request(reqwest::Method::GET, &url, &f.user, "tv")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(backend.0.lock().unwrap().logins, 1);
}

#[tokio::test]
async fn parallel_requests_that_all_get_a_401_share_one_re_login() {
    let (f, backend, url) = setup().await;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let request = f.request(reqwest::Method::GET, &url, &f.user, "tv");
        tasks.push(tokio::spawn(async move {
            request.send().await.unwrap().status()
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), StatusCode::OK);
    }
    assert_eq!(
        backend.0.lock().unwrap().logins,
        1,
        "single flight: later requests reuse the token of the first re-login"
    );
}

#[tokio::test]
async fn the_re_login_uses_a_device_id_owned_by_the_proxy_not_the_clients() {
    let (f, backend, url) = setup().await;
    f.request(reqwest::Method::GET, &url, &f.user, "tv")
        .send()
        .await
        .unwrap();
    let devices = backend.0.lock().unwrap().login_devices.clone();
    assert_eq!(devices.len(), 1);
    assert!(devices[0].starts_with("jellyswarrm-"), "{devices:?}");
    assert!(
        !devices[0].contains("tv"),
        "the client's own id is not used: {devices:?}"
    );
}

#[tokio::test]
async fn a_backend_that_refuses_the_re_login_does_not_cause_a_loop() {
    let (f, backend, url) = setup().await;
    backend.0.lock().unwrap().reject_logins = true;
    let response = f
        .request(reqwest::Method::GET, &url, &f.user, "tv")
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "the 401 is passed on"
    );
    assert_eq!(
        stored_token(&f).await,
        "backend-token-0",
        "the stored session is left as it was"
    );
}
