//! A real session-bus exchange with a mock tray host, without GTK or a display.

use super::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zbus::blocking::{Connection, Proxy, connection::Builder};
use zbus::zvariant::OwnedValue;

struct Watcher {
    registered: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(&self, service: &str) {
        self.registered.lock().unwrap().push(service.to_string());
    }
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.registered.lock().unwrap().clone()
    }
}

type Entries = Vec<(i32, HashMap<String, OwnedValue>)>;

#[derive(
    serde::Deserialize, zbus::zvariant::Type, zbus::zvariant::Value, zbus::zvariant::OwnedValue,
)]
struct Layout {
    id: i32,
    properties: HashMap<String, OwnedValue>,
    children: Vec<OwnedValue>,
}

fn entries(menu: &Proxy<'_>) -> Entries {
    let (_, root): (u32, Layout) = menu
        .call(
            "GetLayout",
            &(0i32, -1i32, vec!["label", "enabled", "toggle-state"]),
        )
        .unwrap();
    let mut out = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        out.push((node.id, node.properties));
        pending.extend(
            node.children
                .into_iter()
                .map(|child| Layout::try_from(child).unwrap()),
        );
    }
    out
}

fn item<'a>(entries: &'a Entries, label: &str) -> Option<&'a (i32, HashMap<String, OwnedValue>)> {
    entries.iter().find(|(_, properties)| {
        properties
            .get("label")
            .and_then(|v| <&str>::try_from(v).ok())
            == Some(label)
    })
}

fn wait<T>(mut get: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(value) = get() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "tray host did not observe the expected update"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires an isolated D-Bus session; run tests/ksni_tray.py"]
fn ksni_tray_backend() {
    // Starting before the host verifies that desktop startup ordering doesn't lose the tray.
    let directory = tempfile::tempdir().unwrap();
    let mut tray = Tray::new(directory.path()).unwrap();
    let registered = Arc::new(Mutex::new(Vec::new()));
    let bus = Builder::session()
        .unwrap()
        .name("org.kde.StatusNotifierWatcher")
        .unwrap()
        .serve_at(
            "/StatusNotifierWatcher",
            Watcher {
                registered: registered.clone(),
            },
        )
        .unwrap()
        .build()
        .unwrap();
    let service = wait(|| registered.lock().unwrap().first().cloned());
    let client = Connection::session().unwrap();
    let notifier = Proxy::new(
        &client,
        service.as_str(),
        "/StatusNotifierItem",
        "org.kde.StatusNotifierItem",
    )
    .unwrap();
    assert_eq!(notifier.get_property::<String>("Id").unwrap(), "wisprcheap");
    let pixmaps: Vec<(i32, i32, Vec<u8>)> = notifier.get_property("IconPixmap").unwrap();
    assert!(
        pixmaps
            .iter()
            .any(|(w, h, pixels)| *w > 0 && pixels.len() == (*w * *h * 4) as usize)
    );
    let menu = Proxy::new(
        &client,
        service.as_str(),
        "/MenuBar",
        "com.canonical.dbusmenu",
    )
    .unwrap();
    let initial = entries(&menu);
    assert!(item(&initial, "Open WisprCheap").is_some());
    assert!(!bool::try_from(&item(&initial, "Copy last dictation").unwrap().1["enabled"]).unwrap());

    tray.copy_last.set_enabled(true);
    tray.pause.set_checked(true);
    tray.set_state(IconName::Recording, "Recording");
    tray.set_translations(&["French to English".into(), "English to French".into()], 1);
    wait(|| {
        let current = entries(&menu);
        let pause = item(&current, "Pause dictation")?;
        let copy = item(&current, "Copy last dictation")?;
        // D-Bus menus omit default properties: enabled=true and toggle-state=0.
        (pause
            .1
            .get("toggle-state")
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(0)
            == 1
            && copy
                .1
                .get("enabled")
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(true)
            && item(&current, "French to English").is_some()
            && item(&current, "English to French").is_some()
            && item(&current, "wisprcheap: Recording").is_some())
        .then_some(())
    });
    let pause_id = item(&entries(&menu), "Pause dictation").unwrap().0;
    let events = MenuEvent::receiver();
    while events.try_recv().is_ok() {}
    menu.call::<_, _, ()>(
        "Event",
        &(pause_id, "clicked", OwnedValue::from(0i32), 0u32),
    )
    .unwrap();
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2)).unwrap().id.0,
        "toggle-pause"
    );
    tray.set_translations(&[], -1);
    wait(|| {
        item(&entries(&menu), "French to English")
            .is_none()
            .then_some(())
    });
    drop(tray);
    let daemon = zbus::blocking::fdo::DBusProxy::new(&client).unwrap();
    wait(|| {
        (!daemon
            .name_has_owner(service.as_str().try_into().unwrap())
            .unwrap())
        .then_some(())
    });
    drop(bus);
}
