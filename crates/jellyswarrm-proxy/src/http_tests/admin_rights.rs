//! Per-backend administrator rights: item management reaches only backends where
//! the caller's own backend account is an administrator, server-wide
//! administration is refused on the merged API, and the administrator flag shown
//! to clients means "administrator on at least one backend".
use super::*;
use wiremock::{
    matchers::{method, path},
    Mock, ResponseTemplate,
};

async fn mount_refresh(f: &Fixture, index: usize) -> String {
    let original = f.original(index).await;
    Mock::given(method("POST"))
        .and(path(format!("/Items/{original}/Refresh")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&f.upstreams[index])
        .await;
    original
}

fn refresh(item: &str) -> String {
    format!("/Items/{item}/Refresh?MetadataRefreshMode=FullRefresh")
}

#[tokio::test]
async fn item_management_reaches_only_administrator_backends() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    f.administrator_on(0, false).await;
    f.administrator_on(1, true).await;
    mount_refresh(&f, 0).await;
    mount_refresh(&f, 1).await;

    let allowed = f
        .request(reqwest::Method::POST, &refresh(&f.items[1]), &f.user, "tv")
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::NO_CONTENT);
    assert_eq!(f.requests_without_admin_lookup(1).await.len(), 1);

    let refused = f
        .request(reqwest::Method::POST, &refresh(&f.items[0]), &f.user, "tv")
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert!(
        f.requests_without_admin_lookup(0).await.is_empty(),
        "a non-administrator backend must not receive the management request"
    );
}

#[tokio::test]
async fn management_is_refused_when_not_an_administrator_anywhere() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    f.administrator_on(0, false).await;
    f.administrator_on(1, false).await;
    mount_refresh(&f, 1).await;

    let response = f
        .request(reqwest::Method::POST, &refresh(&f.items[1]), &f.user, "tv")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(f.requests_without_admin_lookup(1).await.is_empty());
}

#[tokio::test]
async fn management_is_refused_when_the_backend_lookup_fails() {
    // No /Users/{id} mock: the backend answers 404, which must read as "not an
    // administrator", never as permission.
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    mount_refresh(&f, 1).await;

    let response = f
        .request(reqwest::Method::POST, &refresh(&f.items[1]), &f.user, "tv")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(f.requests_without_admin_lookup(1).await.is_empty());
}

#[tokio::test]
async fn server_wide_administration_is_refused_even_for_administrators() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    f.administrator_on(0, true).await;
    f.administrator_on(1, true).await;

    for (verb, route, expected) in [
        (
            reqwest::Method::GET,
            "/System/Configuration",
            StatusCode::FORBIDDEN,
        ),
        (
            reqwest::Method::POST,
            "/System/Configuration/encoding",
            StatusCode::FORBIDDEN,
        ),
        (
            reqwest::Method::POST,
            "/System/Restart",
            StatusCode::FORBIDDEN,
        ),
        (
            reqwest::Method::GET,
            "/Library/VirtualFolders",
            StatusCode::FORBIDDEN,
        ),
        (
            reqwest::Method::POST,
            "/Library/Refresh",
            StatusCode::FORBIDDEN,
        ),
        (
            reqwest::Method::GET,
            "/ScheduledTasks",
            StatusCode::FORBIDDEN,
        ),
        (reqwest::Method::GET, "/Plugins", StatusCode::FORBIDDEN),
        (
            reqwest::Method::GET,
            "/sYsTem/%43onfiguration",
            StatusCode::FORBIDDEN,
        ),
        // The explicit /Users routes only serve GET, so these never reach a backend.
        (
            reqwest::Method::POST,
            "/Users/New",
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            reqwest::Method::DELETE,
            "/Users/some-user",
            StatusCode::METHOD_NOT_ALLOWED,
        ),
    ] {
        let response = f.request(verb, route, &f.user, "tv").send().await.unwrap();
        assert_eq!(response.status(), expected, "{route}");
    }
    f.no_upstream_requests().await;
}

#[tokio::test]
async fn item_delete_is_decided_by_the_owning_backend_when_not_an_administrator() {
    // DELETE /Items/{id} is also how a user deletes their own playlist or
    // collection, so a non-administrator is not refused by the proxy.
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    f.administrator_on(0, false).await;
    f.administrator_on(1, false).await;
    let original = f.original(0).await;
    Mock::given(method("DELETE"))
        .and(path(format!("/Items/{original}")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&f.upstreams[0])
        .await;

    let response = f
        .request(
            reqwest::Method::DELETE,
            &format!("/Items/{}", f.items[0]),
            &f.user,
            "tv",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(f.requests_without_admin_lookup(1).await.len(), 0);
}

#[tokio::test]
async fn administrator_flag_means_administrator_on_any_backend() {
    for (admin_on, expected) in [(None, false), (Some(1), true)] {
        let f = Fixture::for_scenario(Scenario::NormalServers).await;
        f.healthy().await;
        for index in 0..2 {
            f.administrator_on(index, admin_on == Some(index)).await;
            Mock::given(method("GET"))
                .and(path("/Users/Me"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "Id": format!("backend-user-{index}"), "Name": "alice", "ServerId": "backend",
                    "Policy": {"IsAdministrator": admin_on == Some(index), "SyncPlayAccess": "None"}
                })))
                .mount(&f.upstreams[index])
                .await;
        }
        let me: Value = f
            .request(reqwest::Method::GET, "/Users/Me", &f.user, "tv")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            me["Policy"]["IsAdministrator"], expected,
            "admin on {admin_on:?}"
        );
    }
}

#[tokio::test]
async fn login_reports_administrator_when_any_backend_says_so() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    for (index, backend) in f.upstreams.iter().enumerate() {
        Mock::given(method("POST"))
            .and(path("/Users/AuthenticateByName"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "User": {"Id": format!("remote-{index}"), "Name": "alice", "ServerId": "backend",
                         "Policy": {"IsAdministrator": index == 1, "SyncPlayAccess": "None"}},
                "AccessToken": format!("fresh-token-{index}"), "ServerId": "backend",
                "SessionInfo": {"UserId": format!("remote-{index}"), "UserName": "alice", "ServerId": "backend"}
            })))
            .mount(backend)
            .await;
    }
    let login: Value = f
        .request(
            reqwest::Method::POST,
            "/Users/AuthenticateByName",
            &f.user,
            "login-device",
        )
        .json(&json!({"Username": "alice", "Pw": "password"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(login["User"]["Policy"]["IsAdministrator"], true);
}

#[tokio::test]
async fn items_tell_the_client_where_the_viewer_may_manage() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    f.administrator_on(0, false).await;
    f.administrator_on(1, true).await;
    for index in 0..2 {
        let original = f.original(index).await;
        Mock::given(method("GET"))
            .and(path(format!("/Items/{original}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"Id": original, "Type": "Movie", "Name": "Movie", "ServerId": "backend"}),
            ))
            .mount(&f.upstreams[index])
            .await;
    }
    let detail = |index: usize| {
        let route = format!("/Items/{}", f.items[index]);
        let request = f.request(reqwest::Method::GET, &route, &f.user, "tv");
        async move { request.send().await.unwrap().json::<Value>().await.unwrap() }
    };

    // Nothing is known about this viewer yet, so nothing is claimed: the proxy
    // never waits for a lookup while answering a normal request.
    let before = detail(1).await;
    assert!(before.get("JellyswarrmCanManage").is_none(), "{before}");

    // The web client asks who the user is right after login: that fills the cache.
    for index in 0..2 {
        Mock::given(method("GET"))
            .and(path("/Users/Me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Id": format!("backend-user-{index}"), "Name": "alice", "ServerId": "backend",
                "Policy": {"IsAdministrator": index == 1, "SyncPlayAccess": "None"}
            })))
            .mount(&f.upstreams[index])
            .await;
    }
    let me = f
        .request(reqwest::Method::GET, "/Users/Me", &f.user, "tv")
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);

    assert_eq!(detail(1).await["JellyswarrmCanManage"], true);
    assert_eq!(detail(0).await["JellyswarrmCanManage"], false);
}

/// A device of this app, with its own backend sessions (as a real login would
/// create), and the header that device sends.
async fn app_device(f: &Fixture, client: &str) -> String {
    let device_id = format!("{}-device", client.replace(' ', "-").to_lowercase());
    let authorization = Authorization {
        client: client.into(),
        device: "Living room".into(),
        device_id: device_id.clone(),
        version: "1".into(),
        token: None,
    };
    for (index, server) in f
        .state
        .server_storage
        .list_servers()
        .await
        .unwrap()
        .iter()
        .enumerate()
    {
        f.state
            .user_authorization
            .store_authorization_session(
                &f.user.id,
                server,
                &authorization,
                format!("app-token-{device_id}-{index}"),
                format!("backend-user-{index}"),
                None,
            )
            .await
            .unwrap();
    }
    Authorization {
        token: Some(f.token().clone()),
        ..authorization
    }
    .to_header_value()
}

#[tokio::test]
async fn the_web_client_reads_items_while_native_apps_get_the_global_flag() {
    for (client, expected_flag) in [("Jellyfin Web", false), ("Jellyfin Android TV", true)] {
        let f = Fixture::for_scenario(Scenario::NormalServers).await;
        f.healthy().await;
        for index in 0..2 {
            f.administrator_on(index, index == 1).await;
            Mock::given(method("GET"))
                .and(path("/Users/Me"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "Id": format!("backend-user-{index}"), "Name": "alice", "ServerId": "backend",
                    "Policy": {"IsAdministrator": index == 1, "SyncPlayAccess": "None"}
                })))
                .mount(&f.upstreams[index])
                .await;
        }
        let header = app_device(&f, client).await;
        let me = reqwest::Client::new()
            .get(format!("{}/Users/Me", f.url))
            .header("Authorization", header)
            .send()
            .await
            .unwrap();
        assert_eq!(me.status(), StatusCode::OK, "{client}");
        let me: Value = me.json().await.unwrap();
        assert_eq!(me["Policy"]["IsAdministrator"], expected_flag, "{client}");
    }
}

// --- Native apps: the backend's "can delete" answer, for administrators --------------

/// Every backend answers `GET /Items/{id}` with an item of `kind` that the backend
/// itself says the viewer may delete and download.
async fn mount_deletable_item(f: &Fixture, kind: &str) {
    for index in 0..2 {
        let original = f.original(index).await;
        Mock::given(method("GET"))
            .and(path(format!("/Items/{original}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Id": original, "Type": kind, "Name": "Item", "ServerId": "backend",
                "CanDelete": true, "CanDownload": true
            })))
            .mount(&f.upstreams[index])
            .await;
    }
}

/// What the proxy tells the viewer about the item that lives on backend `index`.
async fn item_for_viewer(f: &Fixture, index: usize) -> Value {
    let route = format!("/Items/{}", f.items[index]);
    f.request(reqwest::Method::GET, &route, &f.user, "tv")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()
}

/// The app asks who the viewer is right after login; that is how the proxy learns
/// which backends the viewer administers (backend 1 here, not backend 0).
async fn viewer_logs_in(f: &Fixture) {
    for index in 0..2 {
        Mock::given(method("GET"))
            .and(path("/Users/Me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Id": format!("backend-user-{index}"), "Name": "alice", "ServerId": "backend",
                "Policy": {"IsAdministrator": index == 1, "SyncPlayAccess": "None"}
            })))
            .mount(&f.upstreams[index])
            .await;
    }
    let me = f
        .request(reqwest::Method::GET, "/Users/Me", &f.user, "tv")
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);
}

#[tokio::test]
async fn apps_get_the_backends_delete_answer_for_every_kind_of_item_where_the_viewer_administers() {
    for kind in [
        "Movie",
        "Series",
        "Season",
        "Episode",
        "MusicAlbum",
        "Audio",
        "BoxSet",
    ] {
        let f = Fixture::for_scenario(Scenario::NormalServers).await;
        f.healthy().await;
        f.administrator_on(0, false).await;
        f.administrator_on(1, true).await;
        mount_deletable_item(&f, kind).await;
        viewer_logs_in(&f).await;

        let administered = item_for_viewer(&f, 1).await;
        assert_eq!(administered["CanDelete"], true, "{kind}: {administered}");
        let other = item_for_viewer(&f, 0).await;
        assert_eq!(
            other["CanDelete"], false,
            "{kind}: a backend where the viewer is not an administrator offers no delete: {other}"
        );
        // Downloads stay off everywhere: the proxy does not serve them.
        assert_eq!(administered["CanDownload"], false, "{kind}");
        assert_eq!(other["CanDownload"], false, "{kind}");
    }
}

#[tokio::test]
async fn library_views_are_never_offered_for_deletion() {
    for kind in ["UserView", "CollectionFolder"] {
        let f = Fixture::for_scenario(Scenario::NormalServers).await;
        f.healthy().await;
        f.administrator_on(0, false).await;
        f.administrator_on(1, true).await;
        mount_deletable_item(&f, kind).await;
        viewer_logs_in(&f).await;

        let view = item_for_viewer(&f, 1).await;
        assert_eq!(view["CanDelete"], false, "{kind}: {view}");
    }
}

#[tokio::test]
async fn the_backend_has_the_last_word_on_delete() {
    // An administrator whose backend account may not delete (the Jellyfin permission
    // "allow media deletion") must not be offered it: the backend's `false` is kept.
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    f.healthy().await;
    f.administrator_on(0, false).await;
    f.administrator_on(1, true).await;
    for index in 0..2 {
        let original = f.original(index).await;
        Mock::given(method("GET"))
            .and(path(format!("/Items/{original}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Id": original, "Type": "Movie", "Name": "Item", "ServerId": "backend",
                "CanDelete": false, "CanDownload": false
            })))
            .mount(&f.upstreams[index])
            .await;
    }
    viewer_logs_in(&f).await;

    let item = item_for_viewer(&f, 1).await;
    assert_eq!(item["CanDelete"], false, "{item}");
    assert_eq!(item["JellyswarrmCanManage"], true, "{item}");
}

#[tokio::test]
async fn an_unknown_viewer_is_not_claimed_to_manage_but_is_looked_up_in_the_background() {
    let f = Fixture::for_scenario(Scenario::NormalServers).await;
    // Off in the test harness (so request-counting tests are not racy); this is the test
    // of the feature itself.
    f.state.services.admin_rights.enable_background_refresh();
    f.healthy().await;
    f.administrator_on(0, false).await;
    f.administrator_on(1, true).await;
    mount_deletable_item(&f, "Movie").await;

    // Nothing is known yet (no login seen by this proxy process): the answer is
    // not delayed and claims nothing.
    let first = item_for_viewer(&f, 1).await;
    assert_eq!(first["CanDelete"], false, "{first}");
    assert!(first.get("JellyswarrmCanManage").is_none(), "{first}");

    // The lookup that run started in the background lands in the cache, so a
    // following screen of the same app has the flag.
    let mut later = first;
    for _ in 0..60 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        later = item_for_viewer(&f, 1).await;
        if later["CanDelete"] == true {
            break;
        }
    }
    assert_eq!(later["CanDelete"], true, "{later}");
    assert_eq!(later["JellyswarrmCanManage"], true, "{later}");
}
