import sys, re, json, traceback, urllib.parse
sys.path.insert(0, ".")
from lib import call
from playwright.sync_api import sync_playwright

P = "http://127.0.0.1:18090"
A, B = "http://127.0.0.1:18096", "http://127.0.0.1:18097"
USER, PW = "proxyadmin", "proxy-pw-e2e"
OUT = sys.argv[1]
results = []

def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + (f"  [{detail}]" if detail else ""), flush=True)
    results.append(bool(ok))

def scenario(fn):
    print(f"\n== {fn.__name__}", flush=True)
    try:
        fn()
    except Exception as e:  # a crashed scenario is a failure, not a stop
        check(f"{fn.__name__} did not crash", False, f"{type(e).__name__}: {str(e)[:300]}")
        traceback.print_exc(limit=3)

def me(page):
    """The identity the Jellyfin web client in this page is signed in with."""
    page.wait_for_function("() => window.ApiClient && window.ApiClient.accessToken && window.ApiClient.accessToken()", timeout=40000)
    return page.evaluate("""async () => { const u = await window.ApiClient.getCurrentUser();
        return {name: u.Name, admin: !!u.Policy.IsAdministrator, address: window.ApiClient.serverAddress()}; }""")

def real_storage_keys(pg):
    """The browser's real localStorage keys (the page's own `localStorage` is the scoped view)."""
    return pg.evaluate("""() => { const f = document.createElement('iframe'); f.style.display = 'none'; document.body.appendChild(f);
        const keys = Object.keys(f.contentWindow.localStorage); f.remove(); return keys; }""")

def connect(page, server_id, username, password):
    page.goto(f"{P}/ui", wait_until="networkidle")
    page.wait_for_selector(f'.connect-button[data-server-id="{server_id}"]', timeout=20000)
    page.click(f'.connect-button[data-server-id="{server_id}"]')
    page.fill('#connect_form input[name="username"]', username)
    page.fill('#connect_form input[name="password"]', password)
    page.click('#connect_form button[type="submit"]')
    page.wait_for_load_state("networkidle"); page.wait_for_timeout(1500)

def disconnect(page, server_id):
    page.goto(f"{P}/ui", wait_until="networkidle")
    page.wait_for_selector(f'button[hx-delete="/ui/user/servers/{server_id}"]', timeout=20000)
    page.once("dialog", lambda d: d.accept())
    page.click(f'button[hx-delete="/ui/user/servers/{server_id}"]')
    page.wait_for_load_state("networkidle"); page.wait_for_timeout(1500)

with sync_playwright() as pw:
    browser = pw.chromium.launch(executable_path="/usr/bin/google-chrome", args=["--no-sandbox"], headless=True)
    ctx = browser.new_context(viewport={"width": 1280, "height": 800})
    page = ctx.new_page()
    problems = []
    page.on("response", lambda r: problems.append((r.status, r.url)) if r.status >= 400 and "favicon" not in r.url else None)
    state = {}

    def s00_add_the_servers_from_the_ui():
        # Only when the proxy starts with no preconfigured servers: the administrator adds them.
        page.goto(f"{P}/ui/login", wait_until="networkidle")
        page.fill('input[name="username"]', USER); page.fill('input[name="password"]', PW)
        page.click('button[type="submit"]'); page.wait_for_load_state("networkidle"); page.wait_for_timeout(1500)
        page.locator("a.nav-link", has_text=re.compile(r"^\s*Servers\s*$")).first.click()
        page.wait_for_load_state("networkidle"); page.wait_for_timeout(1500)
        if "Server A" in page.inner_text("body"):
            print("(servers came from the configuration: nothing to add)"); return
        page.screenshot(path=f"{OUT}/00a-no-servers.png")
        check("a proxy with no preconfigured server starts with an empty list", "Loading" not in page.inner_text("#server-list") and "Server A" not in page.inner_text("#server-list"))
        for name, url in (("Server A", "http://jsw-jf-a:8096"), ("Server B", "http://jsw-jf-b:8096")):
            page.fill('#add-server input[name="name"]', name)
            page.fill('#add-server input[name="url"]', url)
            page.click('#add-server button[type="submit"]')
            page.wait_for_timeout(2000)
        listing = page.inner_text("#server-list"); page.screenshot(path=f"{OUT}/00b-servers-added.png")
        check("both servers are listed after adding them from the UI", "Server A" in listing and "Server B" in listing, listing[:80].replace("\n", " "))
        check("their address is the one typed", "http://jsw-jf-a:8096" in listing and "http://jsw-jf-b:8096" in listing)
        check("the server list has no streaming-mode choice (everything goes through the proxy)", page.locator("#server-list select").count() == 0)
        page.wait_for_timeout(6000)
        check("both are reported Online", page.inner_text("#server-list").count("Online") >= 2, page.inner_text("#server-list")[:200].replace("\n", " "))
    scenario(s00_add_the_servers_from_the_ui)

    def s01_fresh_login_and_navigation():
        page.goto(f"{P}/ui/login", wait_until="networkidle")
        page.fill('input[name="username"]', USER); page.fill('input[name="password"]', PW)
        page.click('button[type="submit"]'); page.wait_for_load_state("networkidle"); page.wait_for_timeout(2000)
        text = page.inner_text("body")
        page.screenshot(path=f"{OUT}/01-my-servers-empty.png")
        check("the bootstrapped administrator signs in to /ui", page.url.rstrip("/").endswith("/ui"), page.url)
        check("the admin sees 'My servers' and the admin tabs", all(t in text for t in ("My servers", "Servers", "Users", "Settings", "Profile")))
        check("both fresh servers are offered to connect", "Server A" in text and "Server B" in text and "Available Servers" in text)
        check("nothing is connected yet, so no Administer button", page.locator('a:has-text("Administer")').count() == 0)
        setup = page.locator('a:has-text("Set up")')
        check("only the brand-new server is offered 'Set up'", setup.count() == 1 and "/backend/2/web/" in setup.first.get_attribute("href"), str(setup.count()))
        opener = page.locator('a:has-text("Open")')
        check("the server that is already set up is offered 'Open'", opener.count() == 1 and "/backend/1/web/" in opener.first.get_attribute("href"), str(opener.count()))
    scenario(s01_fresh_login_and_navigation)

    def s01a_open_a_server_before_mapping_any_account():
        pg = ctx.new_page()
        pg.goto(f"{P}/ui/backend/1/web/", wait_until="networkidle")
        pg.wait_for_selector("#txtManualName", timeout=30000)
        check("with no mapped account the server's own login page is shown", "Please sign in" in pg.inner_text("body"))
        pg.fill("#txtManualName", "owner"); pg.fill("#txtManualPassword", "owner-pw-A"); pg.click('button[type="submit"]')
        who = me(pg); pg.screenshot(path=f"{OUT}/01a-manual-login-A.png")
        check("signing in there with the server's own account works", who["name"] == "owner" and who["admin"] and who["address"].endswith("/ui/backend/1"), str(who))
        pg.close()
    scenario(s01a_open_a_server_before_mapping_any_account)

    def s01b_first_setup_of_a_new_server_through_the_proxy():
        with ctx.expect_page() as popup:
            page.locator('a:has-text("Set up")').first.click()
        pg = popup.value
        pg.wait_for_load_state("networkidle", timeout=60000); pg.wait_for_timeout(3000)
        pg.screenshot(path=f"{OUT}/01b-wizard-start.png")
        check("the Jellyfin first-run wizard opens through the proxy", "wizard/start" in pg.url and "Welcome to Jellyfin" in pg.inner_text("body"), pg.url)
        steps = []
        for _ in range(8):
            where = pg.url.split("#")[-1]
            steps.append(where)
            submit = pg.locator("button.button-submit:visible")
            if "wizard/start" in where:
                pg.fill("#txtServerName", "Server B (new)")
            elif pg.locator("#txtUsername:visible").count():
                pg.fill("#txtUsername", "boss"); pg.fill("#txtManualPassword", "boss-pw-B"); pg.fill("#txtPasswordConfirm", "boss-pw-B")
            if not submit.count():
                break
            submit.first.click(); pg.wait_for_timeout(2500)
            if "wizard" not in pg.url:
                break
        check("every wizard step runs through the proxy, creating the first user",
              [x for x in steps if x.startswith("/wizard")] == ["/wizard/start", "/wizard/user", "/wizard/library", "/wizard/settings", "/wizard/remoteaccess", "/wizard/finish"], str(steps))
        done = call(B, "GET", "/System/Info/Public")[1]
        check("the server is now set up", done["StartupWizardCompleted"] is True and done["ServerName"] == "Server B (new)", str(done.get("ServerName")))
        # Well past any short-lived state: the page must keep working after the wizard.
        pg.wait_for_timeout(12000)
        pg.reload(wait_until="networkidle")
        pg.wait_for_selector("#txtManualName", timeout=30000)
        pg.screenshot(path=f"{OUT}/01c-after-wizard-login.png")
        pg.fill("#txtManualName", "boss"); pg.fill("#txtManualPassword", "boss-pw-B"); pg.click('button[type="submit"]')
        who = me(pg)
        check("the account the wizard created signs in on the same page", who["name"] == "boss" and who["admin"] and who["address"].endswith("/ui/backend/2"), str(who))
        pg.close()
        # B also needs an ordinary account and a library for the scenarios that follow.
        tok = call(B, "POST", "/Users/AuthenticateByName", {"Username": "boss", "Pw": "boss-pw-B"}, device_id="setupcheck")[1]["AccessToken"]
        call(B, "POST", "/Users/New", {"Name": "guest", "Password": "guest-pw-B"}, token=tok, device_id="setupcheck")
        call(B, "POST", "/Library/VirtualFolders?name=Movies&collectionType=movies&refreshLibrary=true", {"LibraryOptions": {"PathInfos": [{"Path": "/media"}]}}, token=tok, device_id="setupcheck")
        import time
        for _ in range(30):
            if len(call(B, "GET", "/Items?Recursive=true&IncludeItemTypes=Movie", token=tok, device_id="setupcheck")[1]["Items"]) >= 2:
                break
            call(B, "POST", "/Library/Refresh", token=tok, device_id="setupcheck"); time.sleep(3)
        page.goto(f"{P}/ui", wait_until="networkidle"); page.wait_for_timeout(1500)
        check("once set up, the server is no longer offered 'Set up' (at once)", page.locator('a:has-text("Set up")').count() == 0)
    scenario(s01b_first_setup_of_a_new_server_through_the_proxy)

    def s02_federation_is_gone():
        page.goto(f"{P}/ui", wait_until="networkidle")
        page.click('a.nav-link:has-text("Users")'); page.wait_for_load_state("networkidle"); page.wait_for_timeout(1000)
        users = page.inner_text("body"); page.screenshot(path=f"{OUT}/02-users.png")
        check("Users page has no federation option", "Federation" not in users and "Also delete on connected" not in users)
        page.click('a.nav-link:has-text("Servers")', strict=False) if False else None
        page.goto(f"{P}/ui", wait_until="networkidle")
        page.locator('a.nav-link', has_text=re.compile(r"^\s*Servers\s*$")).first.click()
        page.wait_for_load_state("networkidle"); page.wait_for_timeout(1500)
        servers = page.inner_text("body"); page.screenshot(path=f"{OUT}/03-servers-admin.png")
        check("Servers page has no 'Add Admin' nor 'Federated'", "Add Admin" not in servers and "Federated" not in servers)
        check("the Servers page still lists both servers", "Server A" in servers and "Server B" in servers)
    scenario(s02_federation_is_gone)

    def s03_map_accounts_with_different_names():
        connect(page, 1, "owner", "owner-pw-A")        # admin on A, name differs from the proxy user
        connect(page, 2, "guest", "guest-pw-B")        # NOT admin on B
        text = page.inner_text("body"); page.screenshot(path=f"{OUT}/04-connected.png")
        check("both servers are connected", "Your Connected Servers" in text and page.locator('button[hx-delete^="/ui/user/servers/"]').count() == 2)
        check("Administer is offered only for the server where the mapped account is admin",
              page.locator('a:has-text("Administer")').count() == 1 and "/backend/1/web/" in page.locator('a:has-text("Administer")').first.get_attribute("href"),
              str(page.locator('a:has-text("Administer")').count()))
    scenario(s03_map_accounts_with_different_names)

    def s04_dashboard_opens_signed_in():
        link = page.locator('a:has-text("Administer")').first
        with ctx.expect_page() as popup:
            link.click()
        pop = popup.value
        pop.wait_for_load_state("networkidle", timeout=60000)
        who = me(pop); pop.wait_for_timeout(3500)
        pop.screenshot(path=f"{OUT}/05-dashboard-A.png")
        body = pop.inner_text("body")
        check("the panel opens already signed in (no login form)", "Please sign in" not in body, body[:60].replace("\n", " "))
        check("it is the mapped backend account, not the proxy user", who["name"] == "owner" and who["admin"], str(who))
        check("the client talks to the backend through the pass-through", who["address"].endswith("/ui/backend/1"), who["address"])
        check("it lands on the dashboard", "#/dashboard" in pop.url, pop.url)
        check("the dashboard content is there", any(w in body for w in ("Dashboard", "Server", "Activity", "Libraries")), body[:80].replace("\n", " "))
        state["popupA"] = pop
    scenario(s04_dashboard_opens_signed_in)

    def s05_non_admin_server_is_refused():
        r = ctx.request.get(f"{P}/ui/backend/2/web/", max_redirects=0)
        check("the pass-through refuses a server where the mapped account is not admin", r.status == 403, str(r.status))
    scenario(s05_non_admin_server_is_refused)

    def s06_isolation_from_the_proxys_own_web_client():
        web = ctx.new_page()
        web.goto(f"{P}/web/", wait_until="networkidle")
        web.wait_for_selector("#txtManualName, input[name=username]", timeout=30000)
        web.fill("#txtManualName", USER); web.fill("#txtManualPassword", PW)
        web.click('button[type="submit"]'); web.wait_for_load_state("networkidle")
        w1 = me(web); web.wait_for_timeout(2500); web.screenshot(path=f"{OUT}/06-proxy-web-client.png")
        check("the proxy's own web client signs in with the proxy user", w1["name"] == USER and w1["address"] == P, str(w1))
        # Open the panel AFTER the proxy client has stored its credentials at the same origin.
        again = ctx.new_page()
        again.goto(f"{P}/ui/backend/1/web/#/dashboard", wait_until="networkidle")
        w2 = me(again); again.wait_for_timeout(2500); again.screenshot(path=f"{OUT}/07-dashboard-A-after-proxy-client.png")
        check("the panel is NOT hijacked by the proxy client's stored server", w2["name"] == "owner" and w2["address"].endswith("/ui/backend/1"), str(w2))
        web.reload(wait_until="networkidle")
        w3 = me(web)
        check("and the proxy's own client is not hijacked by the panel", w3["name"] == USER and w3["address"] == P, str(w3))
        keys = web.evaluate("() => Object.keys(window.localStorage)")
        check("storage is kept apart per backend", any(k.startswith("jsw-backend-1:") for k in keys) and "jellyfin_credentials" in keys, str([k for k in keys if 'cred' in k]))
        again.close(); web.close()
    scenario(s06_isolation_from_the_proxys_own_web_client)

    def s07_apps_use_the_proxy_user():
        st, native = call(P, "POST", "/Users/AuthenticateByName", {"Username": USER, "Pw": PW}, client="Jellyfin Android TV", device_id="tv")
        check("an app signs in with the proxy user", st == 200, str(st))
        check("native apps get the administrator flag (admin on A)", st == 200 and native["User"]["Policy"]["IsAdministrator"] is True)
        st, webl = call(P, "POST", "/Users/AuthenticateByName", {"Username": USER, "Pw": PW}, client="Jellyfin Web", device_id="webx")
        check("the web client gets false (it reads per-item flags)", st == 200 and webl["User"]["Policy"]["IsAdministrator"] is False)
        for who, pw in (("owner", "owner-pw-A"), ("guest", "guest-pw-B"), (USER, "wrong")):
            st, _ = call(P, "POST", "/Users/AuthenticateByName", {"Username": who, "Pw": pw}, client="Jellyfin Android TV", device_id="tv2")
            check(f"'{who}' with a backend password / wrong password cannot sign in to the proxy (no auto-created users)", st in (401, 403, 400), str(st))
        state["tok"] = webl["AccessToken"]
    scenario(s07_apps_use_the_proxy_user)

    def s08_management_follows_the_mapped_accounts():
        tok = state["tok"]
        st, items = call(P, "GET", "/Items?Recursive=true&IncludeItemTypes=Movie", client="Jellyfin Web", device_id="webx", token=tok)
        movies = items["Items"] if st == 200 else []
        flags = {(m["Name"], m["JellyswarrmCanManage"]) for m in movies if "JellyswarrmCanManage" in m}
        check("the unified library lists the movies of both fresh servers", len(movies) >= 3, str([m["Name"] for m in movies]))
        check("per-item flags: true only for the server where the mapped account is admin",
              ("Test Movie (2026) [Server A]", True) in flags and ("Test Movie (2026) [Server B]", False) in flags and ("Only On B (2026) [Server B]", False) in flags, str(sorted(flags)))
        mine = next(m for m in movies if m.get("JellyswarrmCanManage") is True)
        theirs = next(m for m in movies if m["Name"].startswith("Only On B"))
        q = "?MetadataRefreshMode=FullRefresh&ImageRefreshMode=ValidationOnly&ReplaceAllMetadata=false"
        check("refresh works on the admin server", call(P, "POST", f"/Items/{mine['Id']}/Refresh{q}", client="Jellyfin Web", device_id="webx", token=tok)[0] == 204)
        check("refresh is refused where the mapped account is not admin", call(P, "POST", f"/Items/{theirs['Id']}/Refresh{q}", client="Jellyfin Web", device_id="webx", token=tok)[0] == 403)
        check("server-wide administration on the merged API is refused", call(P, "GET", "/System/Configuration", client="Jellyfin Web", device_id="webx", token=tok)[0] == 403)
    scenario(s08_management_follows_the_mapped_accounts)

    def s09_panel_sessions_do_not_pile_up():
        root = call(A, "POST", "/Users/AuthenticateByName", {"Username": "root-a", "Pw": "rootpw-a"}, device_id="rootcheck")[1]["AccessToken"]
        def panel_devices():
            st, devs = call(A, "GET", "/Devices", token=root, device_id="rootcheck")
            return sorted(d["Id"] for d in devs["Items"] if str(d.get("Id", "")).startswith("jellyswarrm-panel")) if st == 200 else None
        before = panel_devices()
        for _ in range(4):
            p2 = ctx.new_page(); p2.goto(f"{P}/ui/backend/1/web/#/dashboard", wait_until="networkidle"); me(p2); p2.close()
        after = panel_devices()
        check("the backend already knows the panel's device", bool(before), str(before))
        check("opening the dashboard four more times adds no device on the backend", before == after, f"{before} -> {after}")
    scenario(s09_panel_sessions_do_not_pile_up)

    def s10_remap_b_to_an_admin_with_another_name():
        disconnect(page, 2)
        connect(page, 2, "boss", "boss-pw-B")
        page.screenshot(path=f"{OUT}/08-both-admin.png")
        check("with an admin account mapped on both, both get Administer", page.locator('a:has-text("Administer")').count() == 2)
        pop = ctx.new_page(); pop.goto(f"{P}/ui/backend/2/web/#/dashboard", wait_until="networkidle")
        w = me(pop); pop.wait_for_timeout(3000); pop.screenshot(path=f"{OUT}/09-dashboard-B.png")
        check("server B opens signed in as its own mapped account", w["name"] == "boss" and w["admin"] and w["address"].endswith("/ui/backend/2"), str(w))
        pa = ctx.new_page(); pa.goto(f"{P}/ui/backend/1/web/#/dashboard", wait_until="networkidle")
        wa = me(pa)
        check("and server A is still its own (no mix-up between backends)", wa["name"] == "owner" and wa["address"].endswith("/ui/backend/1"), str(wa))
        keys = real_storage_keys(pa)
        check("each backend has its own storage namespace", any(k.startswith("jsw-backend-1:") for k in keys) and any(k.startswith("jsw-backend-2:") for k in keys), str([k for k in keys if 'cred' in k]))
        pop.close(); pa.close()
    scenario(s10_remap_b_to_an_admin_with_another_name)

    def s11_the_administrator_cannot_be_deleted():
        listing = ctx.request.get(f"{P}/ui/users/list").text()
        ids = re.findall(r'hx-post="/ui/users/([^/]+)/delete"', listing)
        check("the Users page lists the administrator", len(ids) >= 1, str(len(ids)))
        r = ctx.request.post(f"{P}/ui/users/{ids[0]}/delete")
        check("deleting the administrator is refused", r.status == 409 and "cannot be deleted" in r.text(), f"{r.status}")
        st, _ = call(P, "POST", "/Users/AuthenticateByName", {"Username": USER, "Pw": PW}, client="Jellyfin Android TV", device_id="tv3")
        check("the administrator can still sign in", st == 200)
    scenario(s11_the_administrator_cannot_be_deleted)

    bad = [p for p in problems if p[0] not in (401, 403, 404)]
    print("\nunexpected HTTP errors seen by the browser:", bad[:8] or "none")
    browser.close()

ok = sum(results)
print(f"\n==== {ok}/{len(results)} checks passed")
sys.exit(0 if ok == len(results) else 1)
