//! Awaiting the UI channel drives GLib's main context; there is no polling timer while idle.

use super::{Ui, UiEvent};
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct EventLoop {
    receiver: mpsc::UnboundedReceiver<UiEvent>,
}

pub fn create() -> anyhow::Result<(EventLoop, Ui)> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let ui = Ui {
        send: Arc::new(move |event| {
            let _ = sender.send(event);
        }),
    };
    Ok((EventLoop { receiver }, ui))
}

pub fn run(
    mut event_loop: EventLoop,
    mut callback: impl FnMut(Option<UiEvent>) -> bool,
) -> anyhow::Result<()> {
    gtk::glib::MainContext::default().block_on(async {
        if callback(None) {
            return;
        }
        while let Some(event) = event_loop.receiver.recv().await {
            if callback(Some(event)) {
                break;
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_events_are_delivered_and_exit_stops_the_loop() {
        // No display or GTK widgets needed to exercise GLib/channel dispatch.
        let (event_loop, ui) = create().unwrap();
        ui.set_paused(true);
        ui.show_log();
        let stop = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            ui.exit(); // Must wake the main context after it starts waiting for messages.
        });
        let mut started = 0;
        let mut events = Vec::new();
        run(event_loop, |event| {
            match event {
                None => started += 1,
                Some(UiEvent::Paused(true)) => events.push("paused"),
                Some(UiEvent::ShowLog) => events.push("log"),
                Some(UiEvent::Exit) => {
                    events.push("exit");
                    return true;
                }
                other => panic!("unexpected event: {other:?}"),
            }
            false
        })
        .unwrap();
        assert_eq!(started, 1);
        assert_eq!(events, ["paused", "log", "exit"]);
        stop.join().unwrap();
    }
}
