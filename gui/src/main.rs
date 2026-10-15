//! armor-gui: the window of armor (ADR-0035). It shows the installed x86
//! applications as cards, and installs, opens and removes them by running
//! the `armor` command (`armor list --porcelain`, `armor install`,
//! `armor run`, `armor remove`, `armor engine fetch`). It holds no logic of
//! its own about applications: it reads `armor`'s output, turning the lines
//! it knows into a progress bar (`progress_of`), with the full output under
//! "Details".
//!
//! Nothing runs on the window's thread that could block it: the listing
//! and every command run on other threads. The window refreshes itself
//! when the applications directory changes (an installation started with a
//! double click outside it), accepts installers dropped on it, and its grid
//! of cards reflows with its width.

use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

const APP_ID: &str = "io.github.FrankNat996.armor";

/// The window's style, on top of the GTK theme: colours derived from the
/// theme's text colour (`currentColor`), so that light and dark themes
/// both work, and two fixed accents for the Windows and Linux badges.
const CSS: &str = "
.app-card {
    padding: 18px 14px 14px 14px;
    border-radius: 16px;
    background-color: alpha(currentColor, 0.045);
    border: 1px solid alpha(currentColor, 0.09);
}
flowboxchild { padding: 0; border-radius: 16px; background: none; }
flowboxchild:hover .app-card { background-color: alpha(currentColor, 0.08); }
flowboxchild:focus-visible .app-card { border-color: #3584e4; }
.app-name { font-weight: 700; font-size: 1.05em; }
.badge {
    font-size: 0.75em;
    font-weight: 700;
    padding: 2px 9px;
    border-radius: 999px;
}
.badge.windows { color: #3584e4; background-color: alpha(#3584e4, 0.16); }
.badge.linux { color: #e66100; background-color: alpha(#ff7800, 0.16); }
.badge.steam { color: #2a7bbf; background-color: alpha(#1b2838, 0.18); }
.pill { border-radius: 999px; padding: 4px 18px; }
.circular { border-radius: 999px; min-width: 30px; min-height: 30px; padding: 0; }
.banner {
    padding: 10px 18px;
    background-color: alpha(#3584e4, 0.12);
    border-bottom: 1px solid alpha(#3584e4, 0.25);
}
.activity {
    padding: 12px 18px;
    border-top: 1px solid alpha(currentColor, 0.12);
    background-color: alpha(currentColor, 0.035);
}
.activity-title { font-weight: 700; }
.details { font-family: monospace; font-size: 0.85em; }
.toast {
    padding: 10px 20px;
    border-radius: 999px;
    background-color: rgba(32, 32, 32, 0.92);
    color: white;
}
.empty-title { font-size: 1.6em; font-weight: 800; }
.dim { opacity: 0.65; }
.dropping { background-color: alpha(#3584e4, 0.08); }
progressbar trough, progressbar progress { min-height: 6px; border-radius: 3px; }
";

/// One record of `armor list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Record {
    /// The Windows engine's `wine` and whether it is there.
    Engine {
        present: bool,
    },
    App {
        name: String,
        kind: String,
    },
    Entry {
        app: String,
        name: String,
        /// A path or a theme icon name; empty if none.
        icon: String,
    },
}

/// Parses `armor list --porcelain`; unknown or short lines are skipped, and
/// fields beyond the known ones ignored, so that a newer `armor` still
/// works; an `entry` line without the icon field (an older `armor`) has no
/// icon.
fn parse_porcelain(text: &str) -> Vec<Record> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["engine", _path, present, ..] => Some(Record::Engine {
                    present: *present == "yes",
                }),
                ["app", name, kind, ..] => Some(Record::App {
                    name: name.to_string(),
                    kind: kind.to_string(),
                }),
                ["entry", app, name, rest @ ..] => Some(Record::Entry {
                    app: app.to_string(),
                    name: name.to_string(),
                    icon: rest.first().unwrap_or(&"").to_string(),
                }),
                _ => None,
            }
        })
        .collect()
}

/// An installed application, from the records.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AppInfo {
    name: String,
    kind: String,
    /// Its menu entries: name and icon.
    entries: Vec<(String, String)>,
}

fn apps_of(records: &[Record]) -> Vec<AppInfo> {
    let mut apps: Vec<AppInfo> = records
        .iter()
        .filter_map(|r| match r {
            Record::App { name, kind } => Some(AppInfo {
                name: name.clone(),
                kind: kind.clone(),
                entries: Vec::new(),
            }),
            _ => None,
        })
        .collect();
    for record in records {
        if let Record::Entry { app, name, icon } = record
            && let Some(info) = apps.iter_mut().find(|a| a.name == *app)
        {
            info.entries.push((name.clone(), icon.clone()));
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// What a line of `armor`'s output says about the progress of a task.
#[derive(Debug, Clone, PartialEq)]
enum Progress {
    /// A new step, in words for the user.
    Step(String),
    /// A step with its share done (0 to 1).
    Fraction(f64, String),
    /// The share done of the current download (curl's progress bar).
    Percent(f64),
    /// The task's own last word on success.
    Done(String),
    /// Nothing for the progress bar (it stays in "Details").
    Noise,
}

/// Reads one line of `armor`'s output (the messages of `apps.rs`,
/// `debs.rs` and `engines.rs`, and curl's progress bar).
fn progress_of(line: &str) -> Progress {
    let line = line.trim();
    // curl --progress-bar: `#####      23.4%`.
    if !line.is_empty() && line.chars().all(|c| "#=-O%. 0123456789".contains(c)) {
        return line
            .split_whitespace()
            .last()
            .and_then(|t| t.strip_suffix('%'))
            .and_then(|t| t.parse::<f64>().ok())
            .map_or(Progress::Noise, |p| Progress::Percent(p / 100.0));
    }
    let Some(text) = line.strip_prefix("armor: ") else {
        return Progress::Noise;
    };
    if let Some((done, total)) = text
        .strip_suffix(" packages unpacked")
        .and_then(|t| t.split_once(" of "))
        && let (Ok(done), Ok(total)) = (done.parse::<f64>(), total.parse::<f64>())
        && total > 0.0
    {
        return Progress::Fraction(
            done / total,
            format!("Unpacking packages ({done} of {total})"),
        );
    }
    if let Some(rest) = text.strip_prefix("downloading ") {
        let (url, size) = rest.rsplit_once(" (").unwrap_or((rest, ""));
        let file = url.rsplit('/').next().unwrap_or(url);
        let size = size.trim_end_matches(')');
        return Progress::Step(if size.is_empty() {
            format!("Downloading {file}")
        } else {
            format!("Downloading {file} ({size})")
        });
    }
    if let Some(name) = text.strip_suffix(" installed; in the menu:") {
        return Progress::Done(format!("{name} is installed"));
    }
    if let Some(version) = text.strip_prefix("armor updated to ") {
        return Progress::Done(format!("armor was updated to {version}"));
    }
    if let Some(name) = text.strip_suffix(" removed") {
        return Progress::Done(format!("{name} was removed"));
    }
    if text.starts_with("preparing the Windows prefix") {
        return Progress::Step("Preparing a Windows environment".to_string());
    }
    if text.starts_with("installing Wine Mono") {
        return Progress::Step("Adding .NET support (Wine Mono)".to_string());
    }
    if text.starts_with("running ") {
        return Progress::Step("Running the installer: follow its window".to_string());
    }
    if let Some(file) = text.strip_prefix("unpacking ") {
        let file = Path::new(file)
            .file_name()
            .map_or(file.to_string(), |f| f.to_string_lossy().into_owned());
        return Progress::Step(format!("Unpacking {file}"));
    }
    if text.contains(" x86-64 packages from Debian")
        || text.contains(" x86-64 system packages from Debian")
    {
        return Progress::Step("Getting the libraries it needs from Debian".to_string());
    }
    if text.starts_with("verified") {
        return Progress::Step("Download verified".to_string());
    }
    if text == "press Enter to close this window" {
        return Progress::Noise;
    }
    Progress::Step(capitalized(text))
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The `armor` command: next to this program when installed together
/// (`/usr/bin`), else from the `PATH`.
fn armor() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("armor")))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("armor"))
}

/// Where armor keeps the installed applications (as `armor` computes it).
fn apps_dir() -> Option<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(data.join("armor/apps"))
}

/// The output of a command running in the background, read by the window.
#[derive(Default)]
struct Job {
    lines: Vec<String>,
    finished: Option<Result<bool, String>>,
}

/// The task panel at the bottom of the window.
struct Activity {
    revealer: gtk::Revealer,
    spinner: gtk::Spinner,
    result: gtk::Image,
    title: gtk::Label,
    status: gtk::Label,
    bar: gtk::ProgressBar,
    expander: gtk::Expander,
    details: gtk::TextBuffer,
    details_view: gtk::TextView,
    close: gtk::Button,
}

struct Ui {
    window: gtk::ApplicationWindow,
    flow: gtk::FlowBox,
    stack: gtk::Stack,
    banner: gtk::Revealer,
    /// The notice of a newer armor, with its button (Update, then Restart).
    update_banner: gtk::Revealer,
    update_label: gtk::Label,
    update_button: gtk::Button,
    /// The newer version on offer, or `None` once installed (the button
    /// then restarts the window).
    update_version: RefCell<Option<String>>,
    search: gtk::SearchEntry,
    install_buttons: Vec<gtk::Button>,
    activity: Activity,
    toast: gtk::Revealer,
    toast_label: gtk::Label,
    toast_generation: Cell<u64>,
    apps: RefCell<Vec<AppInfo>>,
    busy: Cell<bool>,
    refreshing: Cell<bool>,
    refresh_again: Cell<bool>,
    monitor: RefCell<Option<gio::FileMonitor>>,
}

impl Ui {
    fn detail(&self, line: &str) {
        let mut end = self.activity.details.end_iter();
        self.activity.details.insert(&mut end, line);
        self.activity.details.insert(&mut end, "\n");
        let mark =
            self.activity
                .details
                .create_mark(None, &self.activity.details.end_iter(), false);
        self.activity.details_view.scroll_mark_onscreen(&mark);
        self.activity.details.delete_mark(&mark);
    }

    fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        for button in &self.install_buttons {
            button.set_sensitive(!busy);
        }
    }

    /// A short message at the bottom of the window, for three seconds.
    fn toast(self: &Rc<Self>, text: &str) {
        self.toast_label.set_text(text);
        self.toast.set_reveal_child(true);
        let generation = self.toast_generation.get() + 1;
        self.toast_generation.set(generation);
        let ui = self.clone();
        glib::timeout_add_local_once(Duration::from_secs(3), move || {
            if ui.toast_generation.get() == generation {
                ui.toast.set_reveal_child(false);
            }
        });
    }

    fn matches(&self, app: &AppInfo) -> bool {
        let query = self.search.text().to_lowercase();
        let query = query.trim();
        query.is_empty()
            || app.name.to_lowercase().contains(query)
            || app
                .entries
                .iter()
                .any(|(e, _)| e.to_lowercase().contains(query))
    }

    /// Shows the grid, the empty page or the "no match" page.
    fn update_page(&self) {
        let apps = self.apps.borrow();
        let page = if apps.is_empty() {
            "empty"
        } else if apps.iter().any(|a| self.matches(a)) {
            "apps"
        } else {
            "nomatch"
        };
        self.stack.set_visible_child_name(page);
    }
}

/// Runs `armor args…` in the background, its progress in the task panel
/// (`title` names the task); then refreshes the applications.
/// What to do when a task ends, with whether it succeeded.
type Done = Box<dyn Fn(&Rc<Ui>, bool)>;

fn run_armor(ui: &Rc<Ui>, args: Vec<String>, title: &str, done: Option<Done>) {
    if ui.busy.get() {
        ui.toast("Wait for the current task to finish");
        return;
    }
    ui.set_busy(true);
    let activity = &ui.activity;
    activity.details.set_text("");
    ui.detail(&format!("$ armor {}", args.join(" ")));
    activity.title.set_text(title);
    activity.status.set_text("Starting…");
    activity.bar.set_fraction(0.0);
    activity.bar.set_visible(true);
    activity.result.set_visible(false);
    activity.spinner.set_visible(true);
    activity.spinner.start();
    activity.close.set_visible(false);
    activity.expander.set_expanded(false);
    activity.revealer.set_reveal_child(true);

    let job = Arc::new(Mutex::new(Job::default()));
    let worker = job.clone();
    std::thread::spawn(move || {
        let child = Command::new(armor())
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(e) => {
                worker.lock().expect("job lock").finished = Some(Err(e.to_string()));
                return;
            }
        };
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let readers = [
            reader(Box::new(stdout), worker.clone()),
            reader(Box::new(stderr), worker.clone()),
        ];
        let result = child
            .wait()
            .map(|status| status.success())
            .map_err(|e| e.to_string());
        // Programs an installer left running may keep the pipes open: do
        // not wait for them after armor itself ended.
        std::thread::sleep(Duration::from_millis(300));
        for handle in readers {
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
        worker.lock().expect("job lock").finished = Some(result);
    });
    let ui = ui.clone();
    let mut done_text: Option<String> = None;
    let mut last_message = String::new();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let mut job = job.lock().expect("job lock");
        let activity = &ui.activity;
        for line in job.lines.drain(..) {
            match progress_of(&line) {
                Progress::Percent(p) => {
                    activity.bar.set_fraction(p.clamp(0.0, 1.0));
                    continue;
                }
                Progress::Fraction(f, text) => {
                    activity.bar.set_fraction(f.clamp(0.0, 1.0));
                    activity.status.set_text(&text);
                }
                Progress::Step(text) => {
                    activity.bar.pulse();
                    activity.status.set_text(&text);
                    last_message = text;
                }
                Progress::Done(text) => done_text = Some(text),
                Progress::Noise => {}
            }
            ui.detail(&line);
        }
        let Some(result) = job.finished.take() else {
            return glib::ControlFlow::Continue;
        };
        activity.spinner.stop();
        activity.spinner.set_visible(false);
        activity.result.set_visible(true);
        activity.close.set_visible(true);
        let succeeded = matches!(result, Ok(true));
        match result {
            Ok(true) => {
                activity.result.set_icon_name(Some("emblem-ok-symbolic"));
                activity.bar.set_fraction(1.0);
                activity
                    .status
                    .set_text(done_text.as_deref().unwrap_or("Done"));
                let revealer = activity.revealer.clone();
                glib::timeout_add_local_once(Duration::from_secs(8), move || {
                    revealer.set_reveal_child(false)
                });
            }
            Ok(false) => {
                activity.result.set_icon_name(Some("dialog-error-symbolic"));
                activity.bar.set_visible(false);
                activity.status.set_text(if last_message.is_empty() {
                    "It did not finish well: see the details"
                } else {
                    &last_message
                });
                activity.expander.set_expanded(true);
            }
            Err(e) => {
                activity.result.set_icon_name(Some("dialog-error-symbolic"));
                activity.bar.set_visible(false);
                activity.status.set_text(&format!("Cannot run armor: {e}"));
            }
        }
        ui.set_busy(false);
        if let Some(done) = &done {
            done(&ui, succeeded);
        }
        refresh(&ui);
        glib::ControlFlow::Break
    });
}

/// A thread that moves the lines of `stream` into the job (a curl progress
/// bar's carriage returns end a line too).
fn reader(stream: Box<dyn Read + Send>, job: Arc<Mutex<Job>>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut stream = BufReader::new(stream);
        let mut line = Vec::new();
        while let Ok(n) = stream.read_until(b'\n', &mut line) {
            if n == 0 {
                break;
            }
            let text = String::from_utf8_lossy(&line);
            for part in text.split(['\r', '\n']).filter(|p| !p.trim().is_empty()) {
                job.lock().expect("job lock").lines.push(part.to_string());
            }
            line.clear();
        }
    })
}

/// Starts a program of an application; `armor run` waits for it, and says
/// in a desktop notification if it fails to start.
fn open(ui: &Rc<Ui>, app: &str, entry: &str) {
    let started = Command::new(armor())
        .args(["run", app, entry])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match started {
        Ok(mut child) => {
            // Reap it when it ends.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            ui.toast(&format!("Opening {entry}…"));
        }
        Err(e) => ui.toast(&format!("Cannot run armor: {e}")),
    }
}

fn confirm_remove(ui: &Rc<Ui>, app: &AppInfo) {
    let detail = match app.kind.as_str() {
        "windows" => {
            "Its Windows environment, with the program, its settings and its files, and its \
             menu entries are deleted."
        }
        "steam" => {
            "The Steam client, the games it downloaded into it and its sign-in on this computer \
             are deleted. Your Steam account and your games stay yours: install Steam again to \
             download them."
        }
        _ => "Its files, the libraries armor added for it, and its menu entries are deleted.",
    };
    let dialog = gtk::AlertDialog::builder()
        .modal(true)
        .message(format!("Remove {}?", app.name))
        .detail(detail)
        .buttons(["Cancel", "Remove"])
        .cancel_button(0)
        .default_button(0)
        .build();
    let ui = ui.clone();
    let name = app.name.clone();
    dialog.choose(
        Some(&ui.window.clone()),
        gio::Cancellable::NONE,
        move |choice| {
            if choice == Ok(1) {
                let title = format!("Removing {name}");
                run_armor(&ui, vec!["remove".to_string(), name], &title, None);
            }
        },
    );
}

/// The installers `armor install` accepts, by name.
const INSTALLER_PATTERNS: &[&str] = &[
    "*.exe",
    "*.msi",
    "*.deb",
    "*.AppImage",
    "*.appimage",
    "*.tar.gz",
    "*.tgz",
    "*.tar.xz",
    "*.txz",
    "*.tar.bz2",
    "*.tar.zst",
    "*.zip",
    "*.EXE",
    "*.MSI",
    "*.ZIP",
];

fn install(ui: &Rc<Ui>, path: &Path) {
    let file = path
        .file_name()
        .map_or(path.to_string_lossy(), |f| f.to_string_lossy());
    let title = format!("Installing {file}");
    run_armor(
        ui,
        vec!["install".to_string(), path.to_string_lossy().into_owned()],
        &title,
        None,
    );
}

fn choose_installer(ui: &Rc<Ui>) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(
        "x86 programs (.exe, .msi, .zip, .deb, .AppImage, .tar.gz, .tar.xz)",
    ));
    for pattern in INSTALLER_PATTERNS {
        filter.add_pattern(pattern);
    }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder()
        .title("Install an x86 program")
        .modal(true)
        .filters(&filters)
        .default_filter(&filter)
        .build();
    let ui = ui.clone();
    dialog.open(
        Some(&ui.window.clone()),
        gio::Cancellable::NONE,
        move |file| {
            if let Ok(file) = file
                && let Some(path) = file.path()
            {
                install(&ui, &path);
            }
        },
    );
}

/// Reads `armor list --porcelain` on another thread, then rebuilds the
/// cards; a refresh asked for meanwhile runs after it.
fn refresh(ui: &Rc<Ui>) {
    if ui.refreshing.get() {
        ui.refresh_again.set(true);
        return;
    }
    ui.refreshing.set(true);
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let output = gio::spawn_blocking(|| {
            Command::new(armor())
                .args(["list", "--porcelain"])
                .stdin(Stdio::null())
                .output()
        })
        .await;
        let records = match output {
            Ok(Ok(out)) if out.status.success() => {
                parse_porcelain(&String::from_utf8_lossy(&out.stdout))
            }
            Ok(Ok(out)) => {
                ui.toast(String::from_utf8_lossy(&out.stderr).trim());
                Vec::new()
            }
            Ok(Err(e)) => {
                ui.toast(&format!("Cannot run armor ({}): {e}", armor().display()));
                Vec::new()
            }
            Err(_) => Vec::new(),
        };
        populate(&ui, &records);
        ui.refreshing.set(false);
        if ui.refresh_again.replace(false) {
            refresh(&ui);
        }
    });
}

fn populate(ui: &Rc<Ui>, records: &[Record]) {
    ui.banner
        .set_reveal_child(records.contains(&Record::Engine { present: false }));
    while let Some(child) = ui.flow.first_child() {
        ui.flow.remove(&child);
    }
    let apps = apps_of(records);
    for (index, app) in apps.iter().enumerate() {
        let child = gtk::FlowBoxChild::new();
        child.set_widget_name(&index.to_string());
        child.set_child(Some(&card(ui, app)));
        ui.flow.append(&child);
    }
    *ui.apps.borrow_mut() = apps;
    ui.flow.invalidate_filter();
    ui.update_page();
}

/// The icon of an entry: its file, its theme name, or a generic one.
fn entry_icon(icon: &str) -> gtk::Image {
    let image = if icon.starts_with('/') && Path::new(icon).is_file() {
        gtk::Image::from_file(icon)
    } else if !icon.is_empty() && !icon.starts_with('/') {
        gtk::Image::from_icon_name(icon)
    } else {
        gtk::Image::from_icon_name("application-x-executable")
    };
    image.set_pixel_size(64);
    image
}

fn card(ui: &Rc<Ui>, app: &AppInfo) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("app-card");
    card.set_size_request(200, -1);
    let icon = app.entries.first().map_or("", |(_, icon)| icon.as_str());
    card.append(&entry_icon(icon));

    let name = gtk::Label::new(Some(&app.name.replace('-', " ")));
    name.add_css_class("app-name");
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.set_max_width_chars(20);
    name.set_tooltip_text(Some(&app.name));
    card.append(&name);

    let (badge_text, badge_class) = match app.kind.as_str() {
        "windows" => ("Windows", "windows"),
        "steam" => ("Steam", "steam"),
        _ => ("Linux", "linux"),
    };
    let badge = gtk::Label::new(Some(badge_text));
    badge.add_css_class("badge");
    badge.add_css_class(badge_class);
    badge.set_halign(gtk::Align::Center);
    card.append(&badge);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    actions.set_halign(gtk::Align::Center);
    actions.set_margin_top(6);
    match app.entries.first() {
        Some((entry, _)) => {
            let open_button = gtk::Button::with_label("Open");
            open_button.add_css_class("suggested-action");
            open_button.add_css_class("pill");
            open_button.set_tooltip_text(Some(entry));
            let (target, name, entry) = (ui.clone(), app.name.clone(), entry.clone());
            open_button.connect_clicked(move |_| open(&target, &name, &entry));
            actions.append(&open_button);
        }
        None => {
            let none = gtk::Label::new(Some("No menu entry"));
            none.add_css_class("dim");
            actions.append(&none);
        }
    }
    actions.append(&more_menu(ui, app));
    card.append(&actions);
    card
}

/// The "⋯" menu of a card: the other menu entries, then Remove.
fn more_menu(ui: &Rc<Ui>, app: &AppInfo) -> gtk::MenuButton {
    let menu = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let popover = gtk::Popover::new();
    for (entry, _) in app.entries.iter().skip(1) {
        let button = gtk::Button::with_label(&format!("Open {entry}"));
        button.add_css_class("flat");
        button.set_halign(gtk::Align::Fill);
        let (target, name, entry, pop) =
            (ui.clone(), app.name.clone(), entry.clone(), popover.clone());
        button.connect_clicked(move |_| {
            pop.popdown();
            open(&target, &name, &entry);
        });
        menu.append(&button);
    }
    if app.entries.len() > 1 {
        menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    }
    let remove = gtk::Button::with_label("Remove…");
    remove.add_css_class("flat");
    remove.add_css_class("destructive-action");
    let (target, info, pop) = (ui.clone(), app.clone(), popover.clone());
    remove.connect_clicked(move |_| {
        pop.popdown();
        confirm_remove(&target, &info);
    });
    menu.append(&remove);
    popover.set_child(Some(&menu));
    let button = gtk::MenuButton::new();
    button.set_icon_name("view-more-symbolic");
    button.add_css_class("circular");
    button.add_css_class("flat");
    button.set_tooltip_text(Some("More"));
    button.set_popover(Some(&popover));
    button
}

/// A centred page with a large icon, a title, a text and maybe a button.
fn status_page(icon: &str, title: &str, text: &str, button: Option<&gtk::Button>) -> gtk::Box {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    page.set_valign(gtk::Align::Center);
    page.set_halign(gtk::Align::Center);
    page.set_margin_start(24);
    page.set_margin_end(24);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(96);
    image.add_css_class("dim");
    page.append(&image);
    let heading = gtk::Label::new(Some(title));
    heading.add_css_class("empty-title");
    page.append(&heading);
    let body = gtk::Label::new(Some(text));
    body.set_wrap(true);
    body.set_justify(gtk::Justification::Center);
    body.set_max_width_chars(48);
    body.add_css_class("dim");
    page.append(&body);
    if let Some(button) = button {
        button.set_halign(gtk::Align::Center);
        button.set_margin_top(8);
        page.append(button);
    }
    page
}

fn install_button(label: &str) -> gtk::Button {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name("list-add-symbolic"));
    content.append(&gtk::Label::new(Some(label)));
    let button = gtk::Button::new();
    button.set_child(Some(&content));
    button.add_css_class("suggested-action");
    button
}

fn build_activity() -> (gtk::Revealer, Activity) {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 6);
    panel.add_css_class("activity");
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let spinner = gtk::Spinner::new();
    let result = gtk::Image::new();
    result.set_pixel_size(20);
    let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
    texts.set_hexpand(true);
    let title = gtk::Label::new(None);
    title.add_css_class("activity-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let status = gtk::Label::new(None);
    status.add_css_class("dim");
    status.set_xalign(0.0);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    texts.append(&title);
    texts.append(&status);
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.add_css_class("flat");
    close.add_css_class("circular");
    close.set_tooltip_text(Some("Hide"));
    top.append(&spinner);
    top.append(&result);
    top.append(&texts);
    top.append(&close);
    panel.append(&top);
    let bar = gtk::ProgressBar::new();
    bar.set_pulse_step(0.08);
    panel.append(&bar);
    let details = gtk::TextBuffer::new(None);
    let details_view = gtk::TextView::builder()
        .buffer(&details)
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .build();
    details_view.add_css_class("details");
    let scroller = gtk::ScrolledWindow::builder()
        .child(&details_view)
        .min_content_height(140)
        .max_content_height(220)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let expander = gtk::Expander::new(Some("Details"));
    expander.set_child(Some(&scroller));
    panel.append(&expander);
    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideUp)
        .child(&panel)
        .build();
    let hide = revealer.clone();
    close.connect_clicked(move |_| hide.set_reveal_child(false));
    (
        revealer.clone(),
        Activity {
            revealer,
            spinner,
            result,
            title,
            status,
            bar,
            expander,
            details,
            details_view,
            close,
        },
    )
}

/// The file whose time is that of the last automatic check for updates.
fn update_stamp() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(cache.join("armor/update-checked"))
}

/// At most one automatic check a day (ADR-0039).
fn update_check_due() -> bool {
    let Some(stamp) = update_stamp() else {
        return false;
    };
    let due = std::fs::metadata(&stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_none_or(|age| age > Duration::from_secs(24 * 60 * 60));
    if due {
        if let Some(dir) = stamp.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&stamp, b"");
    }
    due
}

/// What `armor update --check --porcelain` said: the installed and the
/// newest version, if a newer one exists.
fn parse_update(text: &str) -> Option<(String, String, bool)> {
    text.lines().find_map(|line| {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["update", available, installed, newest, ..] => Some((
                installed.to_string(),
                newest.to_string(),
                *available == "yes",
            )),
            _ => None,
        }
    })
}

/// Asks `armor update --check` on another thread; shows the update notice
/// if a newer armor exists, and (when the user asked) says so otherwise.
fn check_updates(ui: &Rc<Ui>, asked: bool) {
    if asked {
        ui.toast("Checking for updates…");
    }
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let output = gio::spawn_blocking(|| {
            Command::new(armor())
                .args(["update", "--check", "--porcelain"])
                .stdin(Stdio::null())
                .output()
        })
        .await;
        let Ok(Ok(output)) = output else {
            if asked {
                ui.toast("Cannot run armor to check for updates");
            }
            return;
        };
        match parse_update(&String::from_utf8_lossy(&output.stdout)) {
            Some((installed, newest, true)) => {
                ui.update_label.set_text(&format!(
                    "armor {newest} is available (you have {installed})."
                ));
                ui.update_button.set_label("Update");
                *ui.update_version.borrow_mut() = Some(newest);
                ui.update_banner.set_reveal_child(true);
            }
            Some((installed, _, false)) if asked => {
                ui.toast(&format!("armor {installed} is up to date"));
            }
            Some(_) => {}
            None if asked => {
                let error = String::from_utf8_lossy(&output.stderr);
                let line = error
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim_start_matches("armor: ");
                ui.toast(&capitalized(line));
            }
            None => {}
        }
    });
}

/// The update notice's button: Update, then Restart once installed.
fn update_clicked(ui: &Rc<Ui>) {
    let Some(version) = ui.update_version.borrow().clone() else {
        // Installed: start the new window and close this one.
        let window = armor().with_file_name("armor-gui");
        if Command::new(&window).spawn().is_ok() {
            ui.window.close();
        } else {
            ui.toast("Close armor and open it again from the menu");
        }
        return;
    };
    let title = format!("Updating armor to {version}");
    run_armor(
        ui,
        vec!["update".to_string()],
        &title,
        Some(Box::new(move |ui: &Rc<Ui>, succeeded| {
            if succeeded {
                ui.update_label.set_text(&format!(
                    "armor was updated to {version}: restart this window to use it."
                ));
                ui.update_button.set_label("Restart");
                *ui.update_version.borrow_mut() = None;
            }
        })),
    );
}

fn build(application: &gtk::Application) {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .title("armor")
        .default_width(1000)
        .default_height(680)
        .build();
    window.set_size_request(360, 420);

    let header = gtk::HeaderBar::new();
    let install_header = install_button("Install");
    install_header.set_tooltip_text(Some(
        "Install a program (.exe, .msi, .zip, .deb, .AppImage, .tar)",
    ));
    header.pack_start(&install_header);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search applications")
        .width_chars(16)
        .max_width_chars(32)
        .build();
    search.set_key_capture_widget(Some(&window));
    header.set_title_widget(Some(&search));
    let reload = gtk::Button::from_icon_name("view-refresh-symbolic");
    reload.set_tooltip_text(Some("Refresh"));
    // The main menu: things to install that are not files.
    let get_steam = gtk::Button::with_label("Install Steam (Valve's ARM64 client)");
    get_steam.add_css_class("flat");
    let check_button = gtk::Button::with_label("Check for updates");
    check_button.add_css_class("flat");
    let menu_items = gtk::Box::new(gtk::Orientation::Vertical, 2);
    menu_items.append(&get_steam);
    menu_items.append(&check_button);
    let main_menu = gtk::Popover::new();
    main_menu.set_child(Some(&menu_items));
    let menu_button = gtk::MenuButton::new();
    menu_button.set_icon_name("open-menu-symbolic");
    menu_button.set_tooltip_text(Some("Menu"));
    menu_button.set_popover(Some(&main_menu));
    header.pack_end(&menu_button);
    header.pack_end(&reload);
    window.set_titlebar(Some(&header));

    // The engine notice.
    let banner_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    banner_box.add_css_class("banner");
    banner_box.append(&gtk::Image::from_icon_name("dialog-information-symbolic"));
    let notice = gtk::Label::new(Some(
        "Windows programs need armor's Windows engine (Wine for ARM64 with FEX, about 380 MB).",
    ));
    notice.set_wrap(true);
    notice.set_xalign(0.0);
    notice.set_hexpand(true);
    banner_box.append(&notice);
    let fetch = gtk::Button::with_label("Get the engine");
    fetch.add_css_class("pill");
    banner_box.append(&fetch);
    let banner = gtk::Revealer::builder().child(&banner_box).build();

    // The notice of a newer armor.
    let update_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    update_box.add_css_class("banner");
    update_box.append(&gtk::Image::from_icon_name(
        "software-update-available-symbolic",
    ));
    let update_label = gtk::Label::new(None);
    update_label.set_wrap(true);
    update_label.set_xalign(0.0);
    update_label.set_hexpand(true);
    update_box.append(&update_label);
    let update_button = gtk::Button::with_label("Update");
    update_button.add_css_class("suggested-action");
    update_button.add_css_class("pill");
    update_box.append(&update_button);
    let update_banner = gtk::Revealer::builder().child(&update_box).build();

    // The cards.
    let flow = gtk::FlowBox::builder()
        .homogeneous(true)
        .min_children_per_line(1)
        .max_children_per_line(8)
        .column_spacing(16)
        .row_spacing(16)
        .selection_mode(gtk::SelectionMode::None)
        .activate_on_single_click(false)
        .valign(gtk::Align::Start)
        .margin_top(20)
        .margin_bottom(20)
        .margin_start(20)
        .margin_end(20)
        .build();
    let grid = gtk::ScrolledWindow::builder()
        .child(&flow)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();

    let install_empty = install_button("Install a program…");
    install_empty.add_css_class("pill");
    let empty = status_page(
        "system-software-install-symbolic",
        "No applications yet",
        "Drop an installer here, double-click it in the file manager, or choose one: \
         Windows .exe, .msi and .zip, Linux .deb, AppImage and .tar archives.",
        Some(&install_empty),
    );
    let nomatch = status_page(
        "system-search-symbolic",
        "No matching applications",
        "Try another name.",
        None,
    );
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .vexpand(true)
        .build();
    stack.add_named(&grid, Some("apps"));
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&nomatch, Some("nomatch"));

    let (activity_revealer, activity) = build_activity();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&update_banner);
    content.append(&banner);
    content.append(&stack);
    content.append(&activity_revealer);

    // Toasts over the content.
    let toast_label = gtk::Label::new(None);
    toast_label.set_wrap(true);
    let toast_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    toast_box.add_css_class("toast");
    toast_box.append(&toast_label);
    let toast = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::Crossfade)
        .child(&toast_box)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::End)
        .margin_bottom(24)
        .can_target(false)
        .build();
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&content));
    overlay.add_overlay(&toast);
    window.set_child(Some(&overlay));

    let ui = Rc::new(Ui {
        window: window.clone(),
        flow: flow.clone(),
        stack,
        banner,
        update_banner,
        update_label,
        update_button: update_button.clone(),
        update_version: RefCell::new(None),
        search: search.clone(),
        install_buttons: vec![install_header.clone(), install_empty.clone()],
        activity,
        toast,
        toast_label,
        toast_generation: Cell::new(0),
        apps: RefCell::new(Vec::new()),
        busy: Cell::new(false),
        refreshing: Cell::new(false),
        refresh_again: Cell::new(false),
        monitor: RefCell::new(None),
    });

    let target = Rc::downgrade(&ui);
    flow.set_filter_func(move |child| {
        let Some(ui) = target.upgrade() else {
            return true;
        };
        let apps = ui.apps.borrow();
        child
            .widget_name()
            .parse::<usize>()
            .ok()
            .and_then(|i| apps.get(i))
            .is_none_or(|app| ui.matches(app))
    });
    let target = ui.clone();
    flow.connect_child_activated(move |_, child| {
        let entry = child
            .widget_name()
            .parse::<usize>()
            .ok()
            .and_then(|i| target.apps.borrow().get(i).cloned());
        if let Some(app) = entry
            && let Some((entry, _)) = app.entries.first()
        {
            open(&target, &app.name, entry);
        }
    });
    let target = ui.clone();
    search.connect_search_changed(move |_| {
        target.flow.invalidate_filter();
        target.update_page();
    });
    for button in [&install_header, &install_empty] {
        let target = ui.clone();
        button.connect_clicked(move |_| choose_installer(&target));
    }
    let target = ui.clone();
    reload.connect_clicked(move |_| refresh(&target));
    let target = ui.clone();
    let menu = main_menu.clone();
    get_steam.connect_clicked(move |_| {
        menu.popdown();
        run_armor(
            &target,
            vec!["install".to_string(), "steam".to_string()],
            "Installing Steam",
            None,
        )
    });
    let target = ui.clone();
    let menu = main_menu.clone();
    check_button.connect_clicked(move |_| {
        menu.popdown();
        check_updates(&target, true);
    });
    let target = ui.clone();
    update_button.connect_clicked(move |_| update_clicked(&target));
    let target = ui.clone();
    fetch.connect_clicked(move |_| {
        run_armor(
            &target,
            vec!["engine".to_string(), "fetch".to_string()],
            "Getting the Windows engine",
            None,
        )
    });

    // Installers dropped on the window.
    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let target = ui.clone();
    drop.connect_drop(move |_, value, _, _| {
        target.window.remove_css_class("dropping");
        let Ok(list) = value.get::<gdk::FileList>() else {
            return false;
        };
        match list.files().first().and_then(|f| f.path()) {
            Some(path) => {
                install(&target, &path);
                true
            }
            None => false,
        }
    });
    let target = window.clone();
    drop.connect_enter(move |_, _, _| {
        target.add_css_class("dropping");
        gdk::DragAction::COPY
    });
    let target = window.clone();
    drop.connect_leave(move |_| target.remove_css_class("dropping"));
    window.add_controller(drop);

    // Refresh when the applications change outside the window (a double
    // click on an installer, `armor remove` in a terminal), once things
    // have settled.
    if let Some(dir) = apps_dir()
        && std::fs::create_dir_all(&dir).is_ok()
        && let Ok(monitor) = gio::File::for_path(&dir)
            .monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
    {
        let target = Rc::downgrade(&ui);
        let pending = Rc::new(Cell::new(false));
        monitor.connect_changed(move |_, _, _, _| {
            if pending.replace(true) {
                return;
            }
            let (target, pending) = (target.clone(), pending.clone());
            glib::timeout_add_local_once(Duration::from_millis(800), move || {
                pending.set(false);
                if let Some(ui) = target.upgrade()
                    && !ui.busy.get()
                {
                    refresh(&ui);
                }
            });
        });
        *ui.monitor.borrow_mut() = Some(monitor);
    }

    refresh(&ui);
    if update_check_due() {
        check_updates(&ui, false);
    }
    window.present();
    // Typing searches; the grid starts at its top.
    search.grab_focus();
}

fn main() -> glib::ExitCode {
    let application = gtk::Application::builder().application_id(APP_ID).build();
    application.connect_activate(build);
    application.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_records_are_read() {
        let text = "engine\t/x/wine\tno\napp\t7-Zip\twindows\tyes\tyes\n\
                    entry\t7-Zip\t7-Zip File Manager\t/x/7zFM.png\tfuture\n\
                    entry\t7-Zip\tHelp\nfuture\tfield\n";
        let records = parse_porcelain(text);
        assert_eq!(
            records,
            [
                Record::Engine { present: false },
                Record::App {
                    name: "7-Zip".to_string(),
                    kind: "windows".to_string()
                },
                Record::Entry {
                    app: "7-Zip".to_string(),
                    name: "7-Zip File Manager".to_string(),
                    icon: "/x/7zFM.png".to_string()
                },
                Record::Entry {
                    app: "7-Zip".to_string(),
                    name: "Help".to_string(),
                    icon: String::new()
                },
            ]
        );
        assert_eq!(
            apps_of(&records),
            [AppInfo {
                name: "7-Zip".to_string(),
                kind: "windows".to_string(),
                entries: vec![
                    ("7-Zip File Manager".to_string(), "/x/7zFM.png".to_string()),
                    ("Help".to_string(), String::new())
                ],
            }]
        );
    }

    #[test]
    fn progress_is_read_from_armor_messages() {
        // Lines as `armor install` prints them (apps.rs, debs.rs,
        // engines.rs).
        assert_eq!(
            progress_of("armor: 40 of 201 packages unpacked"),
            Progress::Fraction(40.0 / 201.0, "Unpacking packages (40 of 201)".to_string())
        );
        assert_eq!(
            progress_of("armor: downloading https://example.org/a/VC_redist.x64.exe (25 MB)"),
            Progress::Step("Downloading VC_redist.x64.exe (25 MB)".to_string())
        );
        assert_eq!(
            progress_of("############################                    56.3%"),
            Progress::Percent(0.563)
        );
        assert_eq!(progress_of("#=#=#"), Progress::Noise);
        assert_eq!(
            progress_of("armor: KeePassXC installed; in the menu:"),
            Progress::Done("KeePassXC is installed".to_string())
        );
        assert_eq!(
            progress_of("armor: 7zip removed"),
            Progress::Done("7zip was removed".to_string())
        );
        assert_eq!(
            progress_of("armor: running /home/u/Downloads/setup.exe"),
            Progress::Step("Running the installer: follow its window".to_string())
        );
        assert_eq!(
            progress_of("armor: unpacking /home/u/sublime_text_build_4200_x64.tar.xz"),
            Progress::Step("Unpacking sublime_text_build_4200_x64.tar.xz".to_string())
        );
        assert_eq!(
            progress_of(
                "armor: xnview needs 201 x86-64 packages from Debian's trixie archive (106 MB \
                 to download at most, cached for other applications)"
            ),
            Progress::Step("Getting the libraries it needs from Debian".to_string())
        );
        assert_eq!(
            progress_of("armor: something new"),
            Progress::Step("Something new".to_string())
        );
        assert_eq!(
            progress_of("armor: armor updated to 0.3.0"),
            Progress::Done("armor was updated to 0.3.0".to_string())
        );
        assert_eq!(
            parse_update("update\tyes\t0.2.0\t0.3.0\n"),
            Some(("0.2.0".to_string(), "0.3.0".to_string(), true))
        );
        assert_eq!(parse_update("armor: something else"), None);
        // Wine's and the installers' own lines go to "Details" only.
        assert_eq!(
            progress_of("0024:err:module:import_dll Library x.dll not found"),
            Progress::Noise
        );
        assert_eq!(
            progress_of("armor: press Enter to close this window"),
            Progress::Noise
        );
    }
}
