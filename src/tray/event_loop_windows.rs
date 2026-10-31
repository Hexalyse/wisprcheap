//! Winit pumps the Win32 messages used by the native tray, overlay and log windows.

use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::WindowId;

use super::{Ui, UiEvent};

pub struct EventLoop(winit::event_loop::EventLoop<UiEvent>);

pub fn create() -> anyhow::Result<(EventLoop, Ui)> {
    from_native(winit::event_loop::EventLoop::<UiEvent>::with_user_event().build()?)
}

fn from_native(
    event_loop: winit::event_loop::EventLoop<UiEvent>,
) -> anyhow::Result<(EventLoop, Ui)> {
    let proxy = event_loop.create_proxy();
    let ui = Ui {
        send: Arc::new(move |event| {
            let _ = proxy.send_event(event);
        }),
    };
    Ok((EventLoop(event_loop), ui))
}

pub fn run(
    event_loop: EventLoop,
    callback: impl FnMut(Option<UiEvent>) -> bool,
) -> anyhow::Result<()> {
    struct Handler<F> {
        callback: F,
        started: bool,
    }
    impl<F: FnMut(Option<UiEvent>) -> bool> ApplicationHandler<UiEvent> for Handler<F> {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            event_loop.set_control_flow(ControlFlow::Wait);
            if !self.started {
                self.started = true;
                if (self.callback)(None) {
                    event_loop.exit();
                }
            }
        }
        fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UiEvent) {
            if (self.callback)(Some(event)) {
                event_loop.exit();
            }
        }
        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }
    event_loop.0.run_app(&mut Handler {
        callback,
        started: false,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::platform::windows::EventLoopBuilderExtWindows;

    #[test]
    fn queued_events_are_delivered_and_exit_stops_the_loop() {
        let native = winit::event_loop::EventLoop::<UiEvent>::with_user_event()
            .with_any_thread(true)
            .build()
            .unwrap();
        let (event_loop, ui) = from_native(native).unwrap();
        ui.set_paused(true);
        ui.show_log();
        let stop = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            ui.exit(); // Wake the native event loop from the background thread.
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
