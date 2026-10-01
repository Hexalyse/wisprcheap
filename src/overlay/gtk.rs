//! Linux: a GTK popup (override-redirect on X11) with an RGBA visual, painted from the software-rendered frame
//! and made click-through with an empty input shape. A timer drives the animation while it's on screen.
//!
//! Needs X11 and a compositor (for the transparency). On Wayland, apps can't place their windows (that would
//! take the layer-shell protocol), so the overlay stays off there.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{cairo, gdk, glib};

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
    /// Bottom center of the work area of the monitor under the mouse.
    fn place(&mut self) {
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
        self.scale = scale;
        self.canvas
            .resize((w * scale) as usize, (h * scale) as usize);
        self.window.move_(
            area.x() + (area.width() - w) / 2,
            area.y() + area.height() - h,
        );
    }

    fn paint(&mut self, cr: &cairo::Context) {
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
    if display.type_().name().contains("Wayland") {
        return Err(
            "The recording overlay isn't shown on Wayland, which doesn't let apps place their windows."
                .into(),
        );
    }
    let screen = display.default_screen();
    let visual = screen
        .rgba_visual()
        .filter(|_| screen.is_composited())
        .ok_or("The recording overlay isn't shown: it needs a compositor for transparency.")?;

    let window = gtk::Window::new(gtk::WindowType::Popup);
    window.set_title("wisprcheap overlay");
    window.set_visual(Some(&visual));
    window.set_app_paintable(true);
    window.set_decorated(false);
    window.set_accept_focus(false);
    window.set_focus_on_map(false);
    window.set_keep_above(true);
    window.set_skip_taskbar_hint(true);
    window.set_skip_pager_hint(true);
    window.set_type_hint(gdk::WindowTypeHint::Notification);
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
    /// The window can't be shown here (Wayland, no compositor): don't try again.
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
