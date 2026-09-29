//! `wisprcheap sync pair|unlock|status|now|passphrase|rename|unpair`.

use std::io::BufRead;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use wisprcheap_sync::crypto::{
    CryptoError, DataKey, KdfParams, b64, derive_passphrase_key, random_bytes, unb64, unwrap_key,
    wrap_key,
};
use wisprcheap_sync::protocol::{Keyring, MeResponse, PairRequest, PutKeyringRequest};

use crate::config::{ConfigArgs, LoadedConfig, ensure_config_file, load_config_lenient};
use crate::sync::client::{self, ApiError, Client, normalize_server};
use crate::sync::engine::{SyncContext, SyncError, SyncOutcome, describe_counts, sync_once};
use crate::sync::state::{StateLock, SyncState, default_state_dir};
use crate::yaml_edit::YamlText;

const USAGE: &str = "Usage: wisprcheap sync <command>

Commands:
  pair <server> <code>   Connect this computer to a sync server (code from the server's web page).
                         <server> can also be the whole wisprcheap://pair?... link.
      --name <name>        Device name shown on the server (default: the computer name)
      --passphrase-stdin   Read the sync passphrase from the standard input
  status                 Show the sync state
  now                    Sync now
  unlock                 Enter the sync passphrase again (after a reset on another device)
  passphrase             Change the sync passphrase (for every device)
  rename <name>          Rename this device on the server
  unpair                 Disconnect this computer (the local config and history stay)";

const MIN_PASSPHRASE: usize = 8;

/// What the passphrase is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    /// Creating one (first device, or a change).
    New,
    /// The existing one; `attempt` starts at 1.
    Existing { attempt: u32 },
}

pub type Ask<'a> = &'a mut dyn FnMut(Prompt) -> Result<String>;

/// Result of pairing.
pub struct PairReport {
    pub username: String,
    pub device_name: String,
    pub created_keyring: bool,
    pub first_sync: Result<SyncOutcome, SyncError>,
}

fn hostname() -> String {
    let name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Desktop".into());
    name.chars().take(60).collect()
}

/// Pairing codes are shown as `ABCD-EFGH`; accept any case, spaces and dashes.
pub fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_uppercase()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `wisprcheap://pair?server=…&code=…` → (server, code).
pub fn parse_pair_link(link: &str) -> Option<(String, String)> {
    let query = link.trim().strip_prefix("wisprcheap://pair?")?;
    let mut server = None;
    let mut code = None;
    for part in query.split('&') {
        let (k, v) = part.split_once('=')?;
        match k {
            "server" => server = Some(percent_decode(v)),
            "code" => code = Some(percent_decode(v)),
            _ => {}
        }
    }
    Some((server?, code?))
}

fn keyring_request(user_id: &str, dk: &DataKey, passphrase: &str, kdf: &KdfParams) -> Result<PutKeyringRequest> {
    let salt = random_bytes::<16>();
    let pk = derive_passphrase_key(passphrase, &salt, kdf)?;
    Ok(PutKeyringRequest {
        key_id: dk.key_id(),
        salt: b64(&salt),
        kdf: kdf.clone(),
        wrapped_key: wrap_key(&pk, user_id, dk),
    })
}

fn new_passphrase(ask: Ask<'_>) -> Result<String> {
    let p = ask(Prompt::New)?;
    if p.chars().count() < MIN_PASSPHRASE {
        bail!("the sync passphrase needs at least {MIN_PASSPHRASE} characters");
    }
    Ok(p)
}

/// Unwraps the keyring's data key with the passphrase (3 attempts).
fn unlock_existing(user_id: &str, k: &Keyring, ask: Ask<'_>) -> Result<DataKey> {
    let salt = unb64(&k.salt)?;
    for attempt in 1..=3 {
        let passphrase = ask(Prompt::Existing { attempt })?;
        let pk = derive_passphrase_key(&passphrase, &salt, &k.kdf)?;
        match unwrap_key(&pk, user_id, &k.wrapped_key) {
            Ok(dk) if dk.key_id() == k.key_id => return Ok(dk),
            Ok(_) => bail!("the server's keyring is inconsistent (key id mismatch)"),
            Err(CryptoError::WrongPassphrase) => {
                if attempt < 3 {
                    eprintln!("Wrong passphrase, try again.");
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    bail!("wrong sync passphrase")
}

/// Creates the keyring (first device) or unlocks the existing one. Returns the key and whether it
/// was created.
async fn setup_key(client: &Client, me: &MeResponse, ask: Ask<'_>, kdf: &KdfParams) -> Result<(DataKey, bool)> {
    if let Some(k) = &me.keyring {
        return Ok((unlock_existing(&me.user.id, k, ask)?, false));
    }
    let passphrase = new_passphrase(ask)?;
    let dk = DataKey::generate();
    let req = keyring_request(&me.user.id, &dk, &passphrase, kdf)?;
    match client.put_keyring(&req, None).await {
        Ok(_) => Ok((dk, true)),
        Err(e) if e.code() == Some("keyring_exists") => {
            // Another device was faster.
            let me = client.me().await?;
            let k = me.keyring.as_ref().ok_or_else(|| anyhow!("keyring disappeared"))?;
            Ok((unlock_existing(&me.user.id, k, ask)?, false))
        }
        Err(e) => Err(e.into()),
    }
}

fn load(ctx: &SyncContext) -> Result<LoadedConfig> {
    Ok(load_config_lenient(&ctx.config_args)?.0)
}

/// Edits `sync.*` in the config file (created from the example when there's none).
fn edit_config(loaded: &LoadedConfig, edit: impl FnOnce(&mut YamlText) -> Result<()>) -> Result<PathBuf> {
    let file = ensure_config_file(loaded.config_path.as_deref())?;
    let text = std::fs::read_to_string(&file).map_err(|e| anyhow!("can't read {}: {e}", file.display()))?;
    let mut y = YamlText::new(text.clone());
    edit(&mut y).map_err(|e| anyhow!("can't update {}: {e}", file.display()))?;
    if y.as_str() != text {
        std::fs::write(&file, y.as_str()).map_err(|e| anyhow!("can't write {}: {e}", file.display()))?;
    }
    Ok(file)
}

fn str_value(s: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(s.to_string())
}

/// Pairs this device, sets up the encryption, writes `sync.*` and runs the first sync.
pub async fn pair(
    ctx: &SyncContext,
    server: &str,
    code: &str,
    name: Option<&str>,
    kdf: &KdfParams,
    ask: Ask<'_>,
) -> Result<PairReport> {
    let server = normalize_server(server).map_err(|e| anyhow!(e))?;
    let loaded = load(ctx)?;
    if loaded.config.sync.enabled() {
        bail!(
            "this computer is already connected to {}; run `wisprcheap sync unpair` first",
            loaded.config.sync.server
        );
    }
    let name = name
        .map(String::from)
        .or_else(|| Some(loaded.config.sync.device_name.clone()).filter(|n| !n.trim().is_empty()))
        .unwrap_or_else(hostname);
    let paired = client::pair(
        &server,
        &PairRequest {
            code: normalize_code(code),
            name: name.trim().to_string(),
            platform: std::env::consts::OS.to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        },
    )
    .await
    .map_err(|e| match e.code() {
        Some("invalid_code") => anyhow!("unknown, expired or already used pairing code"),
        _ => anyhow!("{e}"),
    })?;
    let api = Client::new(&server, &paired.token);
    let me = api.me().await?;
    let setup = setup_key(&api, &me, ask, kdf).await;
    let (dk, created) = match setup {
        Ok(v) => v,
        Err(e) => {
            // Don't leave a half-paired device on the server.
            let _ = api.unpair().await;
            return Err(e);
        }
    };
    {
        let _lock = StateLock::acquire(&ctx.state_dir).await?;
        let st = SyncState {
            server: server.clone(),
            user_id: me.user.id.clone(),
            username: me.user.username.clone(),
            device_id: me.device.id.clone(),
            device_name: me.device.name.clone(),
            key_id: dk.key_id(),
            ..Default::default()
        };
        st.save(&ctx.state_dir)?;
    }
    let has_history_mode = loaded
        .config_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| YamlText::new(t).doc().ok())
        .is_some_and(|doc| crate::yaml_edit::get(&doc, &["sync", "history"]).is_some());
    edit_config(&loaded, |y| {
        y.set(&["sync", "server"], &str_value(&server))?;
        y.set(&["sync", "token"], &str_value(&paired.token))?;
        y.set(&["sync", "key"], &str_value(&dk.export()))?;
        y.set(&["sync", "deviceName"], &str_value(&me.device.name))?;
        if !has_history_mode {
            y.set(&["sync", "history"], &str_value("upload"))?;
        }
        Ok(())
    })?;
    let first_sync = sync_once(ctx).await;
    Ok(PairReport {
        username: me.user.username,
        device_name: me.device.name,
        created_keyring: created,
        first_sync,
    })
}

fn connected(loaded: &LoadedConfig) -> Result<Client> {
    let sc = &loaded.config.sync;
    if !sc.enabled() {
        bail!("sync is not set up; run `wisprcheap sync pair <server> <code>`");
    }
    let server = normalize_server(&sc.server).map_err(|e| anyhow!(e))?;
    Ok(Client::new(&server, &sc.token))
}

/// Enters the passphrase again (or sets a new one after an encryption reset), then syncs.
pub async fn unlock(ctx: &SyncContext, kdf: &KdfParams, ask: Ask<'_>) -> Result<Result<SyncOutcome, SyncError>> {
    let loaded = load(ctx)?;
    let api = connected(&loaded)?;
    let me = api.me().await.map_err(api_error)?;
    let current = DataKey::import(&loaded.config.sync.key).ok();
    if let (Some(k), Some(dk)) = (&me.keyring, &current)
        && k.key_id == dk.key_id()
    {
        println!("The encryption key of this device is already up to date.");
    } else {
        let (dk, _) = setup_key(&api, &me, ask, kdf).await?;
        edit_config(&loaded, |y| y.set(&["sync", "key"], &str_value(&dk.export())))?;
    }
    Ok(sync_once(ctx).await)
}

/// Changes the passphrase that protects the data key (same key, so other devices keep working).
pub async fn change_passphrase(ctx: &SyncContext, kdf: &KdfParams, ask: Ask<'_>) -> Result<()> {
    let loaded = load(ctx)?;
    let api = connected(&loaded)?;
    let dk = DataKey::import(&loaded.config.sync.key)
        .map_err(|_| anyhow!("this device has no encryption key; run `wisprcheap sync unlock` first"))?;
    let me = api.me().await.map_err(api_error)?;
    let Some(k) = &me.keyring else {
        bail!("the encryption was reset on the server; run `wisprcheap sync unlock`");
    };
    if k.key_id != dk.key_id() {
        bail!("this device's key is outdated; run `wisprcheap sync unlock` first");
    }
    let passphrase = new_passphrase(ask)?;
    let req = keyring_request(&me.user.id, &dk, &passphrase, kdf)?;
    api.put_keyring(&req, Some(k.key_version))
        .await
        .map_err(|e| match e.code() {
            Some("version_mismatch") => anyhow!("the passphrase was changed meanwhile on another device; try again"),
            _ => anyhow!("{e}"),
        })?;
    Ok(())
}

/// Revokes this device's token and removes `sync.token` / `sync.key` from the config.
pub async fn unpair(ctx: &SyncContext) -> Result<()> {
    let loaded = load(ctx)?;
    if let Ok(api) = connected(&loaded) {
        match api.unpair().await {
            Ok(()) | Err(ApiError::Unauthorized) => {}
            Err(e) => eprintln!("Warning: couldn't tell the server ({e}). Remove the device from the server's web page."),
        }
    }
    edit_config(&loaded, |y| {
        y.remove(&["sync", "token"])?;
        y.remove(&["sync", "key"])
    })?;
    let _lock = StateLock::acquire(&ctx.state_dir).await?;
    let _ = std::fs::remove_file(ctx.state_dir.join("state.json"));
    Ok(())
}

pub async fn rename(ctx: &SyncContext, name: &str) -> Result<()> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        bail!("the device name must have 1 to 60 characters");
    }
    let loaded = load(ctx)?;
    let api = connected(&loaded)?;
    api.rename(name).await.map_err(api_error)?;
    edit_config(&loaded, |y| y.set(&["sync", "deviceName"], &str_value(name)))?;
    Ok(())
}

fn api_error(e: ApiError) -> anyhow::Error {
    anyhow!("{}", SyncError::from(e))
}

// ---------------------------------------------------------------------------
// Terminal front end
// ---------------------------------------------------------------------------

fn read_stdin_line() -> Result<String> {
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim_end_matches(['\r', '\n']).to_string();
    if line.is_empty() {
        bail!("no passphrase on the standard input");
    }
    Ok(line)
}

fn terminal_ask(from_stdin: bool) -> impl FnMut(Prompt) -> Result<String> {
    move |prompt| {
        if from_stdin {
            if let Prompt::Existing { attempt } = prompt
                && attempt > 1
            {
                bail!("wrong sync passphrase");
            }
            return read_stdin_line();
        }
        match prompt {
            Prompt::New => {
                println!(
                    "Choose a sync passphrase. It encrypts your settings, keys, dictionary and history before\n\
                     they leave this computer: the server never sees it. You'll type it once on each device.\n\
                     If you forget it, the synced data can't be decrypted (you can reset it on the server)."
                );
                loop {
                    let a = rpassword::prompt_password("New sync passphrase: ")?;
                    if a.chars().count() < MIN_PASSPHRASE {
                        println!("Use at least {MIN_PASSPHRASE} characters.");
                        continue;
                    }
                    let b = rpassword::prompt_password("Repeat it: ")?;
                    if a == b {
                        return Ok(a);
                    }
                    println!("They don't match, try again.");
                }
            }
            Prompt::Existing { .. } => Ok(rpassword::prompt_password("Sync passphrase: ")?),
        }
    }
}

fn print_outcome(result: &Result<SyncOutcome, SyncError>) -> bool {
    match result {
        Ok(o) => {
            if o.first {
                let applied = describe_counts(&o.applied);
                let pushed = describe_counts(&o.pushed);
                if !applied.is_empty() {
                    println!("  Updated from the server: {applied}.");
                }
                if !pushed.is_empty() {
                    println!("  Uploaded: {pushed}.");
                }
                if o.uploaded > 0 {
                    println!("  History entries uploaded: {}.", o.uploaded);
                }
                if applied.is_empty() && pushed.is_empty() && o.uploaded == 0 {
                    println!("  Nothing to merge.");
                }
            } else {
                println!("Synced: {}.", o.summary());
            }
            if let Some(m) = &o.month {
                let usd = if m.total_usd > 0.0 && m.total_usd < 0.01 {
                    "<$0.01".to_string()
                } else {
                    format!("${:.2}", m.total_usd)
                };
                println!(
                    "  This month, all devices: {usd}, {} words ({} device(s)).",
                    m.words, o.devices
                );
            }
            true
        }
        Err(e) => {
            eprintln!("Sync failed: {e}");
            false
        }
    }
}

/// Lets a running instance refresh its sync status (it syncs too, quickly: nothing is left to do).
async fn notify_running_app() {
    let _ = crate::instance::send_command("sync-now", Duration::from_secs(1)).await;
}

/// Entry point of `wisprcheap sync ...` (`rest`: the arguments after `sync`).
pub fn main(rest: &[String], config: &ConfigArgs) -> i32 {
    let ctx = SyncContext {
        config_args: config.clone(),
        state_dir: default_state_dir(),
    };
    let mut positional = Vec::new();
    let mut name: Option<String> = None;
    let mut from_stdin = false;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--name" => match it.next() {
                Some(n) => name = Some(n.clone()),
                None => {
                    eprintln!("--name needs a value\n\n{USAGE}");
                    return 2;
                }
            },
            "--passphrase-stdin" => from_stdin = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            s if s.starts_with("--") => {
                eprintln!("unknown option {s}\n\n{USAGE}");
                return 2;
            }
            _ => positional.push(a.clone()),
        }
    }
    let Some(command) = positional.first().cloned() else {
        println!("{USAGE}");
        return 2;
    };
    let args = &positional[1..];
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let kdf = KdfParams::default();
    let mut ask = terminal_ask(from_stdin);
    let result: Result<i32> = rt.block_on(async {
        match command.as_str() {
            "pair" => {
                let (server, code) = match args {
                    [link] => parse_pair_link(link).ok_or_else(|| anyhow!("expected <server> <code> or a wisprcheap://pair link"))?,
                    [server, code] => (server.clone(), code.clone()),
                    _ => bail!("usage: wisprcheap sync pair <server> <code>"),
                };
                if server.trim().starts_with("http://") {
                    eprintln!("Warning: the connection to {server} isn't encrypted (http://). The synced data is, but use https:// outside your own network.");
                }
                let report = pair(&ctx, &server, &code, name.as_deref(), &kdf, &mut ask).await?;
                println!(
                    "Connected as \"{}\" (account {}).{}",
                    report.device_name,
                    report.username,
                    if report.created_keyring { " Sync passphrase set." } else { "" }
                );
                let ok = print_outcome(&report.first_sync);
                notify_running_app().await;
                Ok(if ok { 0 } else { 1 })
            }
            "unlock" => {
                let outcome = unlock(&ctx, &kdf, &mut ask).await?;
                let ok = print_outcome(&outcome);
                notify_running_app().await;
                Ok(if ok { 0 } else { 1 })
            }
            "now" => {
                let outcome = sync_once(&ctx).await;
                let ok = print_outcome(&outcome);
                notify_running_app().await;
                Ok(if ok { 0 } else { 1 })
            }
            "status" => status(&ctx).await,
            "passphrase" => {
                change_passphrase(&ctx, &kdf, &mut ask).await?;
                println!("Sync passphrase changed. Other devices keep working; use the new passphrase for new ones.");
                Ok(0)
            }
            "rename" => {
                let [new_name] = args else {
                    bail!("usage: wisprcheap sync rename <name>");
                };
                rename(&ctx, new_name).await?;
                println!("Renamed to \"{}\".", new_name.trim());
                Ok(0)
            }
            "unpair" => {
                unpair(&ctx).await?;
                println!("Disconnected. The config, dictionary and history stay on this computer.");
                Ok(0)
            }
            "help" => {
                println!("{USAGE}");
                Ok(0)
            }
            other => bail!("unknown sync command \"{other}\"\n\n{USAGE}"),
        }
    });
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

async fn status(ctx: &SyncContext) -> Result<i32> {
    let loaded = load(ctx)?;
    let sc = &loaded.config.sync;
    if !sc.enabled() {
        println!("Sync is off. Set it up with `wisprcheap sync pair <server> <code>` (see README).");
        return Ok(0);
    }
    let st = SyncState::load(&ctx.state_dir);
    println!("Server:     {}", sc.server);
    if !st.username.is_empty() {
        println!("Account:    {}", st.username);
    }
    println!(
        "Device:     {} ({})",
        if st.device_name.is_empty() { &sc.device_name } else { &st.device_name },
        if st.device_id.is_empty() { "unknown id" } else { &st.device_id }
    );
    println!("History:    {}", sc.history.as_str());
    let last = st
        .last_sync
        .as_deref()
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "never".into());
    println!("Last sync:  {last}");
    if !st.outbox.is_empty() {
        println!("To send:    {} change(s)", st.outbox.len());
    }
    if !st.pending.is_empty() {
        println!("Not applied: {} change(s) from other devices (see the log)", st.pending.len());
    }
    let api = connected(&loaded)?;
    match api.me().await {
        Ok(me) => {
            let key = DataKey::import(&sc.key).ok();
            let key_state = match (&me.keyring, &key) {
                (Some(k), Some(dk)) if k.key_id == dk.key_id() => "ok",
                (_, None) => "missing (run `wisprcheap sync unlock`)",
                _ => "outdated (run `wisprcheap sync unlock`)",
            };
            println!("Connection: ok (server {})", me.server_version);
            println!("Encryption: {key_state}");
            Ok(0)
        }
        Err(e) => {
            println!("Connection: {}", SyncError::from(e));
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_codes() {
        assert_eq!(
            parse_pair_link("wisprcheap://pair?server=https%3A%2F%2Fsync.example.com&code=ABCD2345"),
            Some(("https://sync.example.com".into(), "ABCD2345".into()))
        );
        assert_eq!(parse_pair_link("https://x"), None);
        assert_eq!(normalize_code(" abcd-2345 "), "ABCD2345");
        assert_eq!(percent_decode("a%20b%zz"), "a b%zz");
    }
}
