//! Linux log window: GTK4 with a read-only, monospace TextView (dark theme).
//! Esc or the close button hide it. Runs on GLib's main context.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

const CSS: &str = "textview.wisprcheap-log, textview.wisprcheap-log text { background-color: #18181b; color: #e4e4e7; font-family: Consolas, 'DejaVu Sans Mono', monospace; font-size: 10pt; }";

struct Widgets {
    window: gtk::Window,
    view: gtk::TextView,
    buffer: gtk::TextBuffer,
    end_mark: gtk::TextMark,
}

pub struct LogWindow {
    widgets: Option<Widgets>,
    lines: VecDeque<String>,
    title: String,
    on_hidden: Rc<dyn Fn()>,
}

impl LogWindow {
    pub fn new(_icon_dir: PathBuf, on_hidden: Box<dyn Fn()>) -> Self {
        Self {
            widgets: None,
            lines: VecDeque::new(),
            title: "wisprcheap log".into(),
            on_hidden: Rc::from(on_hidden),
        }
    }

    fn create(&mut self) {
        if self.widgets.is_some() {
            return;
        }
        let window = gtk::Window::new();
        window.set_title(Some(&self.title));
        let (mut w, mut h) = (900, 560);
        if let Some(display) = gdk::Display::default()
            && let Some(monitor) = display.monitors().item(0).and_downcast::<gdk::Monitor>()
        {
            let area = monitor.geometry();
            w = area.width() * 55 / 100;
            h = area.height() * 55 / 100;
        }
        window.set_default_size(w, h);
        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let view = gtk::TextView::new();
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_monospace(true);
        view.set_left_margin(6);
        view.set_right_margin(6);
        view.add_css_class("wisprcheap-log");
        let provider = gtk::CssProvider::new();
        provider.load_from_data(CSS);
        gtk::style_context_add_provider_for_display(
            &gtk::prelude::WidgetExt::display(&window),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        scrolled.set_child(Some(&view));
        window.set_child(Some(&scrolled));

        let on_hidden = self.on_hidden.clone();
        window.connect_close_request(move |w| {
            w.hide();
            on_hidden();
            glib::Propagation::Stop
        });
        let on_hidden = self.on_hidden.clone();
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak_window = window.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(window) = weak_window.upgrade() {
                    window.hide();
                }
                on_hidden();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(keys);

        let buffer = view.buffer();
        let text = self
            .lines
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        buffer.set_text(&text);
        let end_mark = buffer.create_mark(Some("end"), &buffer.end_iter(), false);
        self.widgets = Some(Widgets {
            window,
            view,
            buffer,
            end_mark,
        });
    }

    fn scroll_to_end(&self) {
        if let Some(w) = &self.widgets {
            w.buffer.move_mark(&w.end_mark, &w.buffer.end_iter());
            w.view.scroll_to_mark(&w.end_mark, 0.0, false, 0.0, 1.0);
        }
    }

    pub fn append(&mut self, line: &str) {
        self.lines.push_back(line.to_string());
        while self.lines.len() > super::LOG_LINES {
            self.lines.pop_front();
        }
        let Some(w) = &self.widgets else {
            return;
        };
        let mut end = w.buffer.end_iter();
        let piece = if w.buffer.char_count() == 0 {
            line.to_string()
        } else {
            format!("\n{line}")
        };
        w.buffer.insert(&mut end, &piece);
        let excess = w.buffer.line_count() - super::LOG_LINES as i32;
        if excess > 0 {
            let mut start = w.buffer.start_iter();
            if let Some(mut cut) = w.buffer.iter_at_line(excess) {
                w.buffer.delete(&mut start, &mut cut);
            }
        }
        if w.window.is_visible() {
            self.scroll_to_end();
        }
    }

    pub fn set_title(&mut self, title: &str) {
        self.title = title.to_string();
        if let Some(w) = &self.widgets {
            w.window.set_title(Some(title));
        }
    }

    pub fn show(&mut self) {
        self.create();
        if let Some(w) = &self.widgets {
            w.window.present();
        }
        self.scroll_to_end();
    }

    pub fn destroy(&mut self) {
        if let Some(widgets) = self.widgets.take() {
            widgets.window.destroy();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    fn settle() {
        let context = glib::MainContext::default();
        let end = Instant::now() + Duration::from_millis(250);
        while Instant::now() < end {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "requires an isolated X11 desktop; run tests/x11_overlay.py"]
    fn gtk4_log_window_backend() {
        gtk::init().unwrap();
        let hidden = Rc::new(Cell::new(0));
        let counter = hidden.clone();
        let dir = tempfile::tempdir().unwrap();
        let mut log = LogWindow::new(
            dir.path().to_path_buf(),
            Box::new(move || counter.set(counter.get() + 1)),
        );
        log.append("buffered before creation");
        log.show();
        settle();
        assert!(log.widgets.as_ref().unwrap().window.is_visible());
        assert!(!log.widgets.as_ref().unwrap().view.is_editable());
        for i in 0..super::super::LOG_LINES + 5 {
            log.append(&format!("line {i}"));
        }
        let widgets = log.widgets.as_ref().unwrap();
        assert_eq!(widgets.buffer.line_count(), super::super::LOG_LINES as i32);
        let text = widgets.buffer.text(
            &widgets.buffer.start_iter(),
            &widgets.buffer.end_iter(),
            false,
        );
        assert!(!text.contains("buffered before creation"));
        assert!(text.ends_with("line 3004"));
        widgets.window.close();
        settle();
        assert!(!log.widgets.as_ref().unwrap().window.is_visible());
        assert_eq!(hidden.get(), 1);
        log.show();
        settle();
        assert!(log.widgets.as_ref().unwrap().window.is_active());
        assert!(
            std::process::Command::new("xdotool")
                .args(["key", "Escape"])
                .status()
                .unwrap()
                .success()
        );
        settle();
        assert!(!log.widgets.as_ref().unwrap().window.is_visible());
        assert_eq!(hidden.get(), 2);
        log.destroy();
        assert!(log.widgets.is_none());
    }
}
