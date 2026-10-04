//! End to end: the real sync server in-process and two desktop "devices" (config, .env, history and
//! sync state in temporary directories). Checks pairing, the first-sync merge, write-back into
//! config.yaml / .env with comments kept, dictionary and key changes both ways, echo suppression,
//! history upload / download, statistics and unpairing.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use wisprcheap::config::{ConfigArgs, LoadedConfig, load_config_lenient};
use wisprcheap::companion::{ActivityScope, Reply, Request, handle_request_at};
use wisprcheap::sync::cli::{self, Prompt};
use wisprcheap::sync::{SyncContext, SyncError, sync_once};
use wisprcheap::yaml_edit::YamlText;
use wisprcheap_server::{AppState, Config, Db, SharedState, auth, db};
use wisprcheap_sync::crypto::KdfParams;

const PASSPHRASE: &str = "correct horse battery";

struct Device {
    dir: PathBuf,
    ctx: SyncContext,
}

impl Device {
    fn new(root: &Path, name: &str, config: &str, env: &str, history: &str) -> Self {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.yaml"), config).unwrap();
        if !env.is_empty() {
            std::fs::write(dir.join(".env"), env).unwrap();
        }
        if !history.is_empty() {
            std::fs::write(dir.join("history.jsonl"), history).unwrap();
        }
        let ctx = SyncContext {
            config_args: ConfigArgs {
                config: Some(dir.join("config.yaml").to_string_lossy().into_owned()),
                isolated: true,
            },
            state_dir: dir.join("state"),
        };
        Self { dir, ctx }
    }

    fn loaded(&self) -> LoadedConfig {
        load_config_lenient(&self.ctx.config_args).unwrap().0
    }

    fn text(&self, file: &str) -> String {
        std::fs::read_to_string(self.dir.join(file)).unwrap_or_default()
    }

    fn terms(&self) -> Vec<String> {
        self.loaded().dictionary.into_iter().map(|d| d.term).collect()
    }

    /// An edit made by the user in config.yaml.
    fn edit(&self, f: impl FnOnce(&mut YamlText)) {
        let mut y = YamlText::new(self.text("config.yaml"));
        f(&mut y);
        std::fs::write(self.dir.join("config.yaml"), y.as_str()).unwrap();
    }
}

async fn start_server(dir: &Path) -> (String, SharedState) {
    let config = Config::for_tests(dir);
    let db = Db::open(&config.db_path()).unwrap();
    let state = AppState::new(config, db);
    let app = wisprcheap_server::app(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap();
    });
    (format!("http://{addr}"), state)
}

fn entry(id: Option<&str>, words: u64) -> String {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
    let id = id.map(|i| format!("\"id\":\"{i}\",")).unwrap_or_default();
    format!(
        "{{\"ts\":\"{ts}\",{id}\"durationSec\":3.2,\"transcription\":{{\"provider\":\"elevenlabs\",\"model\":\"scribe_v2\",\"ms\":800,\"keyterms\":2}},\
         \"polish\":{{\"model\":\"gpt-6-sol\",\"ms\":500,\"inputTokens\":300,\"outputTokens\":20}},\"raw\":\"secret words\",\"text\":\"Secret words.\",\
         \"words\":{words},\"delivered\":\"pasted\",\"costUsd\":{{\"transcription\":0.0002,\"polish\":0.0001,\"total\":0.0003}}}}\n"
    )
}

const CONFIG_A: &str = "# device A
transcription:
  provider: elevenlabs
  elevenlabs:
    apiKey: ${WCTEST_EL}
  openai:
    apiKey: ${WCTEST_OA}
polish:
  apiKey: ${WCTEST_OA}
  model: gpt-6-sol   # the model
dictionary:
  - Kubernetes
  - term: pnpm
    soundsLike: [p n p m]
translation:
  pairs:
    - { from: fr, to: en }
pricing:
  overrides:
    - { model: my-llm, inputPerM: 1, outputPerM: 2 }
";

const CONFIG_B: &str = "# device B
polish:
  model: gpt-6-luna
dictionary:
  - Bun
command:
  temperature: 0.2   # warmer
sync:
  history: download
";

fn kdf() -> KdfParams {
    // The server's minimum, to keep the test fast.
    KdfParams {
        alg: "argon2id".into(),
        m: 8192,
        t: 1,
        p: 1,
    }
}

#[test]
fn two_devices_sync_end_to_end() {
    // The conventional variables of the real environment would leak into the devices' configs.
    // SAFETY: this is the only test of this binary, and no other thread is running yet.
    unsafe {
        std::env::remove_var("ELEVENLABS_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(scenario());
}

async fn scenario() {
    let root = tempfile::tempdir().unwrap();
    let (url, state) = start_server(&root.path().join("server")).await;
    let user_id = {
        let conn = state.db.lock();
        db::create_user(&conn, "alice", &auth::hash_password("a long enough password").unwrap(), true).unwrap()
    };
    let code = || db::create_pairing_code(&state.db.lock(), &user_id, None).unwrap().0;

    let legacy = entry(None, 2);
    let with_id = entry(Some("0b8d7a5e-3c1f-4c2a-9d4e-2f1a6b7c8d9e"), 3);
    let a = Device::new(
        root.path(),
        "a",
        CONFIG_A,
        "WCTEST_EL=el-A\nWCTEST_OA=sk-A\n",
        &format!("{legacy}{with_id}"),
    );
    let b = Device::new(root.path(), "b", CONFIG_B, "", "");

    // --- A pairs first: creates the keyring and uploads its profile and history.
    let mut ask_new = |p: Prompt| {
        assert_eq!(p, Prompt::New);
        Ok(PASSPHRASE.to_string())
    };
    let report = cli::pair(&a.ctx, &url, &code(), Some("Laptop A"), &kdf(), &mut ask_new)
        .await
        .unwrap();
    assert!(report.created_keyring);
    let first = report.first_sync.unwrap();
    assert!(first.first);
    assert_eq!(first.pushed.get("setting"), Some(&28), "{first:?}");
    assert_eq!(first.pushed.get("secret"), Some(&5));
    assert_eq!(first.pushed.get("dict"), Some(&2));
    assert_eq!(first.pushed.get("pair"), Some(&1));
    assert_eq!(first.pushed.get("price"), Some(&1));
    assert_eq!(first.uploaded, 2);
    let month = first.month.expect("month stats");
    assert_eq!((month.entries, month.words), (2, 5));
    let a_text = a.text("config.yaml");
    assert!(a_text.contains("sync:\n  server: http://127.0.0.1"), "{a_text}");
    assert!(a_text.contains("  token: wcs_") && a_text.contains("  key: wck_"), "{a_text}");
    assert!(a_text.starts_with(CONFIG_A), "user content changed:\n{a_text}");
    let a_device = first.device_id.clone();

    // --- B pairs with the same passphrase: the server wins, B's own term is uploaded.
    let mut ask_existing = |p: Prompt| {
        assert!(matches!(p, Prompt::Existing { .. }));
        Ok(PASSPHRASE.to_string())
    };
    let report = cli::pair(&b.ctx, &url, &code(), Some("Desktop B"), &kdf(), &mut ask_existing)
        .await
        .unwrap();
    assert!(!report.created_keyring);
    let first_b = report.first_sync.unwrap();
    assert!(first_b.first);
    assert_eq!(first_b.pushed.get("dict"), Some(&1), "{first_b:?}"); // Bun
    let lb = b.loaded();
    assert_eq!(lb.config.polish.model, "gpt-6-sol");
    assert_eq!(lb.config.command.temperature, None, "inherit removes the key");
    assert_eq!(b.terms(), vec!["Bun", "Kubernetes", "pnpm"]);
    assert_eq!(lb.dictionary[2].sounds_like, vec!["p n p m"]);
    assert_eq!(
        lb.translation_pairs.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        vec!["fr>en"]
    );
    assert_eq!(lb.config.pricing.overrides.len(), 1);
    assert_eq!(lb.config.pricing.overrides[0].model, "my-llm");
    assert_eq!(lb.config.transcription.elevenlabs.api_key.as_deref(), Some("el-A"));
    assert_eq!(lb.config.transcription.openai.api_key.as_deref(), Some("sk-A"));
    assert_eq!(lb.config.polish.api_key.as_deref(), Some("sk-A"));
    let b_text = b.text("config.yaml");
    assert!(b_text.starts_with("# device B\n"), "{b_text}");
    assert!(b.text(".env").contains("ELEVENLABS_API_KEY=el-A"), "{}", b.text(".env"));
    assert!(
        std::fs::read_dir(&b.dir).unwrap().flatten().any(|e| e
            .file_name()
            .to_string_lossy()
            .starts_with("config.yaml.bak-")),
        "first sync keeps a backup"
    );
    // History download: A's two entries, marked with A's device.
    let b_history = b.text("history.jsonl");
    assert_eq!(b_history.lines().count(), 2, "{b_history}");
    assert!(b_history.lines().all(|l| l.contains(&a_device)));
    assert!(b_history.contains("Secret words."));
    assert_eq!(first_b.month.as_ref().map(|m| m.entries), Some(2));
    assert_eq!(first_b.devices, 2);

    // --- A gets B's term; its own comments stay.
    let again = sync_once(&a.ctx).await.unwrap();
    assert!(!again.first);
    assert_eq!(a.terms(), vec!["Kubernetes", "pnpm", "Bun"]);
    assert!(a.text("config.yaml").contains("  model: gpt-6-sol   # the model\n"));

    // --- Nothing changed: nothing is sent back (echo suppression).
    let idle_b = sync_once(&b.ctx).await.unwrap();
    assert!(idle_b.pushed.is_empty() && idle_b.applied.is_empty(), "{idle_b:?}");
    let idle_a = sync_once(&a.ctx).await.unwrap();
    assert!(idle_a.pushed.is_empty() && idle_a.applied.is_empty(), "{idle_a:?}");

    // --- Edits on B (remove a term, change a setting) reach A.
    b.edit(|y| {
        y.list_remove(&["dictionary"], &|v| {
            v.get("term").and_then(|t| t.as_str()) == Some("pnpm")
        })
        .unwrap();
        y.set(&["polish", "model"], &serde_yaml::Value::String("gpt-5-mini".into()))
            .unwrap();
    });
    let pushed = sync_once(&b.ctx).await.unwrap();
    assert_eq!(pushed.pushed.get("dict"), Some(&1));
    assert_eq!(pushed.pushed.get("setting"), Some(&1));
    let got = sync_once(&a.ctx).await.unwrap();
    assert_eq!(got.applied.get("dict"), Some(&1));
    assert_eq!(a.terms(), vec!["Kubernetes", "Bun"]);
    assert_eq!(a.loaded().config.polish.model, "gpt-5-mini");
    assert!(a.text("config.yaml").contains("  model: gpt-5-mini   # the model\n"));

    // --- A new key on A (in its .env) reaches B's .env.
    std::fs::write(a.dir.join(".env"), "WCTEST_EL=el-A\nWCTEST_OA=sk-A2\n").unwrap();
    let sent = sync_once(&a.ctx).await.unwrap();
    assert_eq!(sent.pushed.get("secret"), Some(&1), "{sent:?}"); // openai; cleanup stays "same as OpenAI"
    sync_once(&b.ctx).await.unwrap();
    let lb = b.loaded();
    assert_eq!(lb.config.transcription.openai.api_key.as_deref(), Some("sk-A2"));
    assert_eq!(lb.config.polish.api_key.as_deref(), Some("sk-A2"));
    let idle_b = sync_once(&b.ctx).await.unwrap();
    assert!(idle_b.pushed.is_empty(), "{idle_b:?}");

    // --- A different cleanup key on B gets a variable of its own on A (OPENAI key unchanged).
    std::fs::write(b.dir.join(".env"), format!("{}GROQ=gsk-B\n", b.text(".env"))).unwrap();
    b.edit(|y| {
        y.set(&["polish", "apiKey"], &serde_yaml::Value::String("${GROQ}".into()))
            .unwrap()
    });
    sync_once(&b.ctx).await.unwrap();
    sync_once(&a.ctx).await.unwrap();
    let la = a.loaded();
    assert_eq!(la.config.polish.api_key.as_deref(), Some("gsk-B"));
    assert_eq!(la.config.transcription.openai.api_key.as_deref(), Some("sk-A2"));
    let idle_a = sync_once(&a.ctx).await.unwrap();
    assert!(idle_a.pushed.is_empty(), "{idle_a:?}");

    // --- History recorded on B after pairing is uploaded once.
    let b_device = first_b.device_id.clone();
    let mut new_entry = entry(Some("5f0c2a8e-1b3d-4e6f-8a9b-0c1d2e3f4a5b"), 4);
    new_entry = new_entry.replacen("{", &format!("{{\"device\":\"{b_device}\","), 1);
    // Android's delivery method must also be accepted by desktop history/statistics.
    new_entry = new_entry.replace("\"delivered\":\"pasted\"", "\"delivered\":\"inserted\"");
    let mut hist = b.text("history.jsonl");
    hist.push_str(&new_entry);
    std::fs::write(b.dir.join("history.jsonl"), hist).unwrap();
    let up = sync_once(&b.ctx).await.unwrap();
    assert_eq!(up.uploaded, 1);
    assert_eq!(up.month.as_ref().map(|m| m.entries), Some(3));
    let up = sync_once(&b.ctx).await.unwrap();
    assert_eq!(up.uploaded, 0);

    // --- Companion reads account history automatically, including on upload-only A.
    // An unsynced local entry is included, and legacy/uploaded/downloaded copies aren't counted twice.
    let pending = entry(Some("d7c54367-e3fb-4eb4-8422-0fda8cac39ef"), 6);
    std::fs::write(a.dir.join("history.jsonl"), format!("{}{pending}", a.text("history.jsonl"))).unwrap();
    for (device, current_count, current_words) in [(&a, 3, 11), (&b, 1, 4)] {
        let before_history = device.text("history.jsonl");
        let before_config = device.text("config.yaml");
        let before_state = std::fs::read(device.ctx.state_dir.join("state.json")).unwrap();
        for scope in [ActivityScope::CurrentDevice, ActivityScope::AllDevices] {
            let reply = handle_request_at(device.ctx.config_args.clone(), Request::Activity {
                query: String::new(), errors_only: false, days: Some(7), scope,
            }, device.ctx.state_dir.clone()).await;
            let Reply::Activity(data) = reply else { panic!("{reply:?}"); };
            let expected = if scope == ActivityScope::CurrentDevice {
                (current_count, current_words)
            } else if std::ptr::eq(device, &a) { (4, 15) } else { (3, 9) };
            assert_eq!((data.totals.recordings, data.totals.words), expected);
            let expected_cost = if scope == ActivityScope::CurrentDevice { current_count } else { expected.0 } as f64 * 0.0003;
            assert!((data.totals.total_usd - expected_cost).abs() < 1e-10);
            assert_eq!(data.scope, scope);
            assert_eq!(data.daily.iter().map(|day| day.count).sum::<usize>(), expected.0);
        }
        assert_eq!(device.text("history.jsonl"), before_history);
        assert_eq!(device.text("config.yaml"), before_config);
        assert_eq!(std::fs::read(device.ctx.state_dir.join("state.json")).unwrap(), before_state);
    }

    // A key mismatch must produce an error, never local totals labelled as all-device totals.
    let original_key = a.loaded().config.sync.key;
    a.edit(|y| y.set(&["sync", "key"], &serde_yaml::Value::String(wisprcheap_sync::crypto::DataKey::generate().export())).unwrap());
    let reply = handle_request_at(a.ctx.config_args.clone(), Request::Activity {
        query: String::new(), errors_only: false, days: None, scope: ActivityScope::AllDevices,
    }, a.ctx.state_dir.clone()).await;
    assert!(matches!(reply, Reply::Error(ref message) if message.contains("unlock")), "{reply:?}");
    a.edit(|y| y.set(&["sync", "key"], &serde_yaml::Value::String(original_key)).unwrap());

    // --- Unpairing B: the token is revoked and removed, the data stays.
    cli::unpair(&b.ctx).await.unwrap();
    let b_text = b.text("config.yaml");
    assert!(!b_text.contains("token:") && !b_text.contains("key: wck_"), "{b_text}");
    assert!(matches!(sync_once(&b.ctx).await, Err(SyncError::Disabled)));
    assert_eq!(b.terms(), vec!["Bun", "Kubernetes"]);
    if std::env::var("WCTEST_SHOW").is_ok() {
        for (name, d) in [("A", &a), ("B", &b)] {
            println!("===== {name} config.yaml\n{}===== {name} .env\n{}", d.text("config.yaml"), d.text(".env"));
        }
    }
}
