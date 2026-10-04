#![cfg_attr(windows, windows_subsystem = "windows")]

mod chart;
mod settings_scroll;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use iced::widget::{
    button, canvas, checkbox, column, container, pick_list, row, scrollable, sensor, space, text,
    text_editor, text_input,
};
use iced::{Element, Length, Subscription, Task, Theme, window};
use serde_json::{Value, json};
use wisprcheap::companion::{
    self, Activity, ActivityScope, Change, ConfigDocument, FieldKind, Reply, Request,
    RuntimeStatus, SETTINGS, SettingSpec, StatRow,
};
use wisprcheap::config::ConfigArgs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Overview,
    History,
    Dictionary,
    Settings,
    Translation,
    Pricing,
    Log,
}

impl Page {
    const ALL: [Self; 7] = [
        Self::Overview,
        Self::History,
        Self::Dictionary,
        Self::Settings,
        Self::Translation,
        Self::Pricing,
        Self::Log,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::History => "History",
            Self::Dictionary => "Dictionary",
            Self::Settings => "Settings",
            Self::Translation => "Translation",
            Self::Pricing => "Pricing",
            Self::Log => "Session log",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Period {
    Week,
    Month,
    All,
}
impl Period {
    const ALL: [Self; 3] = [Self::Week, Self::Month, Self::All];
    fn days(self) -> Option<u32> {
        match self {
            Self::Week => Some(7),
            Self::Month => Some(30),
            Self::All => None,
        }
    }
}
impl std::fmt::Display for Period {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Week => "Last 7 days",
            Self::Month => "Last 30 days",
            Self::All => "All time",
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum List {
    Dictionary,
    Pairs,
    Prices,
}
impl List {
    fn path(self) -> &'static str {
        match self {
            Self::Dictionary => "dictionary",
            Self::Pairs => "translation.pairs",
            Self::Prices => "pricing.overrides",
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    Page(Page),
    Section(String),
    SettingsMeasured(&'static str, f32),
    SettingsScrolled(scrollable::Viewport),
    Response(Reply),
    ActivityResponse(String, bool, Period, ActivityScope, Reply),
    Saved(Reply),
    Tick,
    Refresh,
    Field(String, String),
    Bool(String, bool),
    Editor(String, text_editor::Action),
    Reset(String),
    Save,
    Reload,
    Discard,
    KeepEditing,
    Close,
    Search(String),
    Errors(bool),
    Period(Period),
    Scope(ActivityScope),
    Select(usize),
    ListField(List, usize, usize, String),
    Add(List),
    Remove(List, usize),
    Action(Request),
    Start,
    Started(Result<(), String>),
    Copy(String),
    OpenConfig,
    Focus,
    Duplicate,
    Capture,
    Captured(window::Screenshot),
}

struct App {
    args: ConfigArgs,
    page: Page,
    section: String,
    settings_scroll: settings_scroll::SettingsScroll,
    pending_section: Option<String>,
    document: Option<ConfigDocument>,
    fields: BTreeMap<String, String>,
    editors: BTreeMap<String, text_editor::Content>,
    dirty: BTreeSet<String>,
    resets: BTreeSet<String>,
    dictionary: Vec<Vec<String>>,
    pairs: Vec<Vec<String>>,
    prices: Vec<Vec<String>>,
    status: RuntimeStatus,
    polling: bool,
    saving: bool,
    close_after_save: bool,
    starting: bool,
    activity: Option<Activity>,
    query: String,
    errors: bool,
    period: Period,
    scope: ActivityScope,
    selected: Option<usize>,
    devices: Vec<String>,
    log: Vec<String>,
    notice: String,
    confirm: Option<bool>, // true: close, false: reload
    screenshots: Option<PathBuf>,
    capture_index: usize,
    capture_started: bool,
}

fn settings_sections() -> Vec<&'static str> {
    SETTINGS
        .iter()
        .map(|spec| spec.section)
        .fold(Vec::new(), |mut sections, section| {
            if !sections.contains(&section) {
                sections.push(section);
            }
            sections
        })
}

fn main() -> iced::Result {
    let mut settings = window::Settings {
        size: if std::env::args().any(|arg| arg == "--screenshots-small") {
            iced::Size::new(850.0, 600.0)
        } else {
            iced::Size::new(1160.0, 800.0)
        },
        min_size: Some(iced::Size::new(850.0, 600.0)),
        exit_on_close_request: false,
        ..Default::default()
    };
    #[cfg(target_os = "linux")]
    {
        settings.platform_specific.application_id = "wisprcheap".into();
    }
    settings.icon = window::icon::from_rgba(
        wisprcheap::icons::render_rgba(wisprcheap::icons::IconName::Idle, 64),
        64,
        64,
    )
    .ok();
    iced::application(App::boot, App::update, App::view)
        .title("WisprCheap")
        .theme(|_: &App| Theme::Dark)
        .subscription(App::subscription)
        .window(settings)
        .run()
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let mut args = ConfigArgs::default();
        let mut screenshots = None;
        let mut notice = String::new();
        let mut raw = std::env::args().skip(1);
        while let Some(arg) = raw.next() {
            match arg.as_str() {
                "-c" | "--config" => args.config = raw.next(),
                "--screenshots" => screenshots = raw.next().map(PathBuf::from),
                "--screenshots-small" => {}
                other => notice = format!("Unknown argument: {other}"),
            }
        }
        let app = Self {
            args,
            page: Page::Overview,
            section: "General".into(),
            settings_scroll: settings_scroll::SettingsScroll::default(),
            pending_section: None,
            document: None,
            fields: BTreeMap::new(),
            editors: BTreeMap::new(),
            dirty: BTreeSet::new(),
            resets: BTreeSet::new(),
            dictionary: Vec::new(),
            pairs: Vec::new(),
            prices: Vec::new(),
            status: RuntimeStatus::default(),
            polling: true,
            saving: false,
            close_after_save: false,
            starting: false,
            activity: None,
            query: String::new(),
            errors: false,
            period: Period::Month,
            scope: ActivityScope::CurrentDevice,
            selected: None,
            devices: Vec::new(),
            log: Vec::new(),
            notice,
            confirm: None,
            screenshots,
            capture_index: 0,
            capture_started: false,
        };
        let task = Task::batch([
            app.request(Request::Config),
            app.request(Request::Status),
            app.refresh_activity(),
        ]);
        (app, task)
    }

    fn request(&self, request: Request) -> Task<Message> {
        Task::perform(
            companion::request(self.args.clone(), request),
            Message::Response,
        )
    }
    fn refresh_activity(&self) -> Task<Message> {
        let (query, errors, period) = (self.query.clone(), self.errors, self.period);
        let scope = self.scope;
        let request = Request::Activity {
            query: query.clone(),
            errors_only: errors,
            days: period.days(),
            scope,
        };
        Task::perform(
            companion::request(self.args.clone(), request),
            move |reply| Message::ActivityResponse(query.clone(), errors, period, scope, reply),
        )
    }
    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_secs(2)).map(|_| Message::Tick),
            window::close_requests().map(|_| Message::Close),
            Subscription::run(companion_listener),
        ])
    }

    fn load_document(&mut self, doc: ConfigDocument) {
        self.fields = doc
            .fields
            .iter()
            .map(|(path, value)| {
                (
                    path.clone(),
                    display_value(
                        value,
                        SETTINGS
                            .iter()
                            .find(|s| s.path == path)
                            .map(|s| s.kind)
                            .unwrap_or(FieldKind::Text),
                    ),
                )
            })
            .collect();
        self.editors = SETTINGS
            .iter()
            .filter(|s| s.kind == FieldKind::LongText)
            .map(|s| {
                (
                    s.path.into(),
                    text_editor::Content::with_text(
                        self.fields.get(s.path).map(String::as_str).unwrap_or(""),
                    ),
                )
            })
            .collect();
        self.dictionary = doc
            .dictionary
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| {
                vec![
                    v.as_str()
                        .or_else(|| v.get("term").and_then(Value::as_str))
                        .unwrap_or("")
                        .into(),
                    v.get("soundsLike")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default(),
                ]
            })
            .collect();
        self.pairs = doc
            .pairs
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| {
                vec![
                    v.get("from").and_then(Value::as_str).unwrap_or("").into(),
                    v.get("to").and_then(Value::as_str).unwrap_or("").into(),
                ]
            })
            .collect();
        self.prices = doc
            .prices
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| {
                ["model", "perMinute", "inputPerM", "outputPerM"]
                    .iter()
                    .map(|k| {
                        v.get(k)
                            .map(|v| display_value(v, FieldKind::Text))
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .collect();
        self.document = Some(doc);
        self.dirty.clear();
        self.resets.clear();
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        if self.saving
            && matches!(
                message,
                Message::Field(..)
                    | Message::Bool(..)
                    | Message::Editor(..)
                    | Message::Reset(..)
                    | Message::ListField(..)
                    | Message::Add(..)
                    | Message::Remove(..)
            )
        {
            return Task::none();
        }
        match message {
            Message::ActivityResponse(query, errors, period, scope, reply) => {
                if query == self.query
                    && errors == self.errors
                    && period == self.period
                    && scope == self.scope
                {
                    return self.update(Message::Response(reply));
                }
            }
            Message::Page(page) => {
                self.page = page;
                return match page {
                    Page::Overview | Page::History => self.refresh_activity(),
                    Page::Log => self.request(Request::Log),
                    Page::Settings => scroll_settings(self.settings_scroll.offset),
                    _ => Task::none(),
                };
            }
            Message::Section(section) => {
                self.section = section.clone();
                if let Some(offset) = self.settings_scroll.anchor(&section, &settings_sections()) {
                    self.pending_section = None;
                    return scroll_settings(offset);
                }
                self.pending_section = Some(section);
            }
            Message::SettingsMeasured(section, height) => {
                self.settings_scroll.measure(section, height);
                if let Some(section) = &self.pending_section {
                    if let Some(offset) = self.settings_scroll.anchor(section, &settings_sections())
                    {
                        self.pending_section = None;
                        return scroll_settings(offset);
                    }
                } else if let Some(section) = self.settings_scroll.active(&settings_sections()) {
                    self.section = section.into();
                }
            }
            Message::SettingsScrolled(viewport) => {
                self.settings_scroll.scrolled(
                    viewport.absolute_offset().y,
                    viewport.bounds().height,
                    viewport.content_bounds().height,
                );
                if let Some(section) = self.settings_scroll.active(&settings_sections()) {
                    self.section = section.into();
                }
            }
            Message::Tick => {
                if !self.polling {
                    self.polling = true;
                    return self.request(Request::Status);
                }
            }
            Message::Refresh => {
                self.notice.clear();
                return if self.page == Page::Log {
                    self.request(Request::Log)
                } else {
                    self.refresh_activity()
                };
            }
            Message::Response(reply) => match reply {
                Reply::Config(doc) => {
                    if self.dirty.is_empty() {
                        self.load_document(doc);
                    }
                    if self.screenshots.is_some()
                        && !self.capture_started
                        && self.activity.is_some()
                    {
                        self.capture_started = true;
                        return delayed(Message::Capture);
                    }
                }
                Reply::Activity(data) => {
                    self.activity = Some(data);
                    self.selected = None;
                    if self.screenshots.is_some()
                        && !self.capture_started
                        && self.document.is_some()
                    {
                        self.capture_started = true;
                        return delayed(Message::Capture);
                    }
                }
                Reply::Status(status) => {
                    self.status = status;
                    self.polling = false;
                }
                Reply::Devices(devices) => self.devices = devices,
                Reply::Log(lines) => self.log = lines,
                Reply::Error(e) => {
                    self.notice = e;
                    self.polling = false;
                }
                Reply::Ok => {
                    self.polling = true;
                    return Task::batch([self.request(Request::Status), self.refresh_activity()]);
                }
            },
            Message::Saved(reply) => {
                self.saving = false;
                match reply {
                    Reply::Config(doc) => {
                        self.load_document(doc);
                        if self.close_after_save {
                            return iced::exit();
                        }
                        self.confirm = None;
                        self.notice = "Saved. The running app reloads settings automatically after recording finishes.".into();
                    }
                    Reply::Error(e) => self.notice = e,
                    _ => {
                        self.notice =
                            "Unexpected save response; reload settings to check the file.".into()
                    }
                }
                self.close_after_save = false;
            }
            Message::Field(path, value) => {
                self.fields.insert(path.clone(), value);
                self.mark_dirty(path);
            }
            Message::Bool(path, value) => {
                self.fields.insert(path.clone(), value.to_string());
                self.mark_dirty(path);
            }
            Message::Editor(path, action) => {
                let edit = action.is_edit();
                if let Some(editor) = self.editors.get_mut(&path) {
                    editor.perform(action);
                    if edit {
                        self.fields.insert(path.clone(), editor.text());
                    }
                }
                if edit {
                    self.mark_dirty(path);
                }
            }
            Message::Reset(path) => {
                self.dirty.insert(path.clone());
                self.resets.insert(path);
                self.notice = "This override will be removed when you save.".into();
            }
            Message::Save => {
                if self.saving {
                    return Task::none();
                }
                match self.changes() {
                    Ok(changes) => {
                        if let Some(doc) = &self.document {
                            self.saving = true;
                            self.close_after_save = self.confirm == Some(true);
                            self.notice.clear();
                            return Task::perform(
                                companion::request(
                                    self.args.clone(),
                                    Request::Save {
                                        revision: doc.revision.clone(),
                                        changes,
                                    },
                                ),
                                Message::Saved,
                            );
                        }
                    }
                    Err(e) => self.notice = e,
                }
            }
            Message::Reload => {
                if self.dirty.is_empty() {
                    return self.request(Request::Config);
                }
                self.confirm = Some(false);
            }
            Message::Close => {
                if self.saving {
                    self.close_after_save = true;
                    return Task::none();
                }
                if self.dirty.is_empty() {
                    return iced::exit();
                }
                self.confirm = Some(true);
            }
            Message::Discard => {
                if self.saving {
                    self.notice = "Wait for the save to finish.".into();
                } else if self.confirm.take() == Some(true) {
                    return iced::exit();
                } else {
                    self.dirty.clear();
                    self.resets.clear();
                    return self.request(Request::Config);
                }
            }
            Message::KeepEditing => {
                self.confirm = None;
                self.close_after_save = false;
            }
            Message::Search(value) => self.query = value,
            Message::Errors(value) => {
                self.errors = value;
                return self.refresh_activity();
            }
            Message::Period(period) => {
                self.period = period;
                self.activity = None;
                self.selected = None;
                return self.refresh_activity();
            }
            Message::Scope(scope) => {
                if self.scope == scope {
                    return Task::none();
                }
                self.scope = scope;
                self.activity = None;
                self.selected = None;
                self.notice.clear();
                return self.refresh_activity();
            }
            Message::Select(index) => self.selected = Some(index),
            Message::ListField(list, index, field, value) => {
                if let Some(row) = self.list_mut(list).get_mut(index) {
                    row[field] = value;
                }
                self.mark_dirty(list.path().into());
            }
            Message::Add(list) => {
                self.list_mut(list).push(vec![
                    String::new();
                    if matches!(list, List::Prices) { 4 } else { 2 }
                ]);
                self.mark_dirty(list.path().into());
            }
            Message::Remove(list, index) => {
                self.list_mut(list).remove(index);
                self.mark_dirty(list.path().into());
            }
            Message::Action(request) => return self.request(request),
            Message::Start => {
                if !self.dirty.is_empty() {
                    self.notice = "Save or reload your settings before starting dictation.".into();
                } else if !self.starting {
                    self.starting = true;
                    return Task::perform(async_start(self.args.clone()), Message::Started);
                }
            }
            Message::Started(result) => {
                self.starting = false;
                self.notice = match result {
                    Ok(()) => "Dictation started. You can close this window.".into(),
                    Err(e) => e,
                };
                self.polling = true;
                return self.request(Request::Status);
            }
            Message::Copy(value) => return iced::clipboard::write(value),
            Message::OpenConfig => {
                let path = self
                    .document
                    .as_ref()
                    .map(|doc| Ok(PathBuf::from(&doc.path)))
                    .unwrap_or_else(|| {
                        wisprcheap::config::resolve_config_path(&self.args).and_then(|path| {
                            wisprcheap::config::ensure_config_file(path.as_deref())
                        })
                    });
                if let Err(e) = path.and_then(|path| wisprcheap::cli::open_path(&path)) {
                    self.notice = e.to_string();
                }
            }
            Message::Focus => {
                return window::oldest().then(|id| {
                    id.map(|id| window::minimize(id, false).chain(window::gain_focus(id)))
                        .unwrap_or_else(Task::none)
                });
            }
            Message::Duplicate => return iced::exit(),
            Message::Capture => {
                return window::oldest().then(|id| {
                    id.map(|id| window::screenshot(id).map(Message::Captured))
                        .unwrap_or_else(Task::none)
                });
            }
            Message::Captured(shot) => {
                if let Some(dir) = &self.screenshots {
                    if let Err(e) =
                        save_screenshot(dir, self.page, &self.section, self.scope, &shot)
                    {
                        eprintln!("{e}");
                        return iced::exit();
                    }
                    let next_scope = if matches!(self.page, Page::Overview | Page::History) {
                        if self.scope == ActivityScope::CurrentDevice {
                            return self
                                .update(Message::Scope(ActivityScope::AllDevices))
                                .chain(delayed(Message::Capture));
                        }
                        self.update(Message::Scope(ActivityScope::CurrentDevice))
                    } else {
                        Task::none()
                    };
                    if self.page == Page::Settings {
                        let sections = settings_sections();
                        if let Some(next) = sections
                            .iter()
                            .position(|section| *section == self.section)
                            .and_then(|index| sections.get(index + 1))
                        {
                            if let Some(offset) = self.settings_scroll.anchor(next, &sections) {
                                // Scroll without selecting a menu item: captures also verify scroll tracking.
                                return scroll_settings(offset).chain(delayed(Message::Capture));
                            }
                            eprintln!("Settings section layout has not been measured: {next}");
                            return iced::exit();
                        }
                    }
                    self.capture_index += 1;
                    if self.capture_index >= Page::ALL.len() {
                        return iced::exit();
                    }
                    self.page = Page::ALL[self.capture_index];
                    if self.page == Page::History {
                        self.selected = Some(0);
                    }
                    return next_scope.chain(delayed(Message::Capture));
                }
            }
        }
        Task::none()
    }

    fn mark_dirty(&mut self, path: String) {
        self.resets.remove(&path);
        self.dirty.insert(path);
        self.notice.clear();
    }
    fn list_mut(&mut self, list: List) -> &mut Vec<Vec<String>> {
        match list {
            List::Dictionary => &mut self.dictionary,
            List::Pairs => &mut self.pairs,
            List::Prices => &mut self.prices,
        }
    }
    fn changes(&self) -> Result<Vec<Change>, String> {
        self.dirty.iter().map(|path| {
            let value = if self.resets.contains(path) { None }
            else if path == "dictionary" { Some(Value::Array(self.dictionary.iter().map(|r| {
                if r[1].trim().is_empty() { Value::String(r[0].trim().into()) }
                else { json!({"term": r[0].trim(), "soundsLike": r[1].split(',').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>()}) }
            }).collect())) }
            else if path == "translation.pairs" { Some(Value::Array(self.pairs.iter().map(|r| { if r[0].trim().is_empty() { json!({"to": r[1].trim()}) } else { json!({"from": r[0].trim(), "to": r[1].trim()}) } }).collect())) }
            else if path == "pricing.overrides" {
                let prices = self.prices.iter().map(|r| {
                    let mut value = serde_json::Map::new(); value.insert("model".into(), json!(r[0].trim()));
                    for (i, key) in ["perMinute", "inputPerM", "outputPerM"].iter().enumerate() {
                        if !r[i+1].trim().is_empty() { value.insert((*key).into(), parse_number(&r[i+1], key)?); }
                    }
                    Ok(Value::Object(value))
                }).collect::<Result<Vec<_>, String>>()?;
                Some(Value::Array(prices))
            } else {
                let spec = SETTINGS.iter().find(|s| s.path == path).ok_or_else(|| format!("Unknown setting {path}"))?;
                parse_field(spec, &self.fields[path])?
            };
            Ok(Change { path: path.clone(), value })
        }).collect()
    }

    fn view(&self) -> Element<'_, Message> {
        let mut nav = column![
            text("WisprCheap").size(27),
            text("DESKTOP COMPANION").size(11),
            space().height(25)
        ]
        .spacing(8)
        .width(205);
        for page in Page::ALL {
            nav = nav.push(
                button(text(page.label()).size(16))
                    .padding([11, 15])
                    .width(Length::Fill)
                    .style(if self.page == page {
                        button::primary
                    } else {
                        button::text
                    })
                    .on_press(Message::Page(page)),
            );
        }
        nav = nav
            .push(space().height(Length::Fill))
            .push(text("Close this window any time.\nDictation keeps running.").size(12));
        let status = if self.status.running {
            &self.status.label
        } else {
            "Dictation is stopped"
        };
        let mut header = row![
            column![text(self.page.label()).size(30), text(status).size(14)].spacing(5),
            space().width(Length::Fill)
        ]
        .align_y(iced::Alignment::Center)
        .spacing(10);
        header = if self.status.running {
            header.push(
                button(if self.status.paused {
                    "Resume dictation"
                } else {
                    "Pause dictation"
                })
                .on_press(Message::Action(Request::Pause {
                    paused: !self.status.paused,
                })),
            )
        } else {
            header.push(
                button(if self.starting {
                    "Starting…"
                } else {
                    "Start dictation"
                })
                .on_press_maybe((!self.starting).then_some(Message::Start)),
            )
        };
        let body = if self.document.is_none() {
            panel(column![text("Settings could not be loaded yet. You can correct the config file and reload."), row![button("Open config file").on_press(Message::OpenConfig), button("Reload").on_press(Message::Reload)].spacing(10)].spacing(12)).into()
        } else {
            match self.page {
                Page::Overview => self.overview(),
                Page::History => self.history(),
                Page::Dictionary => self.list_view(List::Dictionary),
                Page::Settings => self.settings(),
                Page::Translation => self.translation(),
                Page::Pricing => self.list_view(List::Prices),
                Page::Log => self.logs(),
            }
        };
        let mut content = column![header, space().height(8)].spacing(12);
        if let Some(close) = self.confirm {
            content = content.push(panel(
                row![
                    text(if close {
                        "Unsaved edits. Discard them and close?"
                    } else {
                        "Discard your edits and reload settings?"
                    })
                    .width(Length::Fill),
                    button("Keep editing").on_press(Message::KeepEditing),
                    button(if close {
                        "Discard & close"
                    } else {
                        "Discard & reload"
                    })
                    .on_press(Message::Discard)
                ]
                .spacing(10),
            ));
        }
        if !self.notice.is_empty() {
            content = content.push(panel(text(&self.notice).size(14)));
        }
        if self.document.is_none() {
            content = content.push(text(
                "Loading settings… If loading fails, use your config file to correct the error.",
            ));
        }
        content = content.push(body);
        if !self.dirty.is_empty() {
            content = content.push(panel(
                row![
                    text(format!("{} unsaved change(s)", self.dirty.len())).width(Length::Fill),
                    button("Discard changes").on_press(Message::Reload),
                    button(if self.saving {
                        "Saving…"
                    } else if self.confirm == Some(true) {
                        "Save & close"
                    } else {
                        "Save changes"
                    })
                    .on_press_maybe((!self.saving).then_some(Message::Save))
                ]
                .spacing(10)
                .align_y(iced::Alignment::Center),
            ));
        }
        row![
            container(nav)
                .padding(25)
                .height(Length::Fill)
                .style(container::dark),
            container(content)
                .padding(28)
                .width(Length::Fill)
                .height(Length::Fill)
        ]
        .into()
    }

    fn scope_selector(&self) -> Element<'_, Message> {
        // Match Android's side-by-side FilterChips: filled when selected, outlined otherwise.
        row(
            [ActivityScope::CurrentDevice, ActivityScope::AllDevices].map(|scope| {
                let selected = self.scope == scope;
                button(text(scope.to_string()).size(14))
                    .padding([7, 12])
                    .style(move |theme, status| scope_chip_style(theme, status, selected))
                    .on_press(Message::Scope(scope))
                    .into()
            }),
        )
        .spacing(8)
        .into()
    }

    fn overview(&self) -> Element<'_, Message> {
        let mut body = column![
            row![
                pick_list(Period::ALL, Some(self.period), Message::Period),
                self.scope_selector(),
                space().width(Length::Fill),
                button("Refresh").on_press(Message::Refresh)
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center)
        ]
        .spacing(20);
        if !self.status.running {
            body = body.push(panel(column![text("Your dictation workspace").size(21), text("Configure your providers in Settings, then start dictation. This window is optional while recording.").size(15)].spacing(8)));
        }
        if let Some(data) = &self.activity {
            body = body.push(text(&data.source).size(13));
            let t = &data.totals;
            body = body.push(
                row![
                    metric("WORDS", format_count(t.words)),
                    metric("RECORDINGS", format_count(t.recordings)),
                    metric("AUDIO", format!("{:.1} min", t.audio_minutes)),
                    metric("KNOWN COST", money(t.total_usd))
                ]
                .spacing(12),
            );
            body = body.push(panel(
                column![
                    text("Words per day").size(19),
                    canvas(chart::ActivityChart(&data.daily))
                        .width(Length::Fill)
                        .height(170),
                    text("Last 30 days, limited to the selected period").size(12)
                ]
                .spacing(10),
            ));
            body = body.push(row![panel(column![text("Processing time").size(19), text(format!("Median  {:.2} s     /     95th percentile  {:.2} s", t.median_ms as f64 / 1000.0, t.p95_ms as f64 / 1000.0)), text("STT + LLM time for successful recordings; excludes queue and paste.").size(12)].spacing(10)), panel(column![text("Reliability").size(19), text(format!("{} failed  ·  {} raw-text fallbacks", t.failures, t.fallbacks)), text(format!("{} dictations  ·  {} commands", t.dictations, t.commands))].spacing(10))].spacing(12));
            body = body.push(
                text(format!(
                    "Speech-to-text {}  +  cleanup / translation {}",
                    money(t.stt_usd),
                    money(t.llm_usd)
                ))
                .size(15),
            );
            if t.unknown_prices > 0 {
                body = body.push(text(format!("Cost is partial: {} recording(s) use an unknown model price. Add a price override to track future recordings.", t.unknown_prices)).size(13));
            }
            body = body
                .push(stats_table("Monthly activity", &data.months))
                .push(stats_table("Models", &data.models));
            if t.recordings == 0 {
                body = body.push(text(
                    "Your activity will appear here after your first dictation.",
                ));
            }
        } else {
            body = body.push(text("Loading activity…"));
        }
        if let Some(sync) = &self.status.sync {
            body = body.push(
                row![
                    text(sync),
                    button("Sync now").on_press(Message::Action(Request::SyncNow))
                ]
                .spacing(15),
            );
        }
        body = body.push(
            row![
                button("Copy last dictation").on_press_maybe(
                    self.status
                        .last_text
                        .as_ref()
                        .map(|s| Message::Copy(s.clone()))
                ),
                button("Retry last failed recording").on_press_maybe(
                    (self.status.running && self.status.retry_available)
                        .then_some(Message::Action(Request::RetryFailed))
                )
            ]
            .spacing(10),
        );
        scrollable(body).spacing(16).height(Length::Fill).into()
    }

    fn history(&self) -> Element<'_, Message> {
        let controls = row![
            text_input("Search text, model or error", &self.query)
                .on_input(Message::Search)
                .on_submit(Message::Refresh),
            button("Search").on_press(Message::Refresh),
            pick_list(Period::ALL, Some(self.period), Message::Period)
        ]
        .spacing(10);
        let mut entries = column![
            controls,
            row![
                self.scope_selector(),
                checkbox(self.errors)
                    .label("Errors and cleanup fallbacks only")
                    .on_toggle(Message::Errors)
            ]
            .spacing(16)
            .align_y(iced::Alignment::Center)
            .wrap()
        ]
        .spacing(12);
        if let Some(data) = &self.activity {
            entries = entries.push(text(&data.source).size(13));
            entries = entries.push(
                text(format!(
                    "{} matching recordings · showing the latest {}",
                    data.matches,
                    data.entries.len()
                ))
                .size(13),
            );
            if data.entries.is_empty() {
                entries = entries.push(panel(text("No recordings match these filters.")));
            }
            for (i, entry) in data.entries.iter().enumerate() {
                let kind = if entry.error.is_some() {
                    "Failed"
                } else if entry.polish.as_ref().is_some_and(|p| p.error.is_some()) {
                    "Raw fallback"
                } else if entry.mode.as_deref() == Some("command") {
                    "Command"
                } else {
                    "Dictation"
                };
                let timestamp = chrono::DateTime::parse_from_rfc3339(&entry.ts)
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%b %d · %H:%M")
                            .to_string()
                    })
                    .unwrap_or_else(|_| entry.ts.clone());
                entries = entries.push(
                    button(
                        column![
                            row![
                                text(timestamp).size(13),
                                space().width(Length::Fill),
                                text(format!("{kind} · {} words", entry.words)).size(13)
                            ],
                            text(snippet(entry.error.as_deref().unwrap_or(&entry.text), 100))
                                .size(15)
                        ]
                        .spacing(6),
                    )
                    .width(Length::Fill)
                    .padding(13)
                    .style(button::secondary)
                    .on_press(Message::Select(i)),
                );
                if self.selected == Some(i) {
                    let mut details = column![
                        text(format!(
                            "{} · {:.1}s audio · STT {}ms · {}",
                            entry.transcription.model,
                            entry.duration_sec,
                            entry.transcription.ms,
                            entry
                                .cost_usd
                                .total
                                .map(money)
                                .unwrap_or_else(|| "cost unknown / partial".into())
                        ))
                        .size(13),
                        text("RESULT").size(11),
                        text(&entry.text),
                        button("Copy result").on_press(Message::Copy(entry.text.clone())),
                        text("RAW TRANSCRIPT").size(11),
                        text(&entry.raw),
                        button("Copy raw transcript").on_press(Message::Copy(entry.raw.clone()))
                    ]
                    .spacing(10);
                    if let Some(error) = &entry.error {
                        details = details.push(text(error));
                    }
                    if let Some(pair) = &entry.translation {
                        details = details.push(text(format!("Translation: {pair}")).size(13));
                    }
                    if let Some(Some(selection)) = &entry.selection {
                        details = details
                            .push(text("ORIGINAL SELECTION").size(11))
                            .push(text(selection));
                    }
                    if let Some(polish) = &entry.polish {
                        details = details.push(
                            text(format!(
                                "{} · {}ms · {} input / {} output tokens",
                                polish.model, polish.ms, polish.input_tokens, polish.output_tokens
                            ))
                            .size(13),
                        );
                        if let Some(e) = &polish.error {
                            details = details.push(text(e));
                        }
                    }
                    if let Some(audio) = &entry.audio_file {
                        details =
                            details.push(text(format!("Saved failed audio: {audio}")).size(13));
                    }
                    entries = entries.push(panel(details));
                }
            }
        }
        scrollable(entries).spacing(16).height(Length::Fill).into()
    }

    fn settings(&self) -> Element<'_, Message> {
        let mut sections = column![text("SECTIONS").size(11), space().height(4)].spacing(6);
        for section in settings_sections() {
            sections = sections.push(
                button(text(section).size(15))
                    .padding([9, 10])
                    .width(Length::Fill)
                    .style(if self.section == section {
                        button::primary
                    } else {
                        button::text
                    })
                    .on_press(Message::Section(section.into())),
            );
        }
        let mut intro = column![
            row![
                button("Reload").on_press(Message::Reload),
                button("Open config file").on_press(Message::OpenConfig)
            ]
            .spacing(10)
            .wrap()
        ]
        .spacing(18);
        if let Some(doc) = &self.document {
            intro = intro.push(text(&doc.path).size(12));
            for warning in &doc.warnings {
                intro = intro.push(text(warning).size(13));
            }
        }
        let mut body = column![
            sensor(intro)
                .on_resize(|size| Message::SettingsMeasured(settings_scroll::INTRO, size.height))
        ]
        .spacing(settings_scroll::GAP);
        for section in settings_sections() {
            let mut group = column![text(section).size(24)].spacing(18);
            for spec in SETTINGS.iter().filter(|spec| spec.section == section) {
                group = group.push(self.setting(spec));
            }
            if section == "Recording" {
                group = group.push(
                    button("List available microphones")
                        .on_press(Message::Action(Request::Devices)),
                );
                for device in &self.devices {
                    group = group.push(
                        button(text(device))
                            .style(button::secondary)
                            .on_press(Message::Field("recording.device".into(), device.clone())),
                    );
                }
            }
            if section == "Output & privacy" {
                group = group.push(text("Sync account setup remains available through `wisprcheap sync`. Advanced options can also be edited in the config file.").size(12));
            }
            body = body.push(
                sensor(group)
                    .key(section)
                    .on_resize(move |size| Message::SettingsMeasured(section, size.height)),
            );
        }
        row![
            container(scrollable(sections).spacing(8).height(Length::Fill))
                .padding(12)
                .width(168)
                .height(Length::Fill)
                .style(container::bordered_box),
            scrollable(body)
                .id("settings-fields")
                .on_scroll(Message::SettingsScrolled)
                .spacing(16)
                .width(Length::Fill)
                .height(Length::Fill)
        ]
        .spacing(22)
        .height(Length::Fill)
        .into()
    }

    fn setting(&self, spec: &'static SettingSpec) -> Element<'_, Message> {
        let path = spec.path;
        let value = self.fields.get(path).map(String::as_str).unwrap_or("");
        let mut field = column![
            row![
                text(spec.label).size(16).width(Length::Fill),
                button(if self.resets.contains(path) {
                    "Reset pending"
                } else {
                    "Reset"
                })
                .style(button::text)
                .on_press(Message::Reset(path.into()))
            ],
            text(spec.hint).size(12)
        ]
        .spacing(8);
        let input: Element<'_, Message> = match spec.kind {
            FieldKind::Bool if matches!(value, "true" | "false") => checkbox(value == "true")
                .label("Enabled")
                .on_toggle(move |v| Message::Bool(path.into(), v))
                .into(),
            FieldKind::LongText => {
                if let Some(content) = self.editors.get(path) {
                    text_editor(content)
                        .height(130)
                        .on_action(move |action| Message::Editor(path.into(), action))
                        .into()
                } else {
                    text("Loading…").into()
                }
            }
            _ => text_input("Use default / inherited value", value)
                .secure(spec.kind == FieldKind::Secret)
                .on_input(move |v| Message::Field(path.into(), v))
                .padding(10)
                .into(),
        };
        field = field.push(input);
        panel(field).into()
    }

    fn translation(&self) -> Element<'_, Message> {
        let mut body = column![text("Select a configured pair to translate dictations before pasting. Changes to pairs use the same Save button as other settings.").size(14)].spacing(15);
        let mut choices = row![
            button("Translation off").on_press_maybe(
                self.status
                    .running
                    .then_some(Message::Action(Request::Translate { index: -1 }))
            )
        ]
        .spacing(8);
        for (i, label) in self.status.translations.iter().enumerate() {
            choices = choices.push(
                button(text(label))
                    .on_press(Message::Action(Request::Translate { index: i as i32 })),
            );
        }
        body = body
            .push(choices.wrap())
            .push(
                text(format!(
                    "Selected pair: {}",
                    self.status.translation.as_deref().unwrap_or("off")
                ))
                .size(13),
            )
            .push(self.list_rows(List::Pairs));
        scrollable(body).spacing(16).height(Length::Fill).into()
    }

    fn list_view(&self, list: List) -> Element<'_, Message> {
        let hint = match list {
            List::Dictionary => {
                "Names and specialist terms help speech recognition and cleanup. Separate sound-alike spellings with commas."
            }
            List::Pairs => {
                "Leave the source blank for automatic language detection. Use language names or codes."
            }
            List::Prices => {
                "Price overrides are in USD. Use per-minute pricing for speech models, or input/output prices per million tokens for cleanup models. Changes affect future recordings."
            }
        };
        scrollable(column![text(hint).size(14), self.list_rows(list)].spacing(18))
            .spacing(16)
            .height(Length::Fill)
            .into()
    }

    fn list_rows(&self, list: List) -> Element<'_, Message> {
        let (rows, headings) = match list {
            List::Dictionary => (&self.dictionary, vec!["Term", "Sounds like (optional)"]),
            List::Pairs => (&self.pairs, vec!["From (auto if blank)", "To"]),
            List::Prices => (
                &self.prices,
                vec!["Model", "USD / min", "Input / 1M", "Output / 1M"],
            ),
        };
        let mut body = column![].spacing(10);
        let mut header = row![].spacing(10);
        for heading in &headings {
            header = header.push(text(*heading).size(13).width(Length::Fill));
        }
        header = header.push(space().width(75));
        body = body.push(header);
        for (i, values) in rows.iter().enumerate() {
            let mut line = row![].spacing(10);
            for (j, value) in values.iter().enumerate() {
                line = line.push(
                    text_input(headings[j], value)
                        .padding(10)
                        .on_input(move |v| Message::ListField(list, i, j, v)),
                );
            }
            body = body.push(
                line.push(
                    button("Remove")
                        .style(button::text)
                        .on_press(Message::Remove(list, i)),
                ),
            );
        }
        if rows.is_empty() {
            body = body.push(panel(text("No entries yet. Add one below.")));
        }
        body.push(button("+ Add entry").on_press(Message::Add(list)))
            .into()
    }

    fn logs(&self) -> Element<'_, Message> {
        let lines = self.log.join("\n");
        column![
            row![
                text("Latest session · up to 500 lines").width(Length::Fill),
                button("Refresh").on_press(Message::Refresh),
                button("Copy log").on_press(Message::Copy(lines.clone()))
            ]
            .spacing(10),
            scrollable(text(lines).font(iced::Font::MONOSPACE).size(13))
                .spacing(16)
                .height(Length::Fill)
        ]
        .spacing(15)
        .into()
    }
}

fn companion_listener() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(2, async |mut output| {
        use iced::futures::SinkExt;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let instance = wisprcheap::instance::acquire_instance_at(
            wisprcheap::instance::ui_socket_name(),
            tx,
            Duration::ZERO,
        )
        .await;
        let _instance = match instance {
            Ok(instance) => instance,
            Err(wisprcheap::instance::AcquireError::AlreadyRunning) => {
                let _ = wisprcheap::instance::send_command_at(
                    &wisprcheap::instance::ui_socket_name(),
                    "focus",
                    Duration::from_secs(2),
                )
                .await;
                let _ = output.send(Message::Duplicate).await;
                return;
            }
            Err(e) => {
                let _ = output
                    .send(Message::Response(Reply::Error(e.to_string())))
                    .await;
                return;
            }
        };
        while let Some((command, reply)) = rx.recv().await {
            match command.as_str() {
                "focus" => {
                    let _ = reply.send("ok".into());
                    let _ = output.send(Message::Focus).await;
                }
                "close" => {
                    let _ = reply.send("ok".into());
                    let _ = output.send(Message::Close).await;
                }
                _ => {
                    let _ = reply.send("unknown-command".into());
                }
            }
        }
    })
}

async fn async_start(args: ConfigArgs) -> Result<(), String> {
    wisprcheap::cli::start_from_companion(args)
        .await
        .map_err(|e| e.to_string())
}
fn delayed(message: Message) -> Task<Message> {
    Task::perform(
        async move {
            tokio::time::sleep(Duration::from_millis(700)).await;
            message
        },
        |m| m,
    )
}

fn scroll_settings(offset: f32) -> Task<Message> {
    iced::widget::operation::scroll_to(
        "settings-fields",
        scrollable::AbsoluteOffset { x: 0.0, y: offset },
    )
}
fn scope_chip_style(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    let palette = theme.extended_palette();
    let mut style = button::Style {
        background: selected.then_some(palette.primary.weak.color.into()),
        text_color: if selected {
            palette.primary.weak.text
        } else {
            palette.background.base.text
        },
        border: iced::Border {
            color: if selected {
                iced::Color::TRANSPARENT
            } else {
                palette.background.strong.color
            },
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    };
    if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        style.background = Some(if selected {
            palette.primary.base.color.into()
        } else {
            palette.background.weak.color.into()
        });
        if selected {
            style.text_color = palette.primary.base.text;
        }
    }
    style
}
fn panel<'a>(content: impl Into<Element<'a, Message>>) -> iced::widget::Container<'a, Message> {
    container(content)
        .padding(16)
        .width(Length::Fill)
        .style(container::rounded_box)
}
fn metric(label: &str, value: String) -> Element<'_, Message> {
    panel(column![text(label).size(11), text(value).size(27)].spacing(8)).into()
}
fn money(value: f64) -> String {
    format!("${value:.4}")
}
fn format_count(value: usize) -> String {
    let raw = value.to_string();
    raw.chars()
        .enumerate()
        .fold(String::new(), |mut out, (i, c)| {
            if i > 0 && (raw.len() - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(c);
            out
        })
}
fn snippet(value: &str, length: usize) -> String {
    let mut s: String = value
        .replace(['\n', '\r'], " ")
        .chars()
        .take(length)
        .collect();
    if value.chars().count() > length {
        s.push('…');
    }
    s
}
fn stats_table<'a>(title: &'a str, rows: &'a [StatRow]) -> Element<'a, Message> {
    let mut table = column![
        text(title).size(19),
        row![
            text("Period / model").width(Length::Fill),
            text("Recordings").width(100),
            text("Words").width(100),
            text("Known cost").width(110)
        ]
        .spacing(8)
    ]
    .spacing(12);
    for item in rows {
        table = table.push(
            row![
                text(&item.label).width(Length::Fill),
                text(format_count(item.count)).width(100),
                text(format_count(item.words)).width(100),
                text(money(item.usd)).width(110)
            ]
            .spacing(8),
        );
    }
    if rows.is_empty() {
        table = table.push(text("No activity yet").size(13));
    }
    panel(table).into()
}

fn display_value(value: &Value, kind: FieldKind) -> String {
    match value {
        Value::Null => {
            if kind == FieldKind::Secret {
                String::new()
            } else {
                "null".into()
            }
        }
        Value::String(s) => s.clone(),
        Value::Array(a) if kind == FieldKind::Keys => a
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" + "),
        other => other.to_string(),
    }
}
fn parse_number(value: &str, label: &str) -> Result<Value, String> {
    serde_json::from_str::<Value>(value.trim())
        .ok()
        .filter(Value::is_number)
        .ok_or_else(|| format!("{label}: enter a number"))
}
fn parse_field(spec: &SettingSpec, value: &str) -> Result<Option<Value>, String> {
    if value.trim().is_empty() && spec.kind != FieldKind::Keys && spec.kind != FieldKind::LongText {
        return Ok(None);
    }
    if value.trim().starts_with("${") && value.trim().ends_with('}') && spec.kind != FieldKind::Keys
    {
        return Ok(Some(Value::String(value.into())));
    }
    Ok(Some(match spec.kind {
        FieldKind::Number => {
            if value.trim() == "null" {
                Value::Null
            } else {
                parse_number(value, spec.label)?
            }
        }
        FieldKind::Bool => {
            if value == "true" {
                Value::Bool(true)
            } else if value == "false" {
                Value::Bool(false)
            } else if value.starts_with("${") {
                Value::String(value.into())
            } else {
                return Err(format!("{}: use true or false", spec.label));
            }
        }
        FieldKind::Keys => Value::Array(
            value
                .split('+')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.into()))
                .collect(),
        ),
        _ if spec.path.ends_with("reasoningEffort") && value.trim() == "null" => Value::Null,
        _ if spec.path == "recording.device" && value.parse::<i64>().is_ok() => {
            json!(value.parse::<i64>().unwrap())
        }
        _ => Value::String(value.into()),
    }))
}

fn save_screenshot(
    dir: &std::path::Path,
    page: Page,
    section: &str,
    scope: ActivityScope,
    screenshot: &window::Screenshot,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dir)?;
    let name =
        if matches!(page, Page::Overview | Page::History) && scope == ActivityScope::AllDevices {
            format!("{}-all-devices", page.label())
        } else if page == Page::Settings && section != "General" {
            format!("settings-{section}")
        } else {
            page.label().into()
        };
    let file = std::fs::File::create(dir.join(format!(
        "{}.png",
        name.to_lowercase().replace(" & ", "-").replace(' ', "-")
    )))?;
    let mut encoder = png::Encoder::new(file, screenshot.size.width, screenshot.size.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&screenshot.rgba)?;
    Ok(())
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    fn app() -> App {
        let (mut app, _) = App::boot();
        app.load_document(ConfigDocument {
            path: "test-config.yaml".into(),
            revision: "test".into(),
            fields: BTreeMap::new(),
            dictionary: json!([]),
            pairs: json!([]),
            prices: json!([]),
            warnings: Vec::new(),
        });
        app
    }

    #[test]
    fn closing_clean_companion_exits_and_dirty_companion_keeps_edits_until_confirmed() {
        let mut app = app();
        assert_eq!(app.update(Message::Close).units(), 1);
        let _ = app.update(Message::Bool("sounds.enabled".into(), false));
        assert_eq!(app.update(Message::Close).units(), 0);
        assert_eq!(app.confirm, Some(true));
        assert_eq!(app.fields["sounds.enabled"], "false");
        let _ = app.update(Message::KeepEditing);
        assert_eq!(app.confirm, None);
        assert!(app.dirty.contains("sounds.enabled"));
        let _ = app.update(Message::Close);
        assert_eq!(app.update(Message::Discard).units(), 1);
    }

    #[test]
    fn saving_from_close_confirmation_closes_only_after_success() {
        let mut app = app();
        let _ = app.update(Message::Bool("sounds.enabled".into(), false));
        let _ = app.update(Message::Close);
        let _ = app.update(Message::Save);
        assert!(app.saving && app.close_after_save);
        let doc = app.document.clone().unwrap();
        assert_eq!(app.update(Message::Saved(Reply::Config(doc))).units(), 1);
        assert!(!app.saving);
        assert!(app.dirty.is_empty());
    }

    #[test]
    fn closing_during_save_waits_and_failed_save_preserves_edits() {
        let mut app = app();
        let _ = app.update(Message::Bool("sounds.enabled".into(), false));
        let _ = app.update(Message::Save);
        assert_eq!(app.update(Message::Close).units(), 0);
        assert!(app.close_after_save);
        assert_eq!(
            app.update(Message::Saved(Reply::Error("Save failed".into())))
                .units(),
            0
        );
        assert!(!app.close_after_save && !app.saving);
        assert!(app.dirty.contains("sounds.enabled"));
        assert_eq!(app.notice, "Save failed");
    }
}
