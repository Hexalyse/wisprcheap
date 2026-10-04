//! Linux log window: a GTK window with a read-only, monospace TextView (dark theme).
//! Esc or the close button hide it. Runs on tao's GTK main loop.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};

const CSS: &[u8] = b"textview, textview text { background-color: #18181b; color: #e4e4e7; font-family: Consolas, 'DejaVu Sans Mono', monospace; font-size: 10pt; }";

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
    icon_dir: PathBuf,
    on_hidden: Rc<dyn Fn()>,
}

impl LogWindow {
    pub fn new(icon_dir: PathBuf, on_hidden: Box<dyn Fn()>) -> Self {
        Self {
            widgets: None,
            lines: VecDeque::new(),
            title: "wisprcheap log".into(),
            icon_dir,
            on_hidden: Rc::from(on_hidden),
        }
    }

    fn create(&mut self) {
        if self.widgets.is_some() {
            return;
        }
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_title(&self.title);
        let (mut w, mut h) = (900, 560);
        if let Some(display) = gdk::Display::default()
            && let Some(monitor) = display.primary_monitor().or_else(|| display.monitor(0))
        {
            let area = monitor.workarea();
            w = area.width() * 55 / 100;
            h = area.height() * 55 / 100;
        }
        window.set_default_size(w, h);
        window.set_position(gtk::WindowPosition::Center);
        if let Ok(pixbuf) = gtk::gdk_pixbuf::Pixbuf::from_file(self.icon_dir.join("idle.png")) {
            window.set_icon(Some(&pixbuf));
        }

        let scrolled = gtk::ScrolledWindow::new(None::<&gtk::Adjustment>, None::<&gtk::Adjustment>);
        scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        let view = gtk::TextView::new();
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_monospace(true);
        view.set_left_margin(6);
        view.set_right_margin(6);
        let provider = gtk::CssProvider::new();
        if provider.load_from_data(CSS).is_ok() {
            view.style_context()
                .add_provider(&provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        scrolled.add(&view);
        window.add(&scrolled);

        let on_hidden = self.on_hidden.clone();
        window.connect_delete_event(move |w, _| {
            w.hide();
            on_hidden();
            glib::Propagation::Stop
        });
        let on_hidden = self.on_hidden.clone();
        window.connect_key_press_event(move |w, event| {
            if event.keyval() == gdk::keys::constants::Escape {
                w.hide();
                on_hidden();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });

        let buffer = view.buffer().expect("text buffer");
        let text = self
            .lines
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        buffer.set_text(&text);
        let end_mark = buffer
            .create_mark(Some("end"), &buffer.end_iter(), false)
            .expect("text mark");
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
            let mut cut = w.buffer.iter_at_line(excess);
            w.buffer.delete(&mut start, &mut cut);
        }
        if w.window.is_visible() {
            self.scroll_to_end();
        }
    }

    pub fn set_title(&mut self, title: &str) {
        self.title = title.to_string();
        if let Some(w) = &self.widgets {
            w.window.set_title(title);
        }
    }

    pub fn show(&mut self) {
        self.create();
        if let Some(w) = &self.widgets {
            w.window.show_all();
            w.window.present();
        }
        self.scroll_to_end();
    }

    pub fn hide(&mut self) {
        if let Some(w) = &self.widgets {
            w.window.hide();
        }
    }

    pub fn destroy(&mut self) {
        self.hide();
        self.widgets = None;
    }
}
