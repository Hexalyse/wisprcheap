//! GTK4 surface host for the existing software-rendered overlay, and for the edge glow.
//! Wayland uses Layer Shell; X11 positioning/stacking uses a small native adapter.

use super::glow::{GlowFrame, GlowScene};
use super::paint::Canvas;
use super::scene::{CANVAS_H, CANVAS_W, Scene};
use super::{Feedback, OverlayStatus, OverlayStyle, x11};
use crate::hyprland;
use gtk::prelude::*;
use gtk::{cairo, gdk, glib, graphene, gsk};
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
        let Some(monitor) = pointer_monitor(&self.window, native) else {
            return;
        };
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

/// X11: the monitor under the pointer (where the user is working), or the first one.
fn pointer_monitor(window: &gtk::Window, native: &x11::Window) -> Option<gdk::Monitor> {
    let pointer = native.pointer().ok();
    let monitors = gtk::prelude::WidgetExt::display(window).monitors();
    let monitors: Vec<gdk::Monitor> = (0..monitors.n_items())
        .filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>())
        .collect();
    monitors
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
        .or_else(|| monitors.first())
        .cloned()
}

/// The display, when it can show a transparent overlay (Layer Shell on Wayland, a compositor on X11).
fn overlay_display() -> Result<(gdk::Display, bool), String> {
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
    Ok((display, is_x11))
}

/// Native policy for an overlay surface: no input (clicks go through), and on X11 never focused or managed.
fn realize_overlay(window: &gtk::Window, is_x11: bool) -> Result<Option<x11::Window>, String> {
    // Apply native policy before mapping; present() would request focus.
    gtk::prelude::WidgetExt::realize(window);
    let surface = window
        .surface()
        .ok_or("could not create the native overlay surface")?;
    surface.set_input_region(Some(&cairo::Region::create()));
    if !is_x11 {
        return Ok(None);
    }
    let xid = surface
        .downcast_ref::<gdk4_x11::X11Surface>()
        .ok_or("could not access the X11 overlay surface")?
        .xid();
    Ok(Some(
        x11::Window::new(u32::try_from(xid).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?,
    ))
}

fn create() -> Result<Rc<RefCell<Inner>>, String> {
    let (display, is_x11) = overlay_display()?;
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
    css.load_from_string(
        "window.wisprcheap-overlay { background: transparent; box-shadow: none; }",
    );
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
    let native = realize_overlay(&window, is_x11)?;
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
    window.set_visible(true);
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
        window.set_visible(false);
        glib::ControlFlow::Break
    });
}

// ---------------------------------------------------------------------------
// Edge glow
// ---------------------------------------------------------------------------

/// Draws `GlowFrame`s with GSK gradients (on the GPU), sized by its window.
mod glow_area {
    use super::super::glow::GlowFrame;
    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use std::cell::Cell;

    #[derive(Default)]
    pub struct Imp {
        pub frame: Cell<Option<GlowFrame>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Imp {
        const NAME: &'static str = "WisprcheapGlowArea";
        type Type = super::GlowArea;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Imp {}

    impl WidgetImpl for Imp {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(frame) = self.frame.get() {
                let widget = self.obj();
                super::draw_glow(
                    snapshot,
                    WidgetExt::width(&*widget) as f32,
                    WidgetExt::height(&*widget) as f32,
                    &frame,
                );
            }
        }
    }
}

glib::wrapper! {
    pub struct GlowArea(ObjectSubclass<glow_area::Imp>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl GlowArea {
    fn new() -> Self {
        glib::Object::new()
    }

    fn set_frame(&self, frame: GlowFrame) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        self.imp().frame.set(Some(frame));
        self.queue_draw();
    }
}

/// The wave on the bottom edge at `x`: long, gentle swells travelling to the right as `t` advances.
fn wave(x: f32, t: f32, amplitude: f32) -> f32 {
    amplitude
        * (0.6 * (x / 95.0 - 1.6 * t).sin()
            + 0.28 * (x / 48.0 - 2.3 * t + 1.7).sin()
            + 0.12 * (x / 160.0 - 0.9 * t + 4.1).sin())
}

/// The glow's shape: one continuous band along the bottom edge and up both side edges, its inner outline
/// following the wave along the bottom, curving around the corners, and tapering to nothing as it climbs.
/// `scale` makes the band thinner (and the sides shorter), for the brighter layers near the edges.
fn glow_outline(
    w: f32,
    h: f32,
    t: f32,
    base: f32,
    amplitude: f32,
    reach: f32,
    scale: f32,
) -> gsk::Path {
    let band = base * scale;
    let radius = band * 1.6;
    let reach = reach * (0.55 + 0.45 * scale.min(1.0));
    let corner = band + radius;
    // the wave flattens out near the corners, so the band meets the sides at its base thickness
    let bottom = |x: f32| {
        let ramp = ((x.min(w - x) - corner) / (3.0 * corner)).clamp(0.0, 1.0);
        let ramp = ramp * ramp * (3.0 - 2.0 * ramp);
        h - band - scale * wave(x, t, amplitude) * ramp
    };
    // side thickness at height `u` above the corner's curve: full, easing to nothing at the top
    let side = |u: f32| {
        let p = (u / (reach - corner).max(1.0)).clamp(0.0, 1.0);
        band * (1.0 - p * p * (3.0 - 2.0 * p))
    };
    let path = gsk::PathBuilder::new();
    path.move_to(-20.0, h + 20.0);
    path.line_to(-20.0, h - reach - 20.0);
    path.line_to(0.0, h - reach);
    // down the left side
    let steps = 40;
    for i in 0..=steps {
        let u = (reach - corner) * (1.0 - i as f32 / steps as f32);
        path.line_to(side(u), h - corner - u);
    }
    // left inner corner: a quarter circle from the side into the bottom band
    for i in 1..=12 {
        let a = std::f32::consts::FRAC_PI_2 * i as f32 / 12.0;
        path.line_to(corner - radius * a.cos(), h - corner + radius * a.sin());
    }
    // along the bottom
    let mut x = corner;
    while x < w - corner {
        path.line_to(x, bottom(x));
        x += 5.0;
    }
    // right inner corner, then up the right side
    for i in 0..=12 {
        let a = std::f32::consts::FRAC_PI_2 * i as f32 / 12.0;
        path.line_to(w - corner + radius * a.sin(), h - corner + radius * a.cos());
    }
    for i in 0..=steps {
        let u = (reach - corner) * i as f32 / steps as f32;
        path.line_to(w - side(u), h - corner - u);
    }
    path.line_to(w, h - reach);
    path.line_to(w + 20.0, h - reach - 20.0);
    path.line_to(w + 20.0, h + 20.0);
    path.close();
    path.to_path()
}

/// One frame of the glow over a `w` x `h` window: one continuous band of light along the bottom and up the
/// sides, with a wave on top. It's drawn three times, from wide and faint to thin and bright, so it's
/// brightest right at the window's edges and fades inward, and each layer is blurred on the GPU.
fn draw_glow(s: &gtk::Snapshot, w: f32, h: f32, f: &GlowFrame) {
    if w <= 0.0 || h <= 0.0 || f.opacity <= 0.0 {
        return;
    }
    let [c1, c2, c3] = f.colors;
    let (t, swell) = (f.time, f.swell);
    s.push_opacity(f.opacity as f64);

    // Thickness of the band and height of its wave: low, higher with the voice.
    let base = 10.0 + 10.0 * swell;
    let amplitude = 4.0 + 7.0 * swell;
    let reach = (h * (0.3 + 0.08 * swell)).min(h);
    // everything the outlines can cover (they never climb above `reach`)
    let bounds = graphene::Rect::new(-30.0, h - reach - 30.0, w + 60.0, reach + 60.0);
    // (thickness, color, opacity, blur)
    for (scale, color, alpha, blur) in [
        (1.6, c2, 0.22, 12.0),
        (0.75, c1, 0.25, 6.0),
        (0.25, c3, 0.5, 2.0),
    ] {
        let outline = glow_outline(w, h, t, base, amplitude, reach, scale);
        s.push_blur(blur);
        s.push_fill(&outline, gsk::FillRule::Winding);
        s.append_color(&gdk::RGBA::new(color.r, color.g, color.b, alpha), &bounds);
        s.pop();
        s.pop();
    }
    s.pop();
}

/// Where the glow is drawn.
#[derive(Debug, Clone, PartialEq)]
enum Placement {
    /// Over Hyprland's active window.
    Window(hyprland::ActiveWindow),
    /// The whole output (no focused window, another compositor, or X11).
    Output,
}

/// How often the glow follows the focus while it's on screen.
const FOLLOW_FRAMES: u32 = 15;

struct Glow {
    window: gtk::Window,
    area: GlowArea,
    x11: Option<x11::Window>,
    scene: GlowScene,
    placement: Option<Placement>,
    active: bool,
    frames: u32,
}

impl Glow {
    /// Wayland: over Hyprland's active window when there is one, else over the whole output (the compositor
    /// picks it). X11: the monitor under the pointer.
    fn place(&mut self) {
        if let Some(native) = &self.x11 {
            let Some(monitor) = pointer_monitor(&self.window, native) else {
                return;
            };
            let g = monitor.geometry();
            let scale = monitor.scale_factor().max(1);
            if let Err(e) = native.place(
                g.x() * scale,
                g.y() * scale,
                (g.width() * scale) as u32,
                (g.height() * scale) as u32,
            ) {
                crate::warn!("[overlay] Could not position the X11 edge glow: {e}");
            }
            return;
        }
        let placement = hyprland::active_window()
            .map(Placement::Window)
            .unwrap_or(Placement::Output);
        if self.placement.as_ref() == Some(&placement) {
            return;
        }
        // A mapped GTK window keeps its size: remap it to apply a new one.
        let mapped = self.window.is_mapped();
        if mapped {
            self.window.set_visible(false);
        }
        match &placement {
            Placement::Window(active) => {
                let monitor = monitor_named(&self.window, &active.monitor);
                self.window.set_monitor(monitor.as_ref());
                self.window.set_anchor(Edge::Top, true);
                self.window.set_anchor(Edge::Left, true);
                self.window.set_anchor(Edge::Bottom, false);
                self.window.set_anchor(Edge::Right, false);
                self.window.set_margin(Edge::Top, active.y);
                self.window.set_margin(Edge::Left, active.x);
                self.window.set_default_size(active.width, active.height);
            }
            Placement::Output => {
                self.window.set_monitor(None);
                for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                    self.window.set_anchor(edge, true);
                    self.window.set_margin(edge, 0);
                }
            }
        }
        self.placement = Some(placement);
        if mapped {
            self.window.set_visible(true);
        }
    }

    fn render(&self) {
        self.area.set_frame(self.scene.frame());
    }
}

/// The GDK monitor of an output name (`eDP-1`...).
fn monitor_named(window: &gtk::Window, name: &str) -> Option<gdk::Monitor> {
    let monitors = gtk::prelude::WidgetExt::display(window).monitors();
    (0..monitors.n_items())
        .filter_map(|i| monitors.item(i).and_downcast::<gdk::Monitor>())
        .find(|m| m.connector().is_some_and(|c| c == name))
}

fn create_glow() -> Result<Rc<RefCell<Glow>>, String> {
    let (display, is_x11) = overlay_display()?;
    let window = gtk::Window::builder()
        .title("wisprcheap edge glow")
        .decorated(false)
        // Resizable, so it takes the size it's given instead of its (empty) natural size.
        .focusable(false)
        .build();
    window.add_css_class("wisprcheap-glow");
    let css = gtk::CssProvider::new();
    css.load_from_string("window.wisprcheap-glow { background: transparent; box-shadow: none; }");
    gtk::style_context_add_provider_for_display(
        &display,
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    if !is_x11 {
        window.init_layer_shell();
        window.set_namespace(Some("wisprcheap-edge-glow"));
        window.set_layer(Layer::Overlay);
        // Margins count from the output's corner, not from panels and bars.
        window.set_exclusive_zone(-1);
        window.set_keyboard_mode(KeyboardMode::None);
    }
    let area = GlowArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_can_target(false);
    window.set_child(Some(&area));
    let native = realize_overlay(&window, is_x11)?;
    let glow = Rc::new(RefCell::new(Glow {
        window: window.clone(),
        area,
        x11: native,
        scene: GlowScene::new(),
        placement: None,
        active: false,
        frames: 0,
    }));
    window.connect_map(move |window| {
        if let Some(surface) = window.surface() {
            surface.set_input_region(Some(&cairo::Region::create()));
        }
    });
    Ok(glow)
}

fn wake_glow(glow: &Rc<RefCell<Glow>>) {
    let window = {
        let Ok(mut state) = glow.try_borrow_mut() else {
            return;
        };
        let now = Instant::now();
        if state.active || !state.scene.on_screen(now) {
            return;
        }
        state.place();
        state.scene.tick(now, crate::recorder::take_level());
        state.render();
        state.active = true;
        state.frames = 0;
        state.window.clone()
    };
    window.set_visible(true);
    let weak = Rc::downgrade(glow);
    glib::timeout_add_local(FRAME, move || {
        let Some(glow) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        let Ok(mut state) = glow.try_borrow_mut() else {
            return glib::ControlFlow::Continue;
        };
        let now = Instant::now();
        state.scene.tick(now, crate::recorder::take_level());
        state.render();
        if state.scene.on_screen(now) {
            state.frames += 1;
            if state.frames % FOLLOW_FRAMES == 0 {
                state.place();
            }
            return glib::ControlFlow::Continue;
        }
        state.active = false;
        let window = state.window.clone();
        drop(state);
        window.set_visible(false);
        glib::ControlFlow::Break
    });
}

#[derive(Default)]
pub struct Overlay {
    inner: Option<Rc<RefCell<Inner>>>,
    unavailable: bool,
    glow: Option<Rc<RefCell<Glow>>>,
    glow_unavailable: bool,
}

impl Overlay {
    pub fn new() -> Self {
        Self::default()
    }

    /// Show the configured style; the other one gets a disabled status, so it hides (or stays hidden).
    pub fn set_status(&mut self, status: OverlayStatus) {
        let glow = status.enabled && status.style == OverlayStyle::EdgeGlow && self.glow_ready();
        let off = OverlayStatus {
            enabled: false,
            ..status
        };
        self.set_glow_status(if glow { status } else { off });
        self.set_pill_status(if glow { off } else { status });
    }

    /// Create the edge glow on first use. If it can't be shown, the pill is used instead.
    fn glow_ready(&mut self) -> bool {
        if self.glow.is_none() && !self.glow_unavailable {
            match create_glow() {
                Ok(glow) => self.glow = Some(glow),
                Err(why) => {
                    self.glow_unavailable = true;
                    crate::info!("[overlay] {why}");
                }
            }
        }
        self.glow.is_some()
    }

    fn set_glow_status(&mut self, status: OverlayStatus) {
        if let Some(glow) = &self.glow {
            if let Ok(mut state) = glow.try_borrow_mut() {
                state.scene.set_status(status);
            }
            wake_glow(glow);
        }
    }

    fn set_pill_status(&mut self, status: OverlayStatus) {
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

    /// The style that isn't shown has a disabled status, so only the shown one reacts.
    pub fn feedback(&mut self, feedback: Feedback) {
        if let Some(inner) = &self.inner {
            if let Ok(mut state) = inner.try_borrow_mut() {
                state.scene.feedback(feedback, Instant::now());
            }
            wake(inner);
        }
        if let Some(glow) = &self.glow {
            if let Ok(mut state) = glow.try_borrow_mut() {
                state.scene.feedback(feedback, Instant::now());
            }
            wake_glow(glow);
        }
    }

    pub fn destroy(&mut self) {
        if let Some(inner) = self.inner.take()
            && let Ok(state) = inner.try_borrow()
        {
            state.window.destroy();
        }
        if let Some(glow) = self.glow.take()
            && let Ok(state) = glow.try_borrow()
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

    /// Render glow frames offscreen (the real drawing code, through GSK's Cairo fallback) to PNG files in
    /// WISPRCHEAP_GLOW_PNG_DIR, to look at the glow without recording: quiet, then speaking, a moment apart.
    #[test]
    #[ignore = "needs a display; writes PNGs to WISPRCHEAP_GLOW_PNG_DIR"]
    fn render_glow_frames() {
        use super::super::glow::GlowLook;
        gtk::init().unwrap();
        let dir = std::path::PathBuf::from(
            std::env::var_os("WISPRCHEAP_GLOW_PNG_DIR").expect("set WISPRCHEAP_GLOW_PNG_DIR"),
        );
        std::fs::create_dir_all(&dir).unwrap();
        let (w, h) = (1000.0f32, 640.0f32);
        let frames = [
            ("quiet", 0.0, 0.0),
            ("quiet-later", 0.0, 0.2),
            ("speaking", 0.8, 1.0),
            ("speaking-later", 0.8, 1.4),
        ];
        for (name, swell, time) in frames {
            let snapshot = gtk::Snapshot::new();
            snapshot.append_color(
                &gdk::RGBA::new(0.11, 0.11, 0.14, 1.0),
                &graphene::Rect::new(0.0, 0.0, w, h),
            );
            let frame = GlowFrame {
                opacity: 1.0,
                colors: GlowLook::Dictation.palette(),
                time,
                swell,
            };
            draw_glow(&snapshot, w, h, &frame);
            let node = snapshot.to_node().unwrap();
            let mut surface =
                cairo::ImageSurface::create(cairo::Format::ARgb32, w as i32, h as i32).unwrap();
            {
                let cr = cairo::Context::new(&surface).unwrap();
                node.draw(&cr);
            }
            surface.flush();
            let stride = surface.stride() as usize;
            let bytes = glib::Bytes::from(&*surface.data().unwrap());
            let texture = gdk::MemoryTexture::new(
                w as i32,
                h as i32,
                gdk::MemoryFormat::B8g8r8a8Premultiplied,
                &bytes,
                stride,
            );
            texture
                .save_to_png(dir.join(format!("{name}.png")))
                .unwrap();
        }
    }

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
            style: Default::default(),
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
            style: Default::default(),
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
