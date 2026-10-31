//! GTK4 surface host for the existing software-rendered overlay.
//! Wayland uses Layer Shell; X11 positioning/stacking uses a small native adapter.

use super::paint::Canvas;
use super::scene::{CANVAS_H, CANVAS_W, Scene};
use super::{Feedback, OverlayStatus, x11};
use gtk::prelude::*;
use gtk::{cairo, gdk, glib};
use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

const FRAME: Duration = Duration::from_millis(16);

struct Inner {
    window: gtk::Window,
    area: gtk::DrawingArea,
    x11: Option<x11::Window>,
    scene: Scene,
    canvas: Canvas,
    scale: i32,
    active: bool,
}

impl Inner {
    fn place(&mut self) {
        self.resize_canvas(self.window.scale_factor());
        let Some(native) = &self.x11 else { return };
        let pointer = native.pointer().ok();
        let monitors = gtk::prelude::WidgetExt::display(&self.window).monitors();
        let monitors: Vec<gdk::Monitor> = (0..monitors.n_items())
            .filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>())
            .collect();
        let monitor = monitors
            .iter()
            .find(|monitor| {
                let g = monitor.geometry();
                let scale = monitor.scale_factor().max(1);
                pointer.is_some_and(|(x, y)| {
                    x >= g.x() * scale
                        && x < (g.x() + g.width()) * scale
                        && y >= g.y() * scale
                        && y < (g.y() + g.height()) * scale
                })
            })
            .or_else(|| monitors.first());
        let Some(monitor) = monitor else { return };
        let area = monitor
            .downcast_ref::<gdk4_x11::X11Monitor>()
            .map(|monitor| monitor.workarea())
            .unwrap_or_else(|| monitor.geometry());
        let scale = monitor.scale_factor().max(1);
        let (width, height) = (CANVAS_W as i32, CANVAS_H as i32);
        if let Err(e) = native.place(
            (area.x() + (area.width() - width) / 2) * scale,
            (area.y() + area.height() - height) * scale,
            (width * scale) as u32,
            (height * scale) as u32,
        ) {
            crate::warn!("[overlay] Could not position X11 surface: {e}");
        }
        self.resize_canvas(scale);
    }

    fn resize_canvas(&mut self, scale: i32) {
        let scale = scale.max(1);
        let (width, height) = (
            (CANVAS_W as i32 * scale) as usize,
            (CANVAS_H as i32 * scale) as usize,
        );
        if self.scale != scale || self.canvas.width != width || self.canvas.height != height {
            self.scale = scale;
            self.canvas.resize(width, height);
        }
    }

    fn paint(&mut self, cr: &cairo::Context) {
        self.resize_canvas(self.window.scale_factor());
        let scale = self.scale as f32;
        self.scene.draw(&mut self.canvas, scale, Instant::now());
        let (width, height) = (self.canvas.width as i32, self.canvas.height as i32);
        let Ok(surface) = cairo::ImageSurface::create_for_data(
            self.canvas.pixels.clone(),
            cairo::Format::ARgb32,
            width,
            height,
            width * 4,
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
    let is_x11 = display.is::<gdk4_x11::X11Display>();
    if !is_x11 && !gtk_layer_shell::is_supported() {
        return Err("The recording overlay isn't shown: this Wayland compositor does not support Layer Shell (zwlr_layer_shell_v1).".into());
    }
    if !display.is_rgba() || (is_x11 && !display.is_composited()) {
        return Err(
            "The recording overlay isn't shown: it needs a compositor for transparency.".into(),
        );
    }
    let window = gtk::Window::builder()
        .title("wisprcheap overlay")
        .decorated(false)
        .resizable(false)
        .focusable(false)
        .default_width(CANVAS_W as i32)
        .default_height(CANVAS_H as i32)
        .build();
    window.add_css_class("wisprcheap-overlay");
    let css = gtk::CssProvider::new();
    css.load_from_data("window.wisprcheap-overlay { background: transparent; box-shadow: none; }");
    gtk::style_context_add_provider_for_display(
        &display,
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    if !is_x11 {
        window.init_layer_shell();
        window.set_namespace(Some("wisprcheap-overlay"));
        window.set_layer(Layer::Overlay);
        window.set_anchor(Edge::Bottom, true);
        window.set_exclusive_zone(0);
        window.set_keyboard_mode(KeyboardMode::None);
    }
    let area = gtk::DrawingArea::new();
    area.set_content_width(CANVAS_W as i32);
    area.set_content_height(CANVAS_H as i32);
    area.set_can_target(false);
    window.set_child(Some(&area));
    // Apply native policy before mapping; present() would request focus.
    gtk::prelude::WidgetExt::realize(&window);
    let surface = window
        .surface()
        .ok_or("could not create the native overlay surface")?;
    surface.set_input_region(Some(&cairo::Region::create()));
    let native = if is_x11 {
        let xid = surface
            .downcast_ref::<gdk4_x11::X11Surface>()
            .ok_or("could not access the X11 overlay surface")?
            .xid();
        Some(
            x11::Window::new(u32::try_from(xid).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let inner = Rc::new(RefCell::new(Inner {
        window: window.clone(),
        area: area.clone(),
        x11: native,
        scene: Scene::new(Instant::now()),
        canvas: Canvas::new(0, 0),
        scale: 1,
        active: false,
    }));
    let weak = Rc::downgrade(&inner);
    area.set_draw_func(move |_, cr, _, _| {
        if let Some(inner) = weak.upgrade()
            && let Ok(mut inner) = inner.try_borrow_mut()
        {
            inner.paint(cr);
        }
    });
    let weak = Rc::downgrade(&inner);
    window.connect_map(move |window| {
        if let Some(surface) = window.surface() {
            surface.set_input_region(Some(&cairo::Region::create()));
        }
        if let Some(inner) = weak.upgrade()
            && let Ok(mut inner) = inner.try_borrow_mut()
        {
            inner.place();
        }
    });
    Ok(inner)
}

fn wake(inner: &Rc<RefCell<Inner>>) {
    let window = {
        let Ok(mut state) = inner.try_borrow_mut() else {
            return;
        };
        let now = Instant::now();
        if state.active || !state.scene.on_screen(now) {
            return;
        }
        state.place();
        state.scene.tick(now, crate::recorder::take_level());
        state.active = true;
        state.window.clone()
    };
    window.show();
    let weak = Rc::downgrade(inner);
    glib::timeout_add_local(FRAME, move || {
        let Some(inner) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let Ok(mut state) = inner.try_borrow_mut() else {
            return glib::ControlFlow::Continue;
        };
        let now = Instant::now();
        state.scene.tick(now, crate::recorder::take_level());
        if state.scene.on_screen(now) {
            state.area.queue_draw();
            return glib::ControlFlow::Continue;
        }
        state.active = false;
        let window = state.window.clone();
        drop(state);
        window.hide();
        glib::ControlFlow::Break
    });
}

#[derive(Default)]
pub struct Overlay {
    inner: Option<Rc<RefCell<Inner>>>,
    unavailable: bool,
}

impl Overlay {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_status(&mut self, status: OverlayStatus) {
        if self.inner.is_none() {
            if !status.enabled || self.unavailable {
                return;
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
            if let Ok(mut state) = inner.try_borrow_mut() {
                state.scene.set_status(status);
            }
            wake(inner);
        }
    }

    pub fn feedback(&mut self, feedback: Feedback) {
        if let Some(inner) = &self.inner {
            if let Ok(mut state) = inner.try_borrow_mut() {
                state.scene.feedback(feedback, Instant::now());
            }
            wake(inner);
        }
    }

    pub fn destroy(&mut self) {
        if let Some(inner) = self.inner.take()
            && let Ok(state) = inner.try_borrow()
        {
            state.window.destroy();
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

    #[test]
    #[ignore = "requires an isolated composited X11 server"]
    fn x11_overlay_backend() {
        use x11rb::connection::Connection;
        use x11rb::protocol::xproto::ConnectionExt;
        gtk::init().unwrap();
        let display = gdk::Display::default().unwrap();
        assert!(display.is::<gdk4_x11::X11Display>());
        let (connection, screen) = x11rb::connect(None).unwrap();
        let root = connection.setup().roots[screen].root;
        let size = connection.get_geometry(root).unwrap().reply().unwrap();
        let fixture = gtk::Window::builder()
            .title("Overlay integration fixture")
            .decorated(false)
            .default_width(size.width.into())
            .default_height(size.height.into())
            .build();
        fixture.fullscreen();
        let area = gtk::DrawingArea::new();
        area.set_draw_func(|_, cr, _, _| {
            cr.set_source_rgb(0.12, 0.20, 0.29);
            cr.paint().unwrap();
        });
        let clicks = Rc::new(Cell::new(0));
        let clicked = clicks.clone();
        let gesture = gtk::GestureClick::new();
        gesture.connect_pressed(move |_, _, _, _| clicked.set(clicked.get() + 1));
        area.add_controller(gesture);
        fixture.set_child(Some(&area));
        fixture.present();
        settle(500);
        assert!(fixture.is_active());
        let focus = connection.get_input_focus().unwrap().reply().unwrap().focus;
        let mut overlay = Overlay::new();
        let status = OverlayStatus {
            enabled: true,
            recording: Some(Recording {
                mode: Mode::Dictation,
                hands_free: false,
            }),
            busy: false,
        };
        overlay.set_status(status);
        settle(400);
        let window = overlay
            .inner
            .as_ref()
            .expect("overlay available")
            .borrow()
            .window
            .clone();
        assert!(window.is_mapped());
        assert!(fixture.is_active());
        assert_eq!(
            connection.get_input_focus().unwrap().reply().unwrap().focus,
            focus
        );
        let xid = window
            .surface()
            .unwrap()
            .downcast::<gdk4_x11::X11Surface>()
            .unwrap()
            .xid() as u32;
        let attributes = connection
            .get_window_attributes(xid)
            .unwrap()
            .reply()
            .unwrap();
        assert!(attributes.override_redirect);
        let geometry = connection.get_geometry(xid).unwrap().reply().unwrap();
        let scale = window.scale_factor();
        assert_eq!(geometry.width as i32, CANVAS_W as i32 * scale);
        assert_eq!(geometry.height as i32, CANVAS_H as i32 * scale);
        assert_eq!(
            geometry.x as i32,
            (size.width as i32 - geometry.width as i32) / 2
        );
        assert_eq!(
            geometry.y as i32,
            size.height as i32 - geometry.height as i32
        );
        assert!(
            Command::new("xdotool")
                .args([
                    "mousemove",
                    "--sync",
                    &(size.width / 2).to_string(),
                    &(size.height - 36 * scale as u16).to_string(),
                    "click",
                    "1"
                ])
                .status()
                .unwrap()
                .success()
        );
        settle(200);
        assert_eq!(
            clicks.get(),
            1,
            "overlay must pass pointer events to the fixture"
        );
        assert!(fixture.is_active());
        overlay.set_status(OverlayStatus {
            enabled: true,
            ..Default::default()
        });
        overlay.feedback(Feedback::Pasted);
        settle(1800);
        assert!(!window.is_mapped());
        overlay.set_status(status);
        settle(400);
        assert!(window.is_mapped() && fixture.is_active());
        assert_eq!(
            connection.get_input_focus().unwrap().reply().unwrap().focus,
            focus
        );
        overlay.destroy();
        settle(100);
        assert!(!window.is_mapped());
        fixture.destroy();
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
        let fixture = gtk::Window::new();
        fixture.set_title(Some("Overlay integration fixture"));
        let area = gtk::DrawingArea::new();
        area.set_draw_func(|_, cr, _, _| {
            cr.set_source_rgb(0.12, 0.20, 0.29);
            cr.paint().unwrap();
        });
        let clicks = Rc::new(Cell::new(0));
        let clicked = clicks.clone();
        let gesture = gtk::GestureClick::new();
        gesture.connect_pressed(move |_, _, _, _| {
            clicked.set(clicked.get() + 1);
        });
        area.add_controller(gesture);
        fixture.set_child(Some(&area));
        fixture.present();
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
            .monitor_at_surface(&window.surface().unwrap())
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
