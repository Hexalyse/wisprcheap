//! Web UI: login, first-admin setup, invitations, dashboard, history, devices (pairing), account, admin.
//! Server-rendered HTML (Askama, auto-escaped), no JavaScript; every form is POST with a CSRF token and an
//! Origin check.

use std::collections::BTreeMap;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;

use crate::auth::{self, ClientIp, check_password_rules, check_username, dummy_hash, hash_password, verify_password};
use crate::db::{self, Session};
use crate::util::{b64, format_pairing_code, iso_from_s, random_bytes, sha256, url_encode};
use crate::{SharedState, VERSION, stats};

const COOKIE: &str = "wcs_session";
const SETUP_KEY: &str = "setup_token_hash";

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/", get(dashboard))
        .route("/static/style.css", get(css))
        .route("/login", get(login_page).post(login))
        .route("/logout", post(logout))
        .route("/setup", get(setup_page).post(setup))
        .route("/invite/{code}", get(invite_page).post(invite_submit))
        .route("/history", get(history))
        .route("/devices", get(devices))
        .route("/devices/pair", post(pair_device))
        .route("/devices/{id}/revoke", post(revoke_device))
        .route("/devices/{id}/rename", post(rename_device))
        .route("/account", get(account))
        .route("/account/password", post(change_password))
        .route("/account/signout-all", post(signout_all))
        .route("/account/reset-encryption", post(reset_encryption))
        .route("/account/delete", post(delete_account))
        .route("/admin", get(admin))
        .route("/admin/invite", post(admin_invite))
        .route("/admin/users/{id}/{action}", post(admin_user_action))
}

// --- Errors and helpers ---

pub struct WebError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for WebError {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        tracing::error!("web: {:#}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response()
    }
}

type WebResult = Result<Response, WebError>;

fn render(t: &impl Template) -> WebResult {
    Ok(Html(t.render()?).into_response())
}

pub struct Nav {
    pub username: String,
    pub is_admin: bool,
    pub csrf: String,
}

fn nav(s: &Session) -> Option<Nav> {
    Some(Nav { username: s.user.username.clone(), is_admin: s.user.is_admin, csrf: s.csrf.clone() })
}

fn current_session(state: &SharedState, headers: &HeaderMap) -> anyhow::Result<Option<Session>> {
    let Some(cookie) = auth::cookie(headers, COOKIE) else { return Ok(None) };
    let conn = state.db.lock();
    db::session(&conn, &cookie)
}

fn to_login() -> Response {
    Redirect::to("/login").into_response()
}

fn origin_of(url: &str) -> &str {
    let after_scheme = url.find("://").map(|i| i + 3).unwrap_or(0);
    match url[after_scheme..].find('/') {
        Some(i) => &url[..after_scheme + i],
        None => url,
    }
}

/// POSTs must come from our own pages: same Origin (or Referer) as the public URL.
fn same_origin(state: &SharedState, headers: &HeaderMap) -> bool {
    let expected = origin_of(&state.config.public_url);
    if let Some(origin) = headers.get(header::ORIGIN) {
        return origin.to_str().is_ok_and(|o| o == expected);
    }
    headers
        .get(header::REFERER)
        .and_then(|r| r.to_str().ok())
        .is_some_and(|r| r == expected || r.starts_with(&format!("{expected}/")))
}

fn forbidden(message: &str) -> Response {
    (StatusCode::FORBIDDEN, message.to_string()).into_response()
}

/// Origin + CSRF check for a logged-in POST.
fn check_form(state: &SharedState, headers: &HeaderMap, session: &Session, csrf: &str) -> Option<Response> {
    if !same_origin(state, headers) {
        return Some(forbidden("Cross-site request refused. Open the server at its public URL."));
    }
    if csrf != session.csrf {
        return Some(forbidden("Expired form: go back, reload the page and try again."));
    }
    None
}

fn session_cookie(state: &SharedState, value: &str, max_age: i64) -> HeaderValue {
    let secure = if state.config.is_https() { "; Secure" } else { "" };
    HeaderValue::from_str(&format!("{COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"))
        .expect("cookie header")
}

fn logged_in(state: &SharedState, cookie: &str, to: &str) -> Response {
    let mut res = Redirect::to(to).into_response();
    res.headers_mut().insert(header::SET_COOKIE, session_cookie(state, cookie, db::SESSION_MAX_AGE_S));
    res
}

fn usd(v: f64) -> String {
    if v.abs() < 1.0 { format!("${v:.4}") } else { format!("${v:.2}") }
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

async fn verify(password: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || verify_password(&password, &hash)).await.unwrap_or(false)
}

async fn hash(password: String) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || hash_password(&password)).await?
}

#[derive(Template)]
#[template(path = "message.html")]
struct MessagePage {
    nav: Option<Nav>,
    version: &'static str,
    title: String,
    lines: Vec<String>,
    copy: Option<String>,
    link_href: Option<String>,
    link_label: String,
}

fn message(nav: Option<Nav>, title: &str, lines: &[&str], copy: Option<String>, link: Option<(&str, &str)>) -> WebResult {
    render(&MessagePage {
        nav,
        version: VERSION,
        title: title.into(),
        lines: lines.iter().map(|l| l.to_string()).collect(),
        copy,
        link_href: link.map(|l| l.0.to_string()),
        link_label: link.map(|l| l.1.to_string()).unwrap_or_default(),
    })
}

async fn css() -> Response {
    let mut res = include_str!("../static/style.css").into_response();
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/css; charset=utf-8"));
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600"));
    res
}

// --- Login ---

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    nav: Option<Nav>,
    version: &'static str,
    error: Option<String>,
    username: String,
}

async fn login_page(State(state): State<SharedState>, headers: HeaderMap) -> WebResult {
    if current_session(&state, &headers)?.is_some() {
        return Ok(Redirect::to("/").into_response());
    }
    if db::user_count(&state.db.lock())? == 0 {
        return message(None, "Set up the server", &["No account exists yet. Open the setup link printed in the server log (or run `wisprcheap-server admin create <username>`)."], None, None);
    }
    render(&LoginPage { nav: None, version: VERSION, error: None, username: String::new() })
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: String,
}

async fn login(State(state): State<SharedState>, ClientIp(ip): ClientIp, headers: HeaderMap, Form(f): Form<LoginForm>) -> WebResult {
    if !same_origin(&state, &headers) {
        return Ok(forbidden("Cross-site request refused. Open the server at its public URL."));
    }
    let name = f.username.trim().to_string();
    let keys = [format!("login-ip:{ip}"), format!("login-user:{}", name.to_lowercase())];
    let page = |error: String| LoginPage { nav: None, version: VERSION, error: Some(error), username: name.clone() };
    if let Some(wait) = keys.iter().filter_map(|k| state.limiter.login_blocked(k)).max() {
        return render(&page(format!("Too many failed attempts. Try again in {} s.", wait.as_secs() + 1)));
    }
    let user = db::user_by_name(&state.db.lock(), &name)?;
    let stored = user.as_ref().map(|u| u.password_hash.clone()).unwrap_or_else(|| dummy_hash().to_string());
    let ok = verify(f.password, stored).await;
    let conn = state.db.lock();
    match user {
        Some(u) if ok && !u.disabled => {
            keys.iter().for_each(|k| state.limiter.login_succeeded(k));
            let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
            let cookie = db::create_session(&conn, &u.id, &ip, ua)?;
            db::audit(&conn, Some(&u.id), None, "login", None, &ip);
            Ok(logged_in(&state, &cookie, "/"))
        }
        other => {
            keys.iter().for_each(|k| state.limiter.login_failed(k));
            db::audit(&conn, other.as_ref().map(|u| u.id.as_str()), None, "login_failed", Some(&name), &ip);
            let error = if other.is_some_and(|u| u.disabled && ok) { "This account is disabled." } else { "Wrong username or password." };
            render(&page(error.into()))
        }
    }
}

#[derive(Deserialize)]
struct CsrfForm {
    csrf: String,
}

async fn logout(State(state): State<SharedState>, headers: HeaderMap, Form(f): Form<CsrfForm>) -> WebResult {
    if let Some(s) = current_session(&state, &headers)? {
        if let Some(denied) = check_form(&state, &headers, &s, &f.csrf) {
            return Ok(denied);
        }
        db::delete_session(&state.db.lock(), &s.id_hash)?;
    }
    let mut res = to_login();
    res.headers_mut().insert(header::SET_COOKIE, session_cookie(&state, "", 0));
    Ok(res)
}

// --- First admin (setup link) and invitations ---

#[derive(Template)]
#[template(path = "password_form.html")]
struct PasswordFormPage {
    nav: Option<Nav>,
    version: &'static str,
    title: String,
    intro: String,
    error: Option<String>,
    action: String,
    token: Option<String>,
    ask_username: bool,
    username: String,
    button: String,
}

/// Creates a new one-time setup token when the server has no user yet (printed in the log by `serve`).
pub fn new_setup_token(state: &SharedState) -> anyhow::Result<Option<String>> {
    let conn = state.db.lock();
    if db::user_count(&conn)? > 0 {
        db::meta_delete(&conn, SETUP_KEY)?;
        return Ok(None);
    }
    let token = b64(&random_bytes::<24>());
    db::meta_set(&conn, SETUP_KEY, &b64(&sha256(token.as_bytes())))?;
    Ok(Some(token))
}

fn setup_token_valid(state: &SharedState, token: &str) -> anyhow::Result<bool> {
    let conn = state.db.lock();
    Ok(db::user_count(&conn)? == 0 && db::meta_get(&conn, SETUP_KEY)?.is_some_and(|h| h == b64(&sha256(token.as_bytes()))))
}

fn setup_form(token: &str, error: Option<String>, username: String) -> WebResult {
    render(&PasswordFormPage {
        nav: None,
        version: VERSION,
        title: "Create the admin account".into(),
        intro: "This account manages the server: it invites the other users.".into(),
        error,
        action: "/setup".into(),
        token: Some(token.into()),
        ask_username: true,
        username,
        button: "Create the account".into(),
    })
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

async fn setup_page(State(state): State<SharedState>, Query(q): Query<TokenQuery>) -> WebResult {
    match q.token {
        Some(token) if setup_token_valid(&state, &token)? => setup_form(&token, None, String::new()),
        _ => message(None, "Setup link not valid", &["This link was already used or is outdated. Restart the server to print a new one, or log in."], None, Some(("/login", "Log in"))),
    }
}

#[derive(Deserialize)]
struct AccountForm {
    token: Option<String>,
    username: Option<String>,
    password: String,
    confirm: String,
}

fn check_new_password(username: &str, password: &str, confirm: &str) -> Result<(), String> {
    if password != confirm {
        return Err("The two passwords differ.".into());
    }
    check_password_rules(username, password)
}

async fn setup(State(state): State<SharedState>, ClientIp(ip): ClientIp, headers: HeaderMap, Form(f): Form<AccountForm>) -> WebResult {
    if !same_origin(&state, &headers) {
        return Ok(forbidden("Cross-site request refused."));
    }
    let token = f.token.unwrap_or_default();
    if !setup_token_valid(&state, &token)? {
        return message(None, "Setup link not valid", &["This link was already used or is outdated."], None, Some(("/login", "Log in")));
    }
    let username = f.username.unwrap_or_default().trim().to_string();
    if let Err(e) = check_username(&username).and_then(|_| check_new_password(&username, &f.password, &f.confirm)) {
        return setup_form(&token, Some(e), username);
    }
    let hashed = hash(f.password).await?;
    let conn = state.db.lock();
    let id = db::create_user(&conn, &username, &hashed, true)?;
    db::meta_delete(&conn, SETUP_KEY)?;
    db::audit(&conn, Some(&id), None, "admin_created", Some(&username), &ip);
    let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
    let cookie = db::create_session(&conn, &id, &ip, ua)?;
    Ok(logged_in(&state, &cookie, "/"))
}

fn invite_form(code: &str, reset_for: Option<String>, error: Option<String>, username: String) -> WebResult {
    let reset = reset_for.is_some();
    render(&PasswordFormPage {
        nav: None,
        version: VERSION,
        title: if reset { "Choose a new password".into() } else { "Create your account".into() },
        intro: match reset_for {
            Some(user) => format!("New password for {user}. Your devices stay paired."),
            None => "You were invited to this WisprCheap sync server. Choose a username and a password (12 characters or more).".into(),
        },
        error,
        action: format!("/invite/{code}"),
        token: None,
        ask_username: !reset,
        username,
        button: if reset { "Set the password".into() } else { "Create the account".into() },
    })
}

async fn invite_page(State(state): State<SharedState>, Path(code): Path<String>) -> WebResult {
    let conn = state.db.lock();
    let Some(inv) = db::invite(&conn, &code)? else {
        return message(None, "Link not valid", &["This link was already used or has expired. Ask the admin for a new one."], None, None);
    };
    let reset_for = match inv.user_id {
        Some(id) if inv.purpose == db::INVITE_RESET => db::user_by_id(&conn, &id)?.map(|u| u.username),
        _ => None,
    };
    drop(conn);
    invite_form(&code, reset_for, None, String::new())
}

async fn invite_submit(
    State(state): State<SharedState>,
    ClientIp(ip): ClientIp,
    Path(code): Path<String>,
    headers: HeaderMap,
    Form(f): Form<AccountForm>,
) -> WebResult {
    if !same_origin(&state, &headers) {
        return Ok(forbidden("Cross-site request refused."));
    }
    let inv = db::invite(&state.db.lock(), &code)?;
    let Some(inv) = inv else {
        return message(None, "Link not valid", &["This link was already used or has expired."], None, None);
    };
    if inv.purpose == db::INVITE_RESET {
        let Some(user) = inv.user_id.as_deref().map(|id| db::user_by_id(&state.db.lock(), id)).transpose()?.flatten() else {
            return message(None, "Link not valid", &["The account no longer exists."], None, None);
        };
        if let Err(e) = check_new_password(&user.username, &f.password, &f.confirm) {
            return invite_form(&code, Some(user.username), Some(e), String::new());
        }
        let hashed = hash(f.password).await?;
        let conn = state.db.lock();
        db::set_password(&conn, &user.id, &hashed)?;
        db::delete_user_sessions(&conn, &user.id)?;
        db::use_invite(&conn, &inv.code_hash)?;
        db::audit(&conn, Some(&user.id), None, "password_reset", None, &ip);
        return message(None, "Password changed", &["You can now log in with the new password."], None, Some(("/login", "Log in")));
    }
    let username = f.username.unwrap_or_default().trim().to_string();
    if let Err(e) = check_username(&username).and_then(|_| check_new_password(&username, &f.password, &f.confirm)) {
        return invite_form(&code, None, Some(e), username);
    }
    if db::user_by_name(&state.db.lock(), &username)?.is_some() {
        return invite_form(&code, None, Some("This username is taken.".into()), username);
    }
    let hashed = hash(f.password).await?;
    let conn = state.db.lock();
    let id = db::create_user(&conn, &username, &hashed, false)?;
    db::use_invite(&conn, &inv.code_hash)?;
    db::audit(&conn, Some(&id), None, "user_created", Some(&username), &ip);
    let ua = headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("");
    let cookie = db::create_session(&conn, &id, &ip, ua)?;
    Ok(logged_in(&state, &cookie, "/devices"))
}

// --- Dashboard and history ---

struct Current {
    label: String,
    total: String,
    words: String,
    entries: u64,
    commands: u64,
    audio: String,
}

struct MonthRow {
    month: String,
    entries: u64,
    words: String,
    audio: String,
    stt: String,
    llm: String,
    total: String,
    per10k: String,
}

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardPage {
    nav: Option<Nav>,
    version: &'static str,
    current: Current,
    devices: Vec<(String, u64, String, String)>,
    models: Vec<(String, String, u64, String)>,
    months: Vec<MonthRow>,
    unknown: u64,
    chart: String,
}

/// Monthly totals as an SVG bar chart (numbers and month labels only, so it's safe to embed).
fn chart(months: &[wisprcheap_sync::protocol::MonthStats]) -> String {
    let data: Vec<_> = months.iter().rev().collect();
    let max = data.iter().map(|m| m.total_usd).fold(0.0f64, f64::max).max(1e-9);
    let (w, h, gap) = (720.0, 150.0, 8.0);
    let bar = (w - gap * (data.len() as f64 + 1.0)) / data.len().max(1) as f64;
    let mut svg = format!(r#"<svg viewBox="0 0 {w} {}" role="img" aria-label="Cost per month">"#, h + 20.0);
    for (i, m) in data.iter().enumerate() {
        let bh = (m.total_usd / max * (h - 16.0)).max(2.0);
        let x = gap + i as f64 * (bar + gap);
        svg += &format!(r#"<rect class="bar" x="{x:.1}" y="{:.1}" width="{bar:.1}" height="{bh:.1}" rx="4"></rect>"#, h - bh);
        svg += &format!(r#"<text x="{:.1}" y="{:.1}" text-anchor="middle">{}</text>"#, x + bar / 2.0, h - bh - 4.0, usd(m.total_usd));
        let label: String = m.month.chars().filter(|c| c.is_ascii_digit() || *c == '-').collect();
        svg += &format!(r#"<text x="{:.1}" y="{:.1}" text-anchor="middle">{label}</text>"#, x + bar / 2.0, h + 16.0);
    }
    svg + "</svg>"
}

async fn dashboard(State(state): State<SharedState>, headers: HeaderMap) -> WebResult {
    let Some(s) = current_session(&state, &headers)? else {
        if db::user_count(&state.db.lock())? == 0 {
            return message(None, "Set up the server", &["No account exists yet. Open the setup link printed in the server log (or run `wisprcheap-server admin create <username>`)."], None, None);
        }
        return Ok(to_login());
    };
    let (from, to) = crate::api::default_range(0);
    let conn = state.db.lock();
    let start = stats::month_start_iso(&from, 0).unwrap_or_default();
    let end = stats::next_month(&to).and_then(|m| stats::month_start_iso(&m, 0)).unwrap_or_default();
    let rows = stats::rows(&conn, &s.user.id, &start, &end, 1_000_000)?;
    let months = stats::aggregate(&rows, 0);
    let names: BTreeMap<String, String> = db::list_devices(&conn, &s.user.id)?.into_iter().map(|d| (d.id, d.name)).collect();
    drop(conn);
    let cur = months.iter().find(|m| m.month == to).cloned().unwrap_or_default();
    let label = chrono::NaiveDate::parse_from_str(&format!("{to}-01"), "%Y-%m-%d").map(|d| d.format("%B %Y").to_string()).unwrap_or(to.clone());
    let devices = cur
        .by_device
        .iter()
        .map(|d| (names.get(&d.device).cloned().unwrap_or_else(|| "(removed device)".into()), d.entries, thousands(d.words), usd(d.total_usd)))
        .collect();
    let mut models: BTreeMap<(String, &'static str), (u64, f64)> = BTreeMap::new();
    for r in rows.iter().filter(|r| stats::month_of(&r.stats.ts, 0).as_deref() == Some(to.as_str())) {
        let e = models.entry((r.stats.stt_model.clone(), "speech-to-text")).or_default();
        e.0 += 1;
        e.1 += r.stt();
        if let Some(m) = &r.stats.llm_model {
            let e = models.entry((m.clone(), if r.stats.mode == "command" { "command" } else { "cleanup / translation" })).or_default();
            e.0 += 1;
            e.1 += r.llm();
        }
    }
    let unknown = months.iter().map(|m| m.unknown_price).sum();
    render(&DashboardPage {
        nav: nav(&s),
        version: VERSION,
        current: Current {
            label,
            total: usd(cur.total_usd),
            words: thousands(cur.words),
            entries: cur.entries,
            commands: cur.commands,
            audio: format!("{:.1}", cur.audio_minutes),
        },
        devices,
        models: models.into_iter().map(|((m, use_), (n, c))| (m, use_.to_string(), n, usd(c))).collect(),
        chart: chart(&months),
        months: months
            .iter()
            .map(|m| MonthRow {
                month: m.month.clone(),
                entries: m.entries,
                words: thousands(m.words),
                audio: format!("{:.1}", m.audio_minutes),
                stt: usd(m.stt_usd),
                llm: usd(m.llm_usd),
                total: usd(m.total_usd),
                per10k: if m.words > 0 { usd(m.total_usd / m.words as f64 * 10_000.0) } else { "—".into() },
            })
            .collect(),
        unknown,
    })
}

struct HistRow {
    time: String,
    device: String,
    mode: String,
    audio: String,
    words: u64,
    models: String,
    cost: String,
    status: String,
}

#[derive(Template)]
#[template(path = "history.html")]
struct HistoryPage {
    nav: Option<Nav>,
    version: &'static str,
    rows: Vec<HistRow>,
    older: Option<String>,
}

#[derive(Deserialize)]
struct BeforeQuery {
    before: Option<String>,
}

async fn history(State(state): State<SharedState>, headers: HeaderMap, Query(q): Query<BeforeQuery>) -> WebResult {
    let Some(s) = current_session(&state, &headers)? else { return Ok(to_login()) };
    const PAGE: i64 = 100;
    let conn = state.db.lock();
    let before = q.before.filter(|b| b.len() <= 40).unwrap_or_else(|| "9999".into());
    let rows = stats::rows(&conn, &s.user.id, "", &before, PAGE)?;
    let names: BTreeMap<String, String> = db::list_devices(&conn, &s.user.id)?.into_iter().map(|d| (d.id, d.name)).collect();
    drop(conn);
    let older = (rows.len() as i64 == PAGE).then(|| rows.last().map(|r| url_encode(&r.stats.ts))).flatten();
    let rows = rows
        .iter()
        .map(|r| HistRow {
            time: r.stats.ts.get(..19).unwrap_or(&r.stats.ts).replace('T', " "),
            device: r.device_id.as_ref().and_then(|d| names.get(d)).cloned().unwrap_or_else(|| "—".into()),
            mode: r.stats.mode.clone(),
            audio: format!("{:.1} s", r.stats.duration_sec),
            words: r.stats.words,
            models: [Some(r.stats.stt_model.as_str()), r.stats.llm_model.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" + "),
            cost: usd(r.total()),
            status: r.stats.status.clone(),
        })
        .collect();
    render(&HistoryPage { nav: nav(&s), version: VERSION, rows, older })
}

// --- Devices ---

struct DevRow {
    id: String,
    name: String,
    platform: String,
    version: String,
    paired: String,
    last_seen: String,
    revoked: bool,
    revoked_at: String,
}

#[derive(Template)]
#[template(path = "devices.html")]
struct DevicesPage {
    nav: Option<Nav>,
    version: &'static str,
    csrf: String,
    public_url: String,
    devices: Vec<DevRow>,
}

async fn devices(State(state): State<SharedState>, headers: HeaderMap) -> WebResult {
    let Some(s) = current_session(&state, &headers)? else { return Ok(to_login()) };
    let list = db::list_devices(&state.db.lock(), &s.user.id)?;
    render(&DevicesPage {
        nav: nav(&s),
        version: VERSION,
        csrf: s.csrf.clone(),
        public_url: state.config.public_url.clone(),
        devices: list
            .into_iter()
            .map(|d| DevRow {
                id: d.id,
                name: d.name,
                platform: d.platform,
                version: d.app_version,
                paired: iso_from_s(d.created_at),
                last_seen: d.last_seen_at.map(|t| format!("{} ({})", iso_from_s(t), d.last_ip.unwrap_or_default())).unwrap_or_else(|| "never".into()),
                revoked: d.revoked_at.is_some(),
                revoked_at: d.revoked_at.map(iso_from_s).unwrap_or_default(),
            })
            .collect(),
    })
}

#[derive(Template)]
#[template(path = "pair.html")]
struct PairPage {
    nav: Option<Nav>,
    version: &'static str,
    qr: String,
    code: String,
    expires: String,
    public_url: String,
}

#[derive(Deserialize)]
struct NameForm {
    csrf: String,
    #[serde(default)]
    name: String,
}

async fn pair_device(State(state): State<SharedState>, ClientIp(ip): ClientIp, headers: HeaderMap, Form(f): Form<NameForm>) -> WebResult {
    let Some(s) = current_session(&state, &headers)? else { return Ok(to_login()) };
    if let Some(denied) = check_form(&state, &headers, &s, &f.csrf) {
        return Ok(denied);
    }
    let name = f.name.trim().chars().take(60).collect::<String>();
    let conn = state.db.lock();
    let (code, expires) = db::create_pairing_code(&conn, &s.user.id, (!name.is_empty()).then_some(name.as_str()))?;
    db::audit(&conn, Some(&s.user.id), None, "pairing_code_created", (!name.is_empty()).then_some(name.as_str()), &ip);
    drop(conn);
    let link = format!("wisprcheap://pair?server={}&code={code}", url_encode(&state.config.public_url));
    let qr = qrcode::QrCode::new(link.as_bytes())?
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(240, 240)
        .quiet_zone(false)
        .build();
    let mut res = render(&PairPage {
        nav: nav(&s),
        version: VERSION,
        qr,
        code: format_pairing_code(&code),
        expires: iso_from_s(expires),
        public_url: state.config.public_url.clone(),
    })?;
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

async fn revoke_device(
    State(state): State<SharedState>,
    ClientIp(ip): ClientIp,
    Path(id): Path<String>,
    headers: HeaderMap,
    Form(f): Form<CsrfForm>,
) -> WebResult {
    let Some(s) = current_session(&state, &headers)? else { return Ok(to_login()) };
    if let Some(denied) = check_form(&state, &headers, &s, &f.csrf) {
        return Ok(denied);
    }
    let conn = state.db.lock();
    if !db::revoke_device(&conn, &s.user.id, &id)? {
        return Ok((StatusCode::NOT_FOUND, "No such device").into_response());
    }
    db::audit(&conn, Some(&s.user.id), Some(&id), "device_revoked", None, &ip);
    Ok(Redirect::to("/devices").into_response())
}

async fn rename_device(State(state): State<SharedState>, Path(id): Path<String>, headers: HeaderMap, Form(f): Form<NameForm>) -> WebResult {
    let Some(s) = current_session(&state, &headers)? else { return Ok(to_login()) };
    if let Some(denied) = check_form(&state, &headers, &s, &f.csrf) {
        return Ok(denied);
    }
    let name = f.name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Ok((StatusCode::BAD_REQUEST, "Names have 1 to 60 characters").into_response());
    }
    if !db::rename_device(&state.db.lock(), &s.user.id, &id, name)? {
        return Ok((StatusCode::NOT_FOUND, "No such device").into_response());
    }
    Ok(Redirect::to("/devices").into_response())
}

// --- Account ---

#[derive(Template)]
#[template(path = "account.html")]
struct AccountPage {
    nav: Option<Nav>,
    version: &'static str,
    csrf: String,
    notice: Option<String>,
    error: Option<String>,
    encryption: String,
    audit: Vec<(String, String, String, String, String, String)>,
}

fn audit_rows(entries: Vec<db::AuditEntry>) -> Vec<(String, String, String, String, String, String)> {
    entries
        .into_iter()
        .map(|a| (iso_from_s(a.at), a.username.unwrap_or_default(), a.event, a.detail.unwrap_or_default(), a.device.unwrap_or_default(), a.ip.unwrap_or_default()))
        .collect()
}

fn account_page(state: &SharedState, s: &Session, notice: Option<&str>, error: Option<&str>) -> WebResult {
    let conn = state.db.lock();
    let encryption = match db::keyring(&conn, &s.user.id)? {
        Some(k) => format!("End-to-end encryption is set up (passphrase version {}, key {}).", k.key_version, k.key_id),
        None => "Not set up yet: the first paired device asks for a sync passphrase and creates it.".into(),
    };
    let audit = audit_rows(db::audit_list(&conn, Some(&s.user.id), 50)?);
    drop(conn);
    render(&AccountPage {
        nav: nav(s),
        version: VERSION,
        csrf: s.csrf.clone(),
        notice: notice.map(String::from),
        error: error.map(String::from),
        encryption,
        audit,
    })
}

async fn account(State(state): State<SharedState>, headers: HeaderMap) -> WebResult {
    let Some(s) = current_session(&state, &headers)? else { return Ok(to_login()) };
    account_page(&state, &s, None, None)
}

#[derive(Deserialize)]
struct PasswordChangeForm {
    csrf: String,
    current: String,
    password: String,
    confirm: String,
}

async fn change_password(State(state): State<SharedState>, ClientIp(ip): ClientIp, headers: HeaderMap, Form(f): Form<PasswordChangeForm>) -> WebResult {
    let Some(s) = current_session(&state, &headers)? else { return Ok(to_login()) };
    if let Some(denied) = check_form(&state, &headers, &s, &f.csrf) {
        return Ok(denied);
    }
    if !verify(f.current, s.user.password_hash.clone()).await {
        return account_page(&state, &s, None, Some("The current password is wrong."));
    }
    if let Err(e) = check_new_password(&s.user.username, &f.password, &f.confirm) {
        return account_page(&state, &s, None, Some(&e));
    }
    let hashed = hash(f.password).await?;
    {
        let conn = state.db.lock();
        db::set_password(&conn, &s.user.id, &hashed)?;
        conn.execute("DELETE FROM sessions WHERE user_id = ?1 AND id_hash != ?2", rusqlite::params![s.user.id, s.id_hash])?;
        db::audit(&conn, Some(&s.user.id), None, "password_changed", None, &ip);
    }
    account_page(&state, &s, Some("Password changed. Other browsers were signed out."), None)
}

async fn signout_all(State(state): State<SharedState>, headers: HeaderMap, Form(f): Form<CsrfForm>) -> WebResult {
    let Some(s) = current_session(&state, &headers)? else { return Ok(to_login()) };
    if let Some(denied) = check_form(&state, &headers, &s, &f.csrf) {
        return Ok(denied);
    }
    db::delete_user_sessions(&state.db.lock(), &s.user.id)?;
    let mut res = to_login();
    res.headers_mut().insert(header::SET_COOKIE, session_cookie(&state, "", 0));
    Ok(res)
}

#[derive(Deserialize)]
struct ConfirmForm {
    csrf: String,
    #[serde(default)]
    password: String,
    confirm: String,
}

async fn reset_encryption(State(state): State<SharedState>, ClientIp(ip): ClientIp, headers: HeaderMap, Form(f): Form<ConfirmForm>) -> WebResult {
    let Some(s) = current_session(&state, &headers)? else { return Ok(to_login()) };
    if let Some(denied) = check_form(&state, &headers, &s, &f.csrf) {
        return Ok(denied);
    }
    if f.confirm != "RESET" || !verify(f.password, s.user.password_hash.clone()).await {
        return account_page(&state, &s, None, Some("Type RESET and your password to reset the encryption."));
    }
    {
        let conn = state.db.lock();
        db::reset_encryption(&conn, &s.user.id)?;
        db::audit(&conn, Some(&s.user.id), None, "encryption_reset", None, &ip);
    }
    account_page(&state, &s, Some("Encryption reset. Your devices will ask for a new sync passphrase and upload their data again."), None)
}

async fn delete_account(State(state): State<SharedState>, ClientIp(ip): ClientIp, headers: HeaderMap, Form(f): Form<ConfirmForm>) -> WebResult {
    let Some(s) = current_session(&state, &headers)? else { return Ok(to_login()) };
    if let Some(denied) = check_form(&state, &headers, &s, &f.csrf) {
        return Ok(denied);
    }
    if f.confirm != "DELETE" || !verify(f.password, s.user.password_hash.clone()).await {
        return account_page(&state, &s, None, Some("Type DELETE and your password to delete the account."));
    }
    let conn = state.db.lock();
    if s.user.is_admin {
        let admins: i64 = conn.query_row("SELECT COUNT(*) FROM users WHERE is_admin = 1 AND disabled = 0", [], |r| r.get(0))?;
        if admins <= 1 {
            drop(conn);
            return account_page(&state, &s, None, Some("You are the only admin: the account can't be deleted."));
        }
    }
    db::delete_user(&conn, &s.user.id)?;
    db::audit(&conn, None, None, "account_deleted", Some(&s.user.username), &ip);
    drop(conn);
    let mut res = message(None, "Account deleted", &["The account and all its data were deleted."], None, None)?;
    res.headers_mut().insert(header::SET_COOKIE, session_cookie(&state, "", 0));
    Ok(res)
}

// --- Admin ---

struct UserRow {
    id: String,
    username: String,
    admin: bool,
    disabled: bool,
    devices: i64,
    created: String,
    is_self: bool,
}

#[derive(Template)]
#[template(path = "admin.html")]
struct AdminPage {
    nav: Option<Nav>,
    version: &'static str,
    csrf: String,
    users: Vec<UserRow>,
    db_size: String,
    public_url: String,
    audit: Vec<(String, String, String, String, String, String)>,
}

async fn admin(State(state): State<SharedState>, headers: HeaderMap) -> WebResult {
    let Some(s) = current_session(&state, &headers)? else { return Ok(to_login()) };
    if !s.user.is_admin {
        return Ok(forbidden("Admins only."));
    }
    let conn = state.db.lock();
    let users = db::list_users(&conn)?
        .into_iter()
        .map(|(u, devices)| UserRow {
            is_self: u.id == s.user.id,
            id: u.id,
            username: u.username,
            admin: u.is_admin,
            disabled: u.disabled,
            devices,
            created: iso_from_s(u.created_at),
        })
        .collect();
    let audit = audit_rows(db::audit_list(&conn, None, 100)?);
    drop(conn);
    let size = std::fs::metadata(state.config.db_path()).map(|m| m.len()).unwrap_or(0);
    render(&AdminPage {
        nav: nav(&s),
        version: VERSION,
        csrf: s.csrf.clone(),
        users,
        db_size: format!("{:.1} MB", size as f64 / 1_000_000.0),
        public_url: state.config.public_url.clone(),
        audit,
    })
}

async fn admin_invite(State(state): State<SharedState>, ClientIp(ip): ClientIp, headers: HeaderMap, Form(f): Form<CsrfForm>) -> WebResult {
    let Some(s) = current_session(&state, &headers)? else { return Ok(to_login()) };
    if let Some(denied) = check_form(&state, &headers, &s, &f.csrf) {
        return Ok(denied);
    }
    if !s.user.is_admin {
        return Ok(forbidden("Admins only."));
    }
    let code = {
        let conn = state.db.lock();
        let code = db::create_invite(&conn, db::INVITE_SIGNUP, None, &s.user.id)?;
        db::audit(&conn, Some(&s.user.id), None, "invite_created", None, &ip);
        code
    };
    message(
        nav(&s),
        "Invitation link",
        &["Send this link to the person you invite. It works once and expires in 7 days."],
        Some(format!("{}/invite/{code}", state.config.public_url)),
        Some(("/admin", "Back to admin")),
    )
}

async fn admin_user_action(
    State(state): State<SharedState>,
    ClientIp(ip): ClientIp,
    Path((id, action)): Path<(String, String)>,
    headers: HeaderMap,
    Form(f): Form<ConfirmFormLoose>,
) -> WebResult {
    let Some(s) = current_session(&state, &headers)? else { return Ok(to_login()) };
    if let Some(denied) = check_form(&state, &headers, &s, &f.csrf) {
        return Ok(denied);
    }
    if !s.user.is_admin {
        return Ok(forbidden("Admins only."));
    }
    if id == s.user.id {
        return Ok((StatusCode::BAD_REQUEST, "Use the Account page for your own account.").into_response());
    }
    let conn = state.db.lock();
    let Some(target) = db::user_by_id(&conn, &id)? else {
        return Ok((StatusCode::NOT_FOUND, "No such user").into_response());
    };
    match action.as_str() {
        "reset-link" => {
            let code = db::create_invite(&conn, db::INVITE_RESET, Some(&target.id), &s.user.id)?;
            db::audit(&conn, Some(&target.id), None, "reset_link_created", Some(&format!("by {}", s.user.username)), &ip);
            drop(conn);
            message(
                nav(&s),
                "Password reset link",
                &[&format!("Send this link to {}. It works once and expires in 7 days; their devices stay paired.", target.username)],
                Some(format!("{}/invite/{code}", state.config.public_url)),
                Some(("/admin", "Back to admin")),
            )
        }
        "disable" | "enable" => {
            db::set_disabled(&conn, &target.id, action == "disable")?;
            db::audit(&conn, Some(&target.id), None, &format!("user_{action}d"), Some(&format!("by {}", s.user.username)), &ip);
            Ok(Redirect::to("/admin").into_response())
        }
        "delete" if f.confirm.as_deref() == Some("DELETE") => {
            db::delete_user(&conn, &target.id)?;
            db::audit(&conn, None, None, "user_deleted", Some(&format!("{} by {}", target.username, s.user.username)), &ip);
            Ok(Redirect::to("/admin").into_response())
        }
        _ => Ok((StatusCode::BAD_REQUEST, "Unknown action (type DELETE to delete a user)").into_response()),
    }
}

#[derive(Deserialize)]
struct ConfirmFormLoose {
    csrf: String,
    confirm: Option<String>,
}
