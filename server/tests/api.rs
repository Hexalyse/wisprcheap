//! Integration tests of the HTTP application (API and web UI) on a temporary database.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use wisprcheap_server::{AppState, Config, Db, SharedState, auth, db};

struct T {
    app: Router,
    state: SharedState,
    _dir: tempfile::TempDir,
}

const ORIGIN: &str = "http://localhost:8080";

impl T {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::for_tests(dir.path());
        let db = Db::open(&config.db_path()).unwrap();
        let state = AppState::new(config, db);
        Self { app: wisprcheap_server::app(state.clone()), state, _dir: dir }
    }

    async fn call(&self, method: &str, uri: &str, headers: &[(&str, &str)], body: Body) -> (StatusCode, HeaderMap, String) {
        let mut req = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = self.app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, headers, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn api(&self, method: &str, uri: &str, token: &str, body: Option<Value>) -> (StatusCode, Value) {
        let auth = format!("Bearer {token}");
        let mut headers = vec![("content-type", "application/json")];
        if !token.is_empty() {
            headers.push(("authorization", auth.as_str()));
        }
        let body = body.map(|b| Body::from(b.to_string())).unwrap_or_else(Body::empty);
        let (status, _, text) = self.call(method, uri, &headers, body).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    fn user(&self, name: &str, admin: bool) -> String {
        let conn = self.state.db.lock();
        db::create_user(&conn, name, &auth::hash_password("a long enough password").unwrap(), admin).unwrap()
    }

    async fn pair(&self, user_id: &str, name: &str) -> String {
        let code = db::create_pairing_code(&self.state.db.lock(), user_id, None).unwrap().0;
        let (status, body) = self
            .api("POST", "/v1/pair", "", Some(json!({"code": code, "name": name, "platform": "android", "appVersion": "0.2.0"})))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["token"].as_str().unwrap().to_string()
    }

    async fn form(&self, uri: &str, cookie: Option<&str>, origin: bool, fields: &[(&str, &str)]) -> (StatusCode, HeaderMap, String) {
        let body = fields.iter().map(|(k, v)| format!("{k}={}", urlencode(v))).collect::<Vec<_>>().join("&");
        let cookie_header = cookie.map(|c| format!("wcs_session={c}"));
        let mut headers = vec![("content-type", "application/x-www-form-urlencoded")];
        if origin {
            headers.push(("origin", ORIGIN));
        }
        if let Some(c) = &cookie_header {
            headers.push(("cookie", c));
        }
        self.call("POST", uri, &headers, Body::from(body)).await
    }

    async fn page(&self, uri: &str, cookie: &str) -> (StatusCode, String) {
        let c = format!("wcs_session={cookie}");
        let (status, _, text) = self.call("GET", uri, &[("cookie", &c)], Body::empty()).await;
        (status, text)
    }

    async fn login(&self, username: &str) -> String {
        let (status, headers, _) = self
            .form("/login", None, true, &[("username", username), ("password", "a long enough password")])
            .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        session_cookie(&headers).expect("session cookie")
    }
}

fn urlencode(v: &str) -> String {
    v.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let v = headers.get("set-cookie")?.to_str().ok()?;
    let value = v.strip_prefix("wcs_session=")?.split(';').next()?;
    (!value.is_empty()).then(|| value.to_string())
}

fn csrf(html: &str) -> String {
    let start = html.find("name=\"csrf\" value=\"").expect("csrf field") + 19;
    html[start..start + html[start..].find('"').unwrap()].to_string()
}

fn hlc(ms: u64, device: &str) -> String {
    format!("{ms:013}-0000-{device}")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

fn stats(ts: &str, words: u64) -> Value {
    json!({"ts": ts, "mode": "dictation", "durationSec": 60.0, "sttProvider": "elevenlabs", "sttModel": "scribe_v2",
           "sttMs": 800, "keyterms": 0, "llmModel": "gpt-6-luna", "llmMs": 500, "inputTokens": 1000, "outputTokens": 100,
           "words": words, "status": if words > 0 { "ok" } else { "empty" }, "retry": false,
           "costStt": 0.004, "costLlm": 0.00015, "costTotal": 0.00415})
}

#[tokio::test]
async fn pairing_and_me() {
    let t = T::new();
    let alice = t.user("alice", false);
    let token = t.pair(&alice, "Pixel").await;
    assert!(token.starts_with("wcs_") && token.len() == 47);
    let (status, me) = t.api("GET", "/v1/me", &token, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["user"]["username"], "alice");
    assert_eq!(me["device"]["name"], "Pixel");
    assert!(me["keyring"].is_null());

    // A code works once, and is accepted typed as "abcd-2345".
    let code = db::create_pairing_code(&t.state.db.lock(), &alice, Some("Laptop")).unwrap().0;
    let typed = format!("{}-{}", code[..4].to_lowercase(), &code[4..]);
    let (status, body) = t.api("POST", "/v1/pair", "", Some(json!({"code": typed, "name": "x", "platform": "windows", "appVersion": "1.2.0"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["device"]["name"], "Laptop", "the name given in the web UI wins");
    let (status, body) = t.api("POST", "/v1/pair", "", Some(json!({"code": code, "name": "x", "platform": "windows", "appVersion": "1"}))).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::NOT_FOUND, Some("invalid_code")));

    assert_eq!(t.api("GET", "/v1/me", "wcs_nope", None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(t.api("GET", "/v1/me", "", None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn keyring_rules() {
    let t = T::new();
    let alice = t.user("alice", false);
    let token = t.pair(&alice, "Pixel").await;
    let keyring = json!({"keyId": "7Kpr3YdH0bYsKmxq", "salt": "AAECAwQFBgcICQoLDA0ODw", "kdf": {"alg": "argon2id", "m": 65536, "t": 3, "p": 1}, "wrappedKey": "e1.abc"});
    let (status, body) = t.api("PUT", "/v1/keyring", &token, Some(keyring.clone())).await;
    assert_eq!((status, body["keyVersion"].as_i64()), (StatusCode::OK, Some(1)));
    assert_eq!(t.api("PUT", "/v1/keyring", &token, Some(keyring.clone())).await.0, StatusCode::CONFLICT);

    let put = |if_match: &'static str, body: Value| {
        let t = &t;
        let token = token.clone();
        async move {
            let auth = format!("Bearer {token}");
            let (status, _, text) = t
                .call("PUT", "/v1/keyring", &[("content-type", "application/json"), ("authorization", &auth), ("if-match", if_match)], Body::from(body.to_string()))
                .await;
            (status, serde_json::from_str::<Value>(&text).unwrap_or(Value::Null))
        }
    };
    let mut changed = keyring.clone();
    changed["wrappedKey"] = json!("e1.def");
    let (status, body) = put("1", changed.clone()).await;
    assert_eq!((status, body["keyVersion"].as_i64()), (StatusCode::OK, Some(2)));
    assert_eq!(put("1", changed.clone()).await.0, StatusCode::PRECONDITION_FAILED);
    let mut other_key = changed.clone();
    other_key["keyId"] = json!("AAAAAAAAAAAAAAAA");
    assert_eq!(put("2", other_key).await.1["error"], "key_mismatch");
    let mut bad = keyring.clone();
    bad["salt"] = json!("short");
    assert_eq!(t.api("PUT", "/v1/keyring", &token, Some(bad)).await.0, StatusCode::BAD_REQUEST);
    let (_, me) = t.api("GET", "/v1/me", &token, None).await;
    assert_eq!(me["keyring"]["keyVersion"], 2);
    assert_eq!(me["keyring"]["wrappedKey"], "e1.def");
}

#[tokio::test]
async fn last_writer_wins_and_paging() {
    let t = T::new();
    let alice = t.user("alice", false);
    let phone = t.pair(&alice, "Pixel").await;
    let laptop = t.pair(&alice, "Laptop").await;
    let now = now_ms();
    let change = |h: String, payload: &str| json!({"changes": [{"kind": "setting", "id": "polish.model", "hlc": h, "deleted": false, "payload": payload}]});

    let (_, r) = t.api("POST", "/v1/changes", &phone, Some(change(hlc(now, "dev_a"), "e1.one"))).await;
    assert_eq!(r["results"][0]["status"], "applied");
    // Older write from another device: stale, and the answer carries the winning record.
    let (_, r) = t.api("POST", "/v1/changes", &laptop, Some(change(hlc(now - 1000, "dev_b"), "e1.old"))).await;
    assert_eq!(r["results"][0]["status"], "stale");
    assert_eq!(r["results"][0]["current"]["payload"], "e1.one");
    // Newer write wins.
    let (_, r) = t.api("POST", "/v1/changes", &laptop, Some(change(hlc(now + 1000, "dev_b"), "e1.two"))).await;
    assert_eq!(r["results"][0]["status"], "applied");
    // Too far in the future: rejected.
    let (_, r) = t.api("POST", "/v1/changes", &laptop, Some(change(hlc(now + 3_600_000, "dev_b"), "e1.x"))).await;
    assert_eq!((r["results"][0]["status"].as_str(), r["results"][0]["error"].as_str()), (Some("rejected"), Some("clock_skew")));

    let bad = json!({"changes": [
        {"kind": "nope", "id": "a", "hlc": hlc(now, "dev_a"), "payload": "e1.x"},
        {"kind": "setting", "id": "bad id!", "hlc": hlc(now, "dev_a"), "payload": "e1.x"},
        {"kind": "setting", "id": "a", "hlc": "yesterday", "payload": "e1.x"},
        {"kind": "setting", "id": "a", "hlc": hlc(now, "dev_a")},
        {"kind": "dict", "id": "abcdefghijklmnopqrstuv", "hlc": hlc(now, "dev_a"), "payload": "plain text"},
    ]});
    let (_, r) = t.api("POST", "/v1/changes", &phone, Some(bad)).await;
    let errors: Vec<_> = r["results"].as_array().unwrap().iter().map(|x| x["error"].as_str().unwrap().to_string()).collect();
    assert_eq!(errors, ["invalid_kind", "invalid_id", "invalid_hlc", "missing_payload", "payload_too_large"]);

    // Two more records, then paging.
    let more = json!({"changes": [
        {"kind": "secret", "id": "openai", "hlc": hlc(now, "dev_a"), "payload": "e1.k"},
        {"kind": "dict", "id": "abcdefghijklmnopqrstuv", "hlc": hlc(now, "dev_a"), "deleted": true},
    ]});
    t.api("POST", "/v1/changes", &phone, Some(more)).await;
    let (_, page1) = t.api("GET", "/v1/changes?since=0&limit=2", &phone, None).await;
    assert_eq!(page1["changes"].as_array().unwrap().len(), 2);
    assert_eq!(page1["hasMore"], true);
    assert_eq!(page1["changes"][0]["payload"], "e1.two", "only the latest version of a record is in the feed");
    let since = page1["nextSince"].as_i64().unwrap();
    let (_, page2) = t.api("GET", &format!("/v1/changes?since={since}&limit=2"), &phone, None).await;
    assert_eq!(page2["hasMore"], false);
    let last = &page2["changes"][0];
    assert_eq!((last["kind"].as_str(), last["deleted"].as_bool(), last["payload"].is_null()), (Some("dict"), Some(true), true));
}

#[tokio::test]
async fn history_is_immutable_and_counted() {
    let t = T::new();
    let alice = t.user("alice", false);
    let phone = t.pair(&alice, "Pixel").await;
    let id = "0f8fad5b-d9cb-469f-a165-70867728950e";
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let entry = |deleted: bool| {
        json!({"changes": [{"kind": "history", "id": id, "hlc": hlc(now_ms(), "dev_a"), "deleted": deleted,
                             "payload": if deleted { Value::Null } else { json!("e1.entry") }, "stats": stats(&ts, 42)}]})
    };
    let (_, r) = t.api("POST", "/v1/changes", &phone, Some(entry(false))).await;
    assert_eq!(r["results"][0]["status"], "applied");
    let (_, r) = t.api("POST", "/v1/changes", &phone, Some(entry(false))).await;
    assert_eq!(r["results"][0]["status"], "exists");
    let (_, r) = t.api("POST", "/v1/changes", &phone, Some(json!({"changes": [{"kind": "history", "id": id, "hlc": hlc(now_ms(), "dev_a"), "payload": "e1.x"}]}))).await;
    assert_eq!(r["results"][0]["status"], "exists", "stats are not re-validated for an existing entry");
    let (_, r) = t.api("POST", "/v1/changes", &phone, Some(json!({"changes": [{"kind": "history", "id": "11111111-2222-3333-4444-555555555555", "hlc": hlc(now_ms(), "dev_a"), "payload": "e1.x"}]}))).await;
    assert_eq!(r["results"][0]["error"], "invalid_stats");

    let (_, feed) = t.api("GET", "/v1/changes", &phone, None).await;
    assert_eq!(feed["changes"][0]["stats"]["words"], 42);
    let (_, none) = t.api("GET", "/v1/changes?exclude=history", &phone, None).await;
    assert!(none["changes"].as_array().unwrap().is_empty());

    let (status, s) = t.api("GET", "/v1/stats", &phone, None).await;
    assert_eq!(status, StatusCode::OK);
    let month = &s["months"][0];
    assert_eq!((month["entries"].as_u64(), month["words"].as_u64()), (Some(1), Some(42)));
    assert!(month["totalUsd"].as_f64().unwrap() > 0.0, "recomputed from the price table");
    assert_eq!(s["devices"][0]["name"], "Pixel");

    let (_, r) = t.api("POST", "/v1/changes", &phone, Some(entry(true))).await;
    assert_eq!(r["results"][0]["status"], "applied");
    let (_, s) = t.api("GET", "/v1/stats", &phone, None).await;
    assert!(s["months"].as_array().unwrap().is_empty(), "deleted entries leave the statistics");
    assert_eq!(t.api("GET", "/v1/stats?from=2026-13&to=2026-01", &phone, None).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn users_are_isolated() {
    let t = T::new();
    let alice = t.user("alice", false);
    let bob = t.user("bob", false);
    let a = t.pair(&alice, "Alice phone").await;
    let b = t.pair(&bob, "Bob phone").await;
    let change = json!({"changes": [{"kind": "secret", "id": "openai", "hlc": hlc(now_ms(), "dev_a"), "payload": "e1.alice"}]});
    t.api("POST", "/v1/changes", &a, Some(change)).await;
    let (_, feed) = t.api("GET", "/v1/changes", &b, None).await;
    assert!(feed["changes"].as_array().unwrap().is_empty());
    // Same record id for Bob: an independent record, not a conflict with Alice's.
    let (_, r) = t.api("POST", "/v1/changes", &b, Some(json!({"changes": [{"kind": "secret", "id": "openai", "hlc": hlc(1, "dev_b"), "payload": "e1.bob"}]}))).await;
    assert_eq!(r["results"][0]["status"], "applied");
    let (_, feed) = t.api("GET", "/v1/changes", &a, None).await;
    assert_eq!(feed["changes"][0]["payload"], "e1.alice");
    let (_, s) = t.api("GET", "/v1/stats", &b, None).await;
    assert_eq!(s["devices"].as_array().unwrap().len(), 1);

    // Web: Bob can't revoke or rename Alice's device.
    let alice_device = db::list_devices(&t.state.db.lock(), &alice).unwrap()[0].id.clone();
    let bob_cookie = t.login("bob").await;
    let (_, html) = t.page("/devices", &bob_cookie).await;
    let token = csrf(&html);
    let (status, _, _) = t.form(&format!("/devices/{alice_device}/revoke"), Some(&bob_cookie), true, &[("csrf", &token)]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = t.form(&format!("/devices/{alice_device}/rename"), Some(&bob_cookie), true, &[("csrf", &token), ("name", "pwned")]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(t.api("GET", "/v1/me", &a, None).await.1["device"]["name"], "Alice phone");
    // Non-admins can't reach the admin pages.
    assert_eq!(t.page("/admin", &bob_cookie).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn revoked_devices_and_disabled_users_lose_access() {
    let t = T::new();
    let alice = t.user("alice", false);
    let token = t.pair(&alice, "Pixel").await;
    let other = t.pair(&alice, "Laptop").await;
    assert_eq!(t.api("DELETE", "/v1/device", &token, None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(t.api("GET", "/v1/me", &token, None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(t.api("GET", "/v1/me", &other, None).await.0, StatusCode::OK);
    db::set_disabled(&t.state.db.lock(), &alice, true).unwrap();
    assert_eq!(t.api("GET", "/v1/me", &other, None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn setup_login_csrf_and_invitations() {
    let t = T::new();
    let token = wisprcheap_server::web::new_setup_token(&t.state).unwrap().unwrap();
    let pw = "a long enough password";
    // Wrong token, then the real one.
    let (status, _, html) = t.form("/setup", None, true, &[("token", "nope"), ("username", "admin"), ("password", pw), ("confirm", pw)]).await;
    assert!(status == StatusCode::OK && html.contains("not valid"));
    let (status, headers, _) = t.form("/setup", None, true, &[("token", &token), ("username", "admin"), ("password", pw), ("confirm", pw)]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let admin_cookie = session_cookie(&headers).unwrap();
    // The setup link is single-use.
    let (_, _, html) = t.form("/setup", None, true, &[("token", &token), ("username", "admin2"), ("password", pw), ("confirm", pw)]).await;
    assert!(html.contains("not valid"));

    // Pairing needs the CSRF token and a same-origin request.
    let (_, html) = t.page("/devices", &admin_cookie).await;
    let csrf_token = csrf(&html);
    assert_eq!(t.form("/devices/pair", Some(&admin_cookie), true, &[("csrf", "wrong")]).await.0, StatusCode::FORBIDDEN);
    assert_eq!(t.form("/devices/pair", Some(&admin_cookie), false, &[("csrf", &csrf_token)]).await.0, StatusCode::FORBIDDEN);
    let (status, _, html) = t.form("/devices/pair", Some(&admin_cookie), true, &[("csrf", &csrf_token), ("name", "Phone")]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("<svg") && html.contains("wisprcheap sync pair http://localhost:8080 "));

    // Invitation → sign-up → the new user is logged in.
    let (_, html) = t.page("/admin", &admin_cookie).await;
    let (status, _, html) = t.form("/admin/invite", Some(&admin_cookie), true, &[("csrf", &csrf(&html))]).await;
    assert_eq!(status, StatusCode::OK);
    let start = html.find("/invite/").unwrap();
    let path = html[start..start + html[start..].find('<').unwrap()].to_string();
    let (status, _, html) = t.form(&path, None, true, &[("username", "carol"), ("password", "short"), ("confirm", "short")]).await;
    assert!(status == StatusCode::OK && html.contains("at least 12"));
    let (status, headers, _) = t.form(&path, None, true, &[("username", "carol"), ("password", pw), ("confirm", pw)]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(session_cookie(&headers).is_some());
    let (_, _, html) = t.form(&path, None, true, &[("username", "dave"), ("password", pw), ("confirm", pw)]).await;
    assert!(html.contains("not valid"), "invitation links work once");
    assert!(db::user_by_name(&t.state.db.lock(), "carol").unwrap().is_some());
}

#[tokio::test]
async fn login_throttling_and_logout() {
    let t = T::new();
    t.user("alice", false);
    for _ in 0..5 {
        let (_, _, html) = t.form("/login", None, true, &[("username", "alice"), ("password", "wrong password!")]).await;
        assert!(html.contains("Wrong username or password"));
    }
    let (_, _, html) = t.form("/login", None, true, &[("username", "alice"), ("password", "a long enough password")]).await;
    assert!(html.contains("Too many failed attempts"), "blocked even with the right password");
    let (_, _, html) = t.form("/login", None, true, &[("username", "ghost"), ("password", "whatever it is")]).await;
    assert!(html.contains("Too many failed attempts") || html.contains("Wrong username"));

    let t = T::new();
    t.user("bob", false);
    let cookie = t.login("bob").await;
    let (status, html) = t.page("/", &cookie).await;
    assert!(status == StatusCode::OK && html.contains("Last 12 months"));
    let token = csrf(&html);
    let (status, headers, _) = t.form("/logout", Some(&cookie), true, &[("csrf", &token)]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(session_cookie(&headers).is_none());
    let (_, _, text) = t.call("GET", "/", &[("cookie", &format!("wcs_session={cookie}"))], Body::empty()).await;
    assert!(text.is_empty(), "redirected to the login page");
}

#[tokio::test]
async fn security_headers_and_health() {
    let t = T::new();
    let (status, headers, body) = t.call("GET", "/healthz", &[], Body::empty()).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "ok"));
    assert!(headers.get("content-security-policy").unwrap().to_str().unwrap().contains("default-src 'self'"));
    assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
    // Not `no-referrer`: with it, browsers send `Origin: null` on our own form POSTs.
    assert_eq!(headers.get("referrer-policy").unwrap(), "same-origin");
}

/// What browsers really send when they hide the origin: `Origin: null` + `Sec-Fetch-Site`.
#[tokio::test]
async fn null_origin_from_the_same_site_is_accepted() {
    let t = T::new();
    let token = wisprcheap_server::web::new_setup_token(&t.state).unwrap().unwrap();
    let pw = "a long enough password";
    let body = format!("token={token}&username=admin&password={0}&confirm={0}", urlencode(pw));
    let post = |fetch_site: &'static str| {
        let body = body.clone();
        let t = &t;
        async move {
            let mut headers = vec![("content-type", "application/x-www-form-urlencoded"), ("origin", "null")];
            if !fetch_site.is_empty() {
                headers.push(("sec-fetch-site", fetch_site));
            }
            t.call("POST", "/setup", &headers, Body::from(body)).await.0
        }
    };
    assert_eq!(post("").await, StatusCode::FORBIDDEN);
    assert_eq!(post("cross-site").await, StatusCode::FORBIDDEN);
    assert_eq!(post("same-origin").await, StatusCode::SEE_OTHER);
    // A real but different origin is still refused.
    let (status, _, _) = t
        .call(
            "POST",
            "/login",
            &[("content-type", "application/x-www-form-urlencoded"), ("origin", "https://evil.example"), ("sec-fetch-site", "same-origin")],
            Body::from(format!("username=admin&password={}", urlencode(pw))),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn renaming_a_user_keeps_devices_and_sessions() {
    let t = T::new();
    let id = t.user("admin", true);
    t.user("bob", false);
    let token = t.pair(&id, "Phone").await;
    let cookie = t.login("admin").await;
    {
        let conn = t.state.db.lock();
        assert!(db::rename_user(&conn, "admin", "bob").is_err(), "taken");
        assert!(db::rename_user(&conn, "admin", "no spaces").is_err(), "invalid");
        assert!(db::rename_user(&conn, "nobody", "x1").is_err(), "unknown");
        assert_eq!(db::rename_user(&conn, "ADMIN", "Hexalyse").unwrap(), id);
    }
    let (status, me) = t.api("GET", "/v1/me", &token, None).await;
    assert_eq!((status, me["user"]["username"].as_str()), (StatusCode::OK, Some("Hexalyse")));
    let (status, html) = t.page("/devices", &cookie).await;
    assert!(status == StatusCode::OK && html.contains("Hexalyse"));
    t.login("hexalyse").await;
    let (status, _, _) = t.form("/login", None, true, &[("username", "admin"), ("password", "a long enough password")]).await;
    assert_ne!(status, StatusCode::SEE_OTHER);
}