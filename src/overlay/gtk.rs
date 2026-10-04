//! Linux: a GTK popup on X11 or a Layer Shell surface on Wayland, painted from the software-rendered frame
//! and made click-through with an empty input shape. A timer drives the animation while it's on screen.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{cairo, gdk, glib};
use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use super::paint::Canvas;
use super::scene::{CANVAS_H, CANVAS_W, Scene};
use super::{Feedback, OverlayStatus};

const FRAME: Duration = Duration::from_millis(16);

struct Inner {
    window: gtk::Window,
    scene: Scene,
    canvas: Canvas,
    /// Device pixels per GTK pixel on the overlay's monitor.
    scale: i32,
    /// Shown, with the animation timer running.
    active: bool,
}

impl Inner {
    /// Layer Shell chooses the output and placement; X11 uses the monitor under the mouse.
    fn place(&mut self) {
        if self.window.is_layer_window() {
            self.resize_canvas(self.window.scale_factor());
            return;
        }
        let Some(display) = gdk::Display::default() else {
            return;
        };
        let under_mouse = display
            .default_seat()
            .and_then(|seat| seat.pointer())
            .and_then(|pointer| {
                let (_, x, y) = pointer.position();
                display.monitor_at_point(x, y)
            });
        let Some(monitor) = under_mouse
            .or_else(|| display.primary_monitor())
            .or_else(|| display.monitor(0))
        else {
            return;
        };
        let area = monitor.workarea();
        let scale = monitor.scale_factor().max(1);
        let (w, h) = (CANVAS_W as i32, CANVAS_H as i32);
        self.resize_canvas(scale);
        self.window.move_(
            area.x() + (area.width() - w) / 2,
            area.y() + area.height() - h,
        );
    }

    fn resize_canvas(&mut self, scale: i32) {
        let scale = scale.max(1);
        let (w, h) = (
            (CANVAS_W as i32 * scale) as usize,
            (CANVAS_H as i32 * scale) as usize,
        );
        if self.scale != scale || self.canvas.width != w || self.canvas.height != h {
            self.scale = scale;
            self.canvas.resize(w, h);
        }
    }

    fn paint(&mut self, cr: &cairo::Context) {
        // Wayland resolves the output/scale when mapping, and can change it while the window is visible.
        self.resize_canvas(self.window.scale_factor());
        let scale = self.scale as f32;
        self.scene.draw(&mut self.canvas, scale, Instant::now());
        let (w, h) = (self.canvas.width as i32, self.canvas.height as i32);
        // cairo's ARGB32 is premultiplied and native-endian: the canvas' BGRA bytes on little-endian machines.
        let Ok(surface) = cairo::ImageSurface::create_for_data(
            self.canvas.pixels.clone(),
            cairo::Format::ARgb32,
            w,
            h,
            w * 4,
        ) else {
            return;
        };
        surface.set_device_scale(scale as f64, scale as f64);
        cr.set_operator(cairo::Operator::Source);
        if cr.set_source_surface(&surface, 0.0, 0.0).is_ok() {
            let _ = cr.paint();
        }
    }
}

fn create() -> Result<Rc<RefCell<Inner>>, String> {
    let display = gdk::Display::default().ok_or("no display")?;
    let wayland = display.type_().name().contains("Wayland");
    if wayland && !gtk_layer_shell::is_supported() {
        return Err(
            "The recording overlay isn't shown: this Wayland compositor does not support Layer Shell (zwlr_layer_shell_v1)."
                .into(),
        );
    }
    let screen = display.default_screen();
    let visual = screen
        .rgba_visual()
        .filter(|_| wayland || screen.is_composited())
        .ok_or("The recording overlay isn't shown: it needs a compositor for transparency.")?;

    let window = gtk::Window::new(if wayland {
        gtk::WindowType::Toplevel
    } else {
        gtk::WindowType::Popup
    });
    if wayland {
        // Initialize before realization. No explicit monitor: the compositor chooses the output.
        window.init_layer_shell();
        window.set_namespace("wisprcheap-overlay");
        window.set_layer(Layer::Overlay);
        window.set_anchor(Edge::Bottom, true);
        window.set_exclusive_zone(0); // Respect panels without reserving space for the pill.
        window.set_keyboard_mode(KeyboardMode::None);
    } else {
        window.set_keep_above(true);
        window.set_type_hint(gdk::WindowTypeHint::Notification);
    }
    window.set_title("wisprcheap overlay");
    window.set_visual(Some(&visual));
    window.set_app_paintable(true);
    window.set_decorated(false);
    window.set_accept_focus(false);
    window.set_focus_on_map(false);
    window.set_skip_taskbar_hint(true);
    window.set_skip_pager_hint(true);
    window.set_size_request(CANVAS_W as i32, CANVAS_H as i32);
    // An empty input shape: clicks go to the windows below.
    window.input_shape_combine_region(Some(&cairo::Region::create()));

    let inner = Rc::new(RefCell::new(Inner {
        window: window.clone(),
        scene: Scene::new(Instant::now()),
        canvas: Canvas::new(0, 0),
        scale: 1,
        active: false,
    }));
    let weak = Rc::downgrade(&inner);
    window.connect_draw(move |_, cr| {
        if let Some(inner) = weak.upgrade()
            && let Ok(mut inner) = inner.try_borrow_mut()
        {
            inner.paint(cr);
        }
        glib::Propagation::Stop
    });
    Ok(inner)
}

/// Show the window and start the animation if the scene has something to show.
fn wake(inner: &Rc<RefCell<Inner>>) {
    let window = {
        let Ok(mut s) = inner.try_borrow_mut() else {
            return;
        };
        let now = Instant::now();
        if s.active || !s.scene.on_screen(now) {
            return;
        }
        s.place();
        s.scene.tick(now, crate::recorder::take_level());
        s.active = true;
        s.window.clone()
    };
    window.show();
    let weak = Rc::downgrade(inner);
    glib::timeout_add_local(FRAME, move || {
        let Some(inner) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let Ok(mut s) = inner.try_borrow_mut() else {
            return glib::ControlFlow::Continue;
        };
        let now = Instant::now();
        s.scene.tick(now, crate::recorder::take_level());
        if s.scene.on_screen(now) {
            s.window.queue_draw();
            return glib::ControlFlow::Continue;
        }
        s.active = false;
        let window = s.window.clone();
        drop(s);
        window.hide();
        glib::ControlFlow::Break
    });
}

/// The overlay, owned by the UI thread (tao's GTK main loop).
#[derive(Default)]
pub struct Overlay {
    inner: Option<Rc<RefCell<Inner>>>,
    /// The window can't be shown here (unsupported compositor or no transparency): don't try again.
    unavailable: bool,
}

impl Overlay {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_status(&mut self, status: OverlayStatus) {
        if self.inner.is_none() {
            if !status.enabled || self.unavailable {
                return; // no window until it's needed
            }
            match create() {
                Ok(inner) => self.inner = Some(inner),
                Err(why) => {
                    self.unavailable = true;
                    crate::info!("[overlay] {why}");
                    return;
                }
            }
        }
        if let Some(inner) = &self.inner {
            if let Ok(mut s) = inner.try_borrow_mut() {
                s.scene.set_status(status);
            }
            wake(inner);
        }
    }

    pub fn feedback(&mut self, feedback: Feedback) {
        if let Some(inner) = &self.inner {
            if let Ok(mut s) = inner.try_borrow_mut() {
                s.scene.feedback(feedback, Instant::now());
            }
            wake(inner);
        }
    }

    pub fn destroy(&mut self) {
        if let Some(inner) = self.inner.take()
            && let Ok(s) = inner.try_borrow()
        {
            s.window.hide();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::Mode;
    use crate::overlay::Recording;
    use std::cell::Cell;
    use std::process::Command;

    fn settle(ms: u64) {
        let context = glib::MainContext::default();
        let until = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < until {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn sway(command: &str) {
        let output = Command::new("swaymsg").arg(command).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    fn capture(name: &str) {
        if let Some(dir) = std::env::var_os("WISPRCHEAP_OVERLAY_TEST_DIR") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            assert!(
                Command::new("grim")
                    .arg(dir.join(format!("{name}.png")))
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    /// Run in an isolated Sway session with GDK_BACKEND=wayland; optionally export screenshots with grim.
    /// Set WISPRCHEAP_OVERLAY_EXPECT_UNSUPPORTED=1 in a compositor without Layer Shell to check fallback.
    #[test]
    #[ignore = "requires an isolated native Wayland compositor"]
    fn wayland_overlay_backend() {
        gtk::init().unwrap();
        let display = gdk::Display::default().unwrap();
        assert!(display.type_().name().contains("Wayland"));
        let mut overlay = Overlay::new();
        overlay.set_status(OverlayStatus::default());
        assert!(overlay.inner.is_none());
        let mut status = OverlayStatus {
            enabled: true,
            recording: Some(Recording {
                mode: Mode::Dictation,
                hands_free: false,
            }),
            busy: false,
        };
        if std::env::var_os("WISPRCHEAP_OVERLAY_EXPECT_UNSUPPORTED").is_some() {
            assert!(!gtk_layer_shell::is_supported());
            overlay.set_status(status);
            assert!(overlay.unavailable && overlay.inner.is_none());
            assert!(
                create()
                    .err()
                    .unwrap()
                    .contains("does not support Layer Shell")
            );
            overlay.feedback(Feedback::Error);
            overlay.destroy();
            return;
        }
        assert!(gtk_layer_shell::is_supported());

        // A real window below the overlay must retain focus and receive clicks through the pill.
        let fixture = gtk::Window::new(gtk::WindowType::Toplevel);
        fixture.set_title("Overlay integration fixture");
        let area = gtk::DrawingArea::new();
        area.connect_draw(|_, cr| {
            cr.set_source_rgb(0.12, 0.20, 0.29);
            cr.paint().unwrap();
            glib::Propagation::Stop
        });
        area.add_events(gdk::EventMask::BUTTON_PRESS_MASK);
        let clicks = Rc::new(Cell::new(0));
        let clicked = clicks.clone();
        area.connect_button_press_event(move |_, _| {
            clicked.set(clicked.get() + 1);
            glib::Propagation::Stop
        });
        fixture.add(&area);
        fixture.show_all();
        settle(400);
        assert!(fixture.is_active());
        capture("background");

        overlay.set_status(status);
        settle(400);
        let window = overlay.inner.as_ref().unwrap().borrow().window.clone();
        assert!(window.is_layer_window() && window.is_mapped());
        assert_eq!(window.layer(), Layer::Overlay);
        assert!(window.is_anchor(Edge::Bottom));
        assert!(!window.is_anchor(Edge::Left) && !window.is_anchor(Edge::Right));
        assert_eq!(window.keyboard_mode(), KeyboardMode::None);
        assert_eq!(window.exclusive_zone(), 0);
        assert!(fixture.is_active() && !window.is_active());
        capture("recording");
        let monitor = display
            .monitor_at_window(&window.window().unwrap())
            .unwrap();
        let geometry = monitor.geometry();
        sway(&format!(
            "seat seat0 cursor set {} {}",
            geometry.width() / 2,
            geometry.height() - 36
        ));
        sway("seat seat0 cursor press button1");
        settle(100);
        sway("seat seat0 cursor release button1");
        settle(100);
        assert_eq!(
            clicks.get(),
            1,
            "click in the pill must reach the window below"
        );

        status.recording = Some(Recording {
            mode: Mode::Command,
            hands_free: true,
        });
        overlay.set_status(status);
        settle(350);
        capture("command-hands-free");
        status.recording = None;
        status.busy = true;
        overlay.set_status(status);
        settle(350);
        capture("processing");
        status.busy = false;
        overlay.set_status(status);
        overlay.feedback(Feedback::Pasted);
        settle(250);
        capture("feedback");
        settle(1500);
        assert!(!window.is_mapped() && !overlay.inner.as_ref().unwrap().borrow().active);
        capture("hidden");

        status.recording = Some(Recording {
            mode: Mode::Dictation,
            hands_free: false,
        });
        overlay.set_status(status);
        settle(400);
        assert!(window.is_mapped() && fixture.is_active());
        capture("reopened");
        // Scale changes after mapping must resize the native pixel buffer and keep the pill sharp.
        sway("output * scale 2");
        settle(400);
        {
            let inner = overlay.inner.as_ref().unwrap().borrow();
            assert_eq!(inner.scale, 2);
            assert_eq!((inner.canvas.width, inner.canvas.height), (400, 144));
        }
        capture("scale-2");
        overlay.destroy();
        settle(100);
        assert!(!window.is_mapped());
        fixture.close();
    }
}
