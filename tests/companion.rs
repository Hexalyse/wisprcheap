//! Exercise real local IPC and validated settings patches without microphones or provider calls.
use serde_json::{Value, json};
use std::time::Duration;
use wisprcheap::companion::{self, Change, Reply, Request};
use wisprcheap::config::ConfigArgs;
use wisprcheap::instance::{self, AcquireError};

#[tokio::test]
async fn ipc_round_trips_large_unicode_documents_and_preserves_explicit_null() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    std::fs::write(
        &config,
        "# keep me\npolish:\n  enabled: false\n  temperature: 0.6\ncommand:\n  temperature: 0.3\n",
    )
    .unwrap();
    let args = ConfigArgs {
        config: Some(config.to_string_lossy().into_owned()),
        isolated: true,
    };
    #[cfg(windows)]
    let name = format!(r"\\.\pipe\wisprcheap-ui-test-{}", uuid::Uuid::new_v4());
    #[cfg(unix)]
    let name = dir.path().join("ui.sock").to_string_lossy().into_owned();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let listener = instance::acquire_instance_at(name.clone(), tx.clone(), Duration::ZERO)
        .await
        .unwrap();
    assert!(matches!(
        instance::acquire_instance_at(name.clone(), tx, Duration::ZERO).await,
        Err(AcquireError::AlreadyRunning)
    ));
    let server = tokio::spawn(async move {
        while let Some((command, reply)) = rx.recv().await {
            if command == "large" {
                let _ = reply.send(Reply::Log(vec!["Éléphant 🦀\n".repeat(12_000)]).json());
            } else {
                let request: Request =
                    serde_json::from_str(command.strip_prefix(companion::PREFIX).unwrap()).unwrap();
                let _ = reply.send(companion::disk_request(&args, request).json());
            }
        }
    });
    let send = async |request: Request| -> Reply {
        let command = format!(
            "{}{}",
            companion::PREFIX,
            serde_json::to_string(&request).unwrap()
        );
        serde_json::from_str(
            &instance::send_command_at(&name, &command, Duration::from_secs(3))
                .await
                .unwrap(),
        )
        .unwrap()
    };
    let Reply::Config(before) = send(Request::Config).await else {
        panic!("config response");
    };
    let Reply::Config(saved) = send(Request::Save {
        revision: before.revision.clone(),
        changes: vec![
            Change {
                path: "polish.temperature".into(),
                value: Some(Value::Null),
            },
            Change {
                path: "command.temperature".into(),
                value: None,
            },
            Change {
                path: "dictionary".into(),
                value: Some(json!(["Éléphant 🦀"])),
            },
        ],
    })
    .await
    else {
        panic!("save response");
    };
    let source = std::fs::read_to_string(&saved.path).unwrap();
    let yaml: Value = serde_yaml::from_str(&source).unwrap();
    assert!(source.contains("# keep me"));
    assert!(yaml["polish"].get("temperature").unwrap().is_null());
    assert!(yaml["command"].get("temperature").is_none());
    assert_eq!(saved.dictionary, json!(["Éléphant 🦀"]));
    assert!(matches!(
        send(Request::Save {
            revision: before.revision,
            changes: vec![]
        })
        .await,
        Reply::Error(_)
    ));
    let reply = instance::send_command_at(&name, "large", Duration::from_secs(3))
        .await
        .unwrap();
    let Reply::Log(lines) = serde_json::from_str(&reply).unwrap() else {
        panic!("large response");
    };
    assert_eq!(lines[0], "Éléphant 🦀\n".repeat(12_000));
    drop(listener);
    server.abort();
}
