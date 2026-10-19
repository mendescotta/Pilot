mod renderer;
use gtk::{glib, prelude::*};
use pilot::{Access, Action, Backend, Inspection, Service, Snapshot, SystemManagerProfile};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    time::Duration,
};

#[derive(Clone)]
struct Row {
    widget: gtk::ListBoxRow,
    state: gtk::Label,
    summary: gtk::Label,
    icon: gtk::Image,
}
#[derive(Clone)]
struct Ui {
    backend: Backend,
    window: gtk::ApplicationWindow,
    list: gtk::ListBox,
    search: gtk::SearchEntry,
    filter: gtk::DropDown,
    title: gtk::Label,
    subtitle: gtk::Label,
    health: gtk::Label,
    connection: gtk::Label,
    message: gtk::Label,
    facts: gtk::Grid,
    definition: gtk::TextBuffer,
    relations: gtk::TextBuffer,
    diagnostics: gtk::TextBuffer,
    logs: gtk::TextBuffer,
    log_button: gtk::Button,
    primary: gtk::Button,
    restart: gtk::Button,
    advanced: gtk::MenuButton,
    unlock: gtk::Button,
    rows: Rc<RefCell<BTreeMap<String, Row>>>,
    services: Rc<RefCell<Vec<Service>>>,
    selected: Rc<RefCell<Option<String>>>,
    pending: Rc<RefCell<Vec<String>>>,
    refresh_busy: Rc<Cell<bool>>,
    confirmed: Rc<Cell<bool>>,
    generation: Rc<Cell<u64>>,
    monitors: Rc<RefCell<Vec<gtk::gio::FileMonitor>>>,
    invalidated: Rc<Cell<bool>>,
    event_busy: Rc<Cell<bool>>,
    revision: Rc<Cell<Option<u64>>>,
    advanced_actions: Rc<RefCell<Vec<(Action, gtk::Button)>>>,
}
fn label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    if !class.is_empty() {
        label.add_css_class(class);
    }
    label
}
fn padded(orientation: gtk::Orientation, spacing: i32, margin: i32) -> gtk::Box {
    let b = gtk::Box::new(orientation, spacing);
    b.set_margin_top(margin);
    b.set_margin_bottom(margin);
    b.set_margin_start(margin);
    b.set_margin_end(margin);
    b
}
fn text_page() -> (gtk::ScrolledWindow, gtk::TextBuffer) {
    let view = gtk::TextView::new();
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_top_margin(18);
    view.set_bottom_margin(18);
    view.set_left_margin(18);
    view.set_right_margin(18);
    (
        gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .hexpand(true)
            .build(),
        view.buffer(),
    )
}
/// Filter buckets, in dropdown order after "All services".
const FILTERS: [&[&str]; 6] = [
    &["Started", "Paused"],
    &["Failed"],
    &["Stopped", "Skipped"],
    &["Starting", "Stopping", "Finishing"],
    &["Not loaded", "Available", "Template"],
    &["Unknown"],
];
fn state_icon(state: &str) -> &'static str {
    match state {
        "Started" => "media-playback-start-symbolic",
        "Failed" => "dialog-error-symbolic",
        "Paused" => "media-playback-pause-symbolic",
        "Starting" | "Stopping" | "Finishing" => "content-loading-symbolic",
        "Unknown" => "dialog-question-symbolic",
        "Template" => "text-x-generic-symbolic",
        _ => "media-playback-stop-symbolic",
    }
}
impl Ui {
    fn relation_dialog(&self, persistent: bool) {
        let Some(name) = self.selected.borrow().clone() else {
            return;
        };
        let dialog = gtk::Window::builder()
            .title(if persistent {
                "Persistent activation"
            } else {
                "Runtime dependency"
            })
            .transient_for(&self.window)
            .modal(true)
            .default_width(480)
            .build();
        let root = padded(gtk::Orientation::Vertical, 12, 24);
        root.append(&label(&name, "section-title"));
        let note = label(
            if persistent {
                "Choose the service that should persistently depend on this one. Enabling also starts this service; disabling stops it unless something else requires it."
            } else {
                "Changes apply to the running manager. Removing a dependency may stop its target; definition files stay unchanged."
            },
            "dim-label",
        );
        note.set_wrap(true);
        root.append(&note);
        let target = gtk::Entry::new();
        target.set_placeholder_text(Some(if persistent {
            "Enable via (for example boot)"
        } else {
            "Dependency target service"
        }));
        if persistent {
            target.set_text("boot");
        }
        root.append(&target);
        let kind = gtk::DropDown::from_strings(&["need", "milestone", "waits-for"]);
        kind.set_visible(!persistent);
        root.append(&kind);
        let error = label("", "error");
        error.set_wrap(true);
        root.append(&error);
        for add in [true, false] {
            let button = gtk::Button::with_label(if persistent {
                if add {
                    "Enable via target"
                } else {
                    "Disable via target"
                }
            } else if add {
                "Add runtime dependency"
            } else {
                "Remove runtime dependency"
            });
            root.append(&button);
            let ui = self.clone();
            let name = name.clone();
            let target = target.clone();
            let kind = kind.clone();
            let error = error.clone();
            let dialog = dialog.clone();
            button.connect_clicked(move |_| {
                let target = target.text().to_string();
                if let Err(e) = pilot::validate_name(&target) {
                    error.set_text(&e);
                    return;
                }
                let relation = if persistent {
                    pilot::DinitRelation::Persistent {
                        from: target,
                        enabled: add,
                    }
                } else {
                    pilot::DinitRelation::Runtime {
                        target,
                        kind: [
                            pilot::DependencyKind::Need,
                            pilot::DependencyKind::Milestone,
                            pilot::DependencyKind::WaitsFor,
                        ][kind.selected() as usize],
                        add,
                    }
                };
                let name = name.clone();
                let error = error.clone();
                let dialog = dialog.clone();
                ui.job(
                    move || Backend::Dinit.change_relation(&name, relation),
                    move |ui, result| match result {
                        Ok(text) => {
                            dialog.close();
                            ui.message.set_text(&text);
                            ui.refresh(false);
                        }
                        Err(e) => error.set_text(&e),
                    },
                );
            });
        }
        dialog.set_child(Some(&root));
        dialog.present();
    }
    fn watch_paths(&self) {
        use gtk::gio::prelude::*;
        self.monitors.borrow_mut().clear();
        let mut paths = if self.backend == Backend::Runit {
            vec![
                std::path::PathBuf::from("/etc/sv"),
                std::fs::canonicalize("/var/service").unwrap_or_else(|_| "/var/service".into()),
            ]
        } else {
            pilot::dinit::roots()
        };
        if self.backend == Backend::Runit {
            for service in self.services.borrow().iter() {
                paths.push(
                    std::path::Path::new("/var/service")
                        .join(&service.name)
                        .join("supervise"),
                );
            }
        }
        for path in paths {
            if let Ok(monitor) = gtk::gio::File::for_path(path).monitor_directory(
                gtk::gio::FileMonitorFlags::WATCH_MOVES,
                None::<&gtk::gio::Cancellable>,
            ) {
                let invalidated = self.invalidated.clone();
                monitor.connect_changed(move |_, _, _, _| invalidated.set(true));
                self.monitors.borrow_mut().push(monitor);
            }
        }
    }
    fn definition_dialog(&self) {
        let dialog = gtk::Window::builder()
            .title("Administrator definition")
            .transient_for(&self.window)
            .modal(true)
            .default_width(680)
            .default_height(660)
            .build();
        let root = padded(gtk::Orientation::Vertical, 12, 24);
        root.append(&label("Administrator definition", "section-title"));
        let note=label("Create under a new name to keep package definitions intact. Dinit replacements require a Pilot-created, unowned file and keep a recoverable backup.","dim-label");
        note.set_wrap(true);
        root.append(&note);
        let name = gtk::Entry::new();
        name.set_placeholder_text(Some("New service name"));
        root.append(&name);
        let kind = gtk::DropDown::from_strings(&[
            "process",
            "bgprocess",
            "scripted",
            "internal",
            "triggered",
        ]);
        kind.set_visible(self.backend == Backend::Dinit);
        root.append(&kind);
        let command = gtk::Entry::new();
        command.set_placeholder_text(Some("Command (not needed for internal / triggered)"));
        root.append(&command);
        let pid_file = gtk::Entry::new();
        pid_file.set_placeholder_text(Some("Absolute PID file (bgprocess only)"));
        pid_file.set_visible(self.backend == Backend::Dinit);
        root.append(&pid_file);
        let buffered = gtk::CheckButton::with_label("Buffered output");
        buffered.set_visible(self.backend == Backend::Dinit);
        root.append(&buffered);
        let down = gtk::CheckButton::with_label("Initially down");
        down.set_visible(self.backend == Backend::Runit);
        root.append(&down);
        let logger =
            gtk::CheckButton::with_label("Add svlogd logger (1 MB rotation, retain 10 files)");
        logger.set_visible(self.backend == Backend::Runit);
        root.append(&logger);
        let replace = gtk::CheckButton::with_label(
            "Replace a Pilot-created dinit definition, keeping a backup",
        );
        replace.set_visible(self.backend == Backend::Dinit);
        root.append(&replace);
        let path = label("", "dim-label");
        path.set_wrap(true);
        root.append(&path);
        let (page, preview) = text_page();
        root.append(&page);
        let error = label("", "error");
        error.set_wrap(true);
        root.append(&error);
        let apply = gtk::Button::with_label("Validate and install displayed definition");
        apply.add_css_class("suggested-action");
        root.append(&apply);
        let draft: Rc<dyn Fn() -> pilot::admin::DefinitionDraft> = Rc::new({
            let name = name.clone();
            let kind = kind.clone();
            let command = command.clone();
            let pid_file = pid_file.clone();
            let buffered = buffered.clone();
            let down = down.clone();
            let logger = logger.clone();
            move || pilot::admin::DefinitionDraft {
                name: name.text().to_string(),
                kind: ["process", "bgprocess", "scripted", "internal", "triggered"]
                    [kind.selected() as usize]
                    .into(),
                command: command.text().to_string(),
                pid_file: pid_file.text().to_string(),
                buffered: buffered.is_active(),
                initially_down: down.is_active(),
                logger: logger.is_active(),
            }
        });
        let update: Rc<dyn Fn()> = Rc::new({
            let draft = draft.clone();
            let preview = preview.clone();
            let path = path.clone();
            let backend = self.backend;
            move || {
                let d = draft();
                let text = d.preview(backend).unwrap_or_else(|e| e);
                preview.set_text(&text);
                path.set_text(&format!(
                    "{}{}{}",
                    if backend == Backend::Dinit {
                        "/etc/dinit.d/"
                    } else {
                        "/etc/sv/"
                    },
                    d.name,
                    if backend == Backend::Runit {
                        "/run"
                    } else {
                        ""
                    }
                ));
            }
        });
        for entry in [&name, &command, &pid_file] {
            let u = update.clone();
            entry.connect_changed(move |_| u());
        }
        let u = update.clone();
        kind.connect_selected_notify(move |_| u());
        for check in [&buffered, &down, &logger] {
            let u = update.clone();
            check.connect_toggled(move |_| u());
        }
        update();
        let ui = self.clone();
        let backend = self.backend;
        let d = dialog.clone();
        apply.connect_clicked(move |button| {
            let draft = draft();
            if let Err(e) = draft.render(backend) {
                error.set_text(&e);
                return;
            }
            let replace = replace.is_active();
            let error = error.clone();
            let d = d.clone();
            let button = button.clone();
            button.set_sensitive(false);
            ui.job(
                move || backend.install_definition(&draft, replace),
                move |ui, result| {
                    button.set_sensitive(true);
                    match result {
                        Ok(text) => {
                            d.close();
                            ui.message.set_text(&text);
                            ui.refresh(false);
                        }
                        Err(e) => error.set_text(&e),
                    }
                },
            );
        });
        dialog.set_child(Some(&root));
        dialog.present();
    }
    fn job<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, String> + Send + 'static,
        done: impl FnOnce(&Self, Result<T, String>) + 'static,
    ) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
        let weak = self.window.downgrade();
        let ui = self.clone();
        let mut done = Some(done);
        glib::timeout_add_local(Duration::from_millis(40), move || {
            if weak.upgrade().is_none() {
                return glib::ControlFlow::Break;
            }
            match rx.try_recv() {
                Ok(result) => {
                    if let Some(done) = done.take() {
                        done(&ui, result);
                    }
                    glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(_) => {
                    if let Some(done) = done.take() {
                        done(&ui, Err("Worker disconnected".into()));
                    }
                    glib::ControlFlow::Break
                }
            }
        });
    }
    fn refresh(&self, unlock: bool) {
        if self.refresh_busy.replace(true) {
            return;
        }
        let backend = self.backend;
        self.unlock.set_sensitive(false);
        self.job(
            move || backend.snapshot(unlock),
            |ui, result| {
                ui.refresh_busy.set(false);
                ui.unlock.set_sensitive(true);
                match result {
                    Ok(snapshot) => ui.apply_snapshot(snapshot),
                    Err(e) => {
                        ui.connection
                            .set_text("Disconnected · last snapshot retained");
                        ui.message.set_text(&e);
                    }
                }
            },
        );
    }
    fn apply_snapshot(&self, mut snapshot: Snapshot) {
        let connected = snapshot.access == Access::Connected;
        match &snapshot.access {
            Access::Connected => {
                self.connection.set_text("Connected");
                self.unlock.set_visible(false);
                self.confirmed.set(true);
            }
            Access::AuthenticationRequired => {
                self.connection.set_text(if self.confirmed.get() {
                    "Locked · last snapshot retained"
                } else {
                    "Authentication required"
                });
                self.unlock.set_visible(true);
            }
            Access::Unavailable(e) => {
                self.connection
                    .set_text("Unavailable · last snapshot retained");
                self.message.set_text(e);
                self.unlock.set_visible(true);
            }
        }
        if !connected && self.confirmed.get() {
            return;
        }
        for service in &mut snapshot.services {
            if service.state == "Unknown" {
                if let Some(old) = self
                    .services
                    .borrow()
                    .iter()
                    .find(|old| old.name == service.name && old.state != "Unknown")
                {
                    service.state = old.state.clone();
                    service.summary =
                        format!("Status unavailable; last confirmed state: {}", old.state);
                }
            }
        }
        let changed = *self.services.borrow() != snapshot.services;
        if changed {
            let mut rows = self.rows.borrow_mut();
            rows.retain(|name, row| {
                if snapshot.services.iter().any(|s| &s.name == name) {
                    true
                } else {
                    self.list.remove(&row.widget);
                    false
                }
            });
            for (index, service) in snapshot.services.iter().enumerate() {
                let row = rows.entry(service.name.clone()).or_insert_with(|| {
                    let widget = gtk::ListBoxRow::new();
                    widget.set_widget_name(&service.name);
                    let content = padded(gtk::Orientation::Horizontal, 12, 12);
                    let icon = gtk::Image::from_icon_name(state_icon(&service.state));
                    content.append(&icon);
                    let lines = gtk::Box::new(gtk::Orientation::Vertical, 4);
                    lines.set_hexpand(true);
                    let name = label(&service.name, "service-name");
                    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    let summary = label("", "dim-label");
                    summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    summary.set_max_width_chars(30);
                    lines.append(&name);
                    lines.append(&summary);
                    content.append(&lines);
                    let state = label("", "badge");
                    content.append(&state);
                    widget.set_child(Some(&content));
                    self.list.insert(&widget, index as i32);
                    Row {
                        widget,
                        state,
                        summary,
                        icon,
                    }
                });
                row.state.set_text(if service.state == "Started" {
                    "Running"
                } else {
                    &service.state
                });
                row.summary.set_text(&service.summary);
                row.icon.set_icon_name(Some(state_icon(&service.state)));
                row.widget.set_tooltip_text(Some(&service.summary));
                for class in ["failure", "running"] {
                    row.widget.remove_css_class(class);
                }
                if service.state == "Failed" {
                    row.widget.add_css_class("failure");
                }
                if service.state == "Started" {
                    row.widget.add_css_class("running");
                }
            }
            drop(rows);
            *self.services.borrow_mut() = snapshot.services;
            self.apply_filter();
            self.watch_paths();
        }
        let services = self.services.borrow();
        let count = |bucket: &[&str]| {
            services
                .iter()
                .filter(|s| bucket.contains(&s.state.as_str()))
                .count()
        };
        let mut health = format!(
            "{} running     {} failed     {} stopped",
            count(FILTERS[0]),
            count(FILTERS[1]),
            count(FILTERS[2])
        );
        for (n, text) in [
            (count(FILTERS[3]), "in transition"),
            (count(FILTERS[4]), "inactive"),
        ] {
            if n > 0 {
                health.push_str(&format!("     {n} {text}"));
            }
        }
        health.push_str(&format!("     {} definitions", services.len()));
        self.health.set_text(&health);
        if !connected {
            self.health.set_text(&format!(
                "{} definitions · live health counts unavailable",
                services.len()
            ));
        }
        if services.is_empty() {
            self.message.set_text("No system definitions were found. Check the PID 1 evidence and service search paths.");
        }
        if let Some(name) = self.selected.borrow().clone() {
            if let Some(service) = services.iter().find(|s| s.name == name) {
                self.subtitle.set_text(&service.summary);
                self.primary.set_label(
                    if matches!(service.state.as_str(), "Started" | "Starting" | "Paused") {
                        "Stop"
                    } else {
                        "Start"
                    },
                );
            } else {
                self.title.set_text("Select a service");
                self.subtitle
                    .set_text("The selected service is no longer listed.");
                self.primary.set_sensitive(false);
                self.restart.set_sensitive(false);
                self.advanced.set_sensitive(false);
            }
        }
        drop(services);
        if let Some(name) = self.selected.borrow().clone() {
            self.controls_for(&name);
        }
        if self.selected.borrow().is_none() {
            if let Some(row) = self.list.row_at_index(0) {
                self.list.select_row(Some(&row));
            }
        }
    }
    fn apply_filter(&self) {
        let query = self.search.text().to_lowercase();
        let filter = self.filter.selected() as usize;
        for service in self.services.borrow().iter() {
            let matches = filter == 0
                || FILTERS
                    .get(filter - 1)
                    .is_none_or(|bucket| bucket.contains(&service.state.as_str()));
            if let Some(row) = self.rows.borrow().get(&service.name) {
                row.widget.set_visible(
                    matches
                        && (service.name.to_lowercase().contains(&query)
                            || service.state.to_lowercase().contains(&query)),
                );
            }
        }
    }
    fn selection(&self, name: Option<String>) {
        *self.selected.borrow_mut() = name.clone();
        self.generation.set(self.generation.get() + 1);
        let Some(name) = name else {
            return;
        };
        let generation = self.generation.get();
        self.title.set_text(&name);
        self.subtitle.set_text("Reading service details…");
        self.controls_for(&name);
        self.definition.set_text("Loading definition…");
        self.relations.set_text("Loading relationships…");
        self.logs
            .set_text("Choose Read output to fetch available logs.");
        let backend = self.backend;
        self.job(
            move || {
                let mut inspection = backend.inspect(&name)?;
                let status = backend
                    .status(&name)
                    .unwrap_or_else(|e| format!("Live status unavailable: {e}"));
                inspection.apply_status(backend, status);
                Ok(inspection)
            },
            move |ui, result| {
                if ui.generation.get() != generation {
                    return;
                }
                match result {
                    Ok(inspection) => ui.show_inspection(inspection),
                    Err(e) => {
                        ui.subtitle.set_text(&e);
                        ui.definition.set_text(&e);
                    }
                }
            },
        );
    }
    fn controls_for(&self, name: &str) {
        let idle = !self.pending.borrow().iter().any(|n| n == name);
        let service = self
            .services
            .borrow()
            .iter()
            .find(|s| s.name == name)
            .cloned();
        let known = service.as_ref().is_some_and(|s| {
            s.state != "Unknown"
                && !matches!(s.state.as_str(), "Available" | "Template")
                && !s.summary.starts_with("Status unavailable")
        });
        self.primary.set_sensitive(idle && known);
        self.restart.set_sensitive(
            idle && service
                .as_ref()
                .is_some_and(|s| matches!(s.state.as_str(), "Started" | "Paused")),
        );
        self.advanced
            .set_sensitive(idle && service.as_ref().is_none_or(|s| s.state != "Template"));
        let running = self
            .services
            .borrow()
            .iter()
            .find(|s| s.name == name)
            .is_some_and(|s| matches!(s.state.as_str(), "Started" | "Starting" | "Paused"));
        self.primary
            .set_label(if running { "Stop" } else { "Start" });
    }
    fn show_inspection(&self, inspection: Inspection) {
        let setting = |name: &str| {
            inspection
                .facts
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        for (action, button) in self.advanced_actions.borrow().iter() {
            button.set_sensitive(match action {
                Action::Trigger | Action::Untrigger => setting("type") == Some("triggered"),
                Action::ClearLog => setting("log-type") == Some("buffer"),
                _ => true,
            });
        }
        while let Some(child) = self.facts.first_child() {
            self.facts.remove(&child);
        }
        for (i, (key, value)) in inspection.facts.iter().enumerate() {
            let key = label(key, "dim-label");
            key.set_valign(gtk::Align::Start);
            let value = label(value, "");
            value.set_wrap(true);
            value.set_selectable(true);
            value.set_hexpand(true);
            self.facts.attach(&key, 0, i as i32, 1, 1);
            self.facts.attach(&value, 1, i as i32, 1, 1);
        }
        self.definition.set_text(&inspection.definition);
        self.relations.set_text(&inspection.relations);
        self.diagnostics.set_text(&inspection.diagnostics);
        self.logs.set_text(&inspection.log_description);
        self.log_button.set_sensitive(inspection.log_available);
        if let Some(service) = self
            .services
            .borrow()
            .iter()
            .find(|s| s.name == inspection.name)
        {
            self.subtitle.set_text(&service.summary);
        }
    }
    fn action(&self, action: Action) {
        let Some(name) = self.selected.borrow().clone() else {
            return;
        };
        if self.pending.borrow().contains(&name) {
            return;
        }
        if action.needs_confirmation() {
            let dialog = gtk::Window::builder()
                .transient_for(&self.window)
                .modal(true)
                .title(action.label())
                .default_width(420)
                .build();
            let root = padded(gtk::Orientation::Vertical, 18, 24);
            let title = label(&format!("{} {name}?", action.label()), "section-title");
            title.set_wrap(true);
            root.append(&title);
            let text = label(action.consequence(self.backend), "dim-label");
            text.set_wrap(true);
            root.append(&text);
            let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            buttons.set_halign(gtk::Align::End);
            let cancel = gtk::Button::with_label("Cancel");
            let confirm = gtk::Button::with_label(action.label());
            confirm.add_css_class(if action == Action::Enable {
                "suggested-action"
            } else {
                "destructive-action"
            });
            buttons.append(&cancel);
            buttons.append(&confirm);
            root.append(&buttons);
            dialog.set_child(Some(&root));
            let d = dialog.clone();
            cancel.connect_clicked(move |_| d.close());
            let ui = self.clone();
            let d = dialog.clone();
            confirm.connect_clicked(move |_| {
                d.close();
                ui.execute(action, name.clone());
            });
            dialog.present();
        } else {
            self.execute(action, name);
        }
    }
    fn execute(&self, action: Action, name: String) {
        self.pending.borrow_mut().push(name.clone());
        self.controls_for(&name);
        self.message
            .set_text(&format!("{}: {}…", name, action.label()));
        let backend = self.backend;
        let task_name = name.clone();
        self.job(
            move || backend.execute(action, &task_name),
            move |ui, result| {
                ui.pending.borrow_mut().retain(|n| n != &name);
                ui.message.set_text(&match result {
                    Ok(text) if text.is_empty() => {
                        format!("{}: request accepted; waiting for manager state", name)
                    }
                    Ok(text) => text,
                    Err(e) => e,
                });
                if ui.selected.borrow().as_deref() == Some(&name) {
                    ui.controls_for(&name);
                    ui.selection(Some(name.clone()));
                }
                ui.refresh(false);
            },
        );
    }
    fn read_logs(&self) {
        let Some(name) = self.selected.borrow().clone() else {
            return;
        };
        let backend = self.backend;
        let generation = self.generation.get();
        self.job(
            move || backend.logs(&name),
            move |ui, result| {
                if ui.generation.get() == generation {
                    ui.logs.set_text(&result.unwrap_or_else(|e| e));
                }
            },
        );
    }
}
fn build(app: &gtk::Application, profile: &SystemManagerProfile) {
    let css = gtk::CssProvider::new();
    css.load_from_data(include_str!("../assets/pilot.css"));
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Pilot · System services")
        .icon_name("org.voidlab.Pilot")
        .default_width(1120)
        .default_height(760)
        .build();
    let header = gtk::HeaderBar::new();
    let heading = gtk::Box::new(gtk::Orientation::Vertical, 2);
    heading.append(&label("Pilot", "app-title"));
    heading.append(&label("System services", "dim-label"));
    header.set_title_widget(Some(&heading));
    window.set_titlebar(Some(&header));
    let Some(backend) = profile.backend else {
        let root = padded(gtk::Orientation::Vertical, 18, 48);
        root.set_valign(gtk::Align::Center);
        root.append(&gtk::Image::from_icon_name("dialog-information-symbolic"));
        root.append(&label("Unsupported system manager", "page-title"));
        let evidence = label(&profile.evidence(), "dim-label");
        evidence.set_wrap(true);
        evidence.set_selectable(true);
        root.append(&evidence);
        window.set_child(Some(&root));
        window.present();
        return;
    };
    let connection = label("Checking supervisor…", "dim-label");
    header.pack_start(&label(&format!("{} · PID 1", backend.label()), "badge"));
    header.pack_start(&connection);
    let unlock = gtk::Button::with_label("Unlock live status");
    unlock.add_css_class("suggested-action");
    header.pack_end(&unlock);
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    let create = gtk::Button::from_icon_name("list-add-symbolic");
    create.set_tooltip_text(Some("Create an administrator definition"));
    header.pack_end(&create);
    refresh.set_tooltip_text(Some("Refresh without prompting for authentication"));
    header.pack_end(&refresh);
    let evidence = gtk::MenuButton::builder()
        .icon_name("dialog-information-symbolic")
        .build();
    let popover = gtk::Popover::new();
    let e = label(&profile.evidence(), "");
    e.set_selectable(true);
    e.set_wrap(true);
    e.set_max_width_chars(65);
    let p = padded(gtk::Orientation::Vertical, 12, 18);
    p.append(&e);
    popover.set_child(Some(&p));
    evidence.set_popover(Some(&popover));
    header.pack_end(&evidence);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let health = label("Discovering system services…", "health");
    let summary = padded(gtk::Orientation::Horizontal, 12, 16);
    summary.append(&health);
    root.append(&summary);
    if profile.overridden {
        let warning = label(
            "Development override active — operations still address the system manager",
            "warning",
        );
        root.append(&warning);
    }
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let pane = gtk::Paned::new(gtk::Orientation::Horizontal);
    pane.set_position(350);
    pane.set_vexpand(true);
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar.add_css_class("service-sidebar");
    sidebar.set_size_request(300, -1);
    let tools = padded(gtk::Orientation::Vertical, 10, 12);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search services…"));
    tools.append(&search);
    let filter = gtk::DropDown::from_strings(&[
        "All services",
        "Running",
        "Failed",
        "Stopped",
        "In transition",
        "Inactive definitions",
        "Status unknown",
    ]);
    tools.append(&filter);
    sidebar.append(&tools);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("service-list");
    sidebar.append(
        &gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .build(),
    );
    pane.set_start_child(Some(&sidebar));
    let detail = gtk::Box::new(gtk::Orientation::Vertical, 0);
    detail.set_hexpand(true);
    let service_header = padded(gtk::Orientation::Vertical, 12, 24);
    let title = label("Your system, at a glance", "page-title");
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    service_header.append(&title);
    let subtitle = label(
        "Select a service to inspect its configuration and supervisor state.",
        "dim-label",
    );
    subtitle.set_wrap(true);
    subtitle.set_selectable(true);
    service_header.append(&subtitle);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let primary = gtk::Button::with_label("Start");
    primary.add_css_class("suggested-action");
    let restart = gtk::Button::with_label("Restart");
    let advanced = gtk::MenuButton::builder().label("More").build();
    primary.set_sensitive(false);
    restart.set_sensitive(false);
    advanced.set_sensitive(false);
    controls.append(&primary);
    controls.append(&restart);
    controls.append(&advanced);
    service_header.append(&controls);
    detail.append(&service_header);
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    let switch = gtk::StackSwitcher::new();
    switch.set_stack(Some(&stack));
    switch.set_halign(gtk::Align::Start);
    switch.set_margin_start(24);
    switch.set_margin_bottom(12);
    detail.append(&switch);
    let overview = padded(gtk::Orientation::Vertical, 18, 24);
    let facts = gtk::Grid::new();
    facts.set_row_spacing(14);
    facts.set_column_spacing(24);
    overview.append(&facts);
    let diagnostics_expander = gtk::Expander::new(Some("Supervisor diagnostics"));
    let (diagnostic_page, diagnostics) = text_page();
    diagnostic_page.set_min_content_height(160);
    diagnostics_expander.set_child(Some(&diagnostic_page));
    overview.append(&diagnostics_expander);
    stack.add_titled(
        &gtk::ScrolledWindow::builder().child(&overview).build(),
        Some("overview"),
        "Overview",
    );
    let (page, definition) = text_page();
    stack.add_titled(&page, Some("definition"), "Definition");
    let (page, relations) = text_page();
    stack.add_titled(
        &page,
        Some("relations"),
        if backend == Backend::Dinit {
            "Relations"
        } else {
            "Components"
        },
    );
    let logs_page = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let log_button = gtk::Button::with_label("Read output");
    log_button.set_halign(gtk::Align::Start);
    log_button.set_margin_start(18);
    log_button.set_sensitive(false);
    logs_page.append(&log_button);
    let (page, logs) = text_page();
    logs_page.append(&page);
    stack.add_titled(&logs_page, Some("logs"), "Logs");
    detail.append(&stack);
    pane.set_end_child(Some(&detail));
    root.append(&pane);
    let message = label("", "dim-label");
    message.set_wrap(true);
    message.set_selectable(true);
    message.set_margin_start(18);
    message.set_margin_end(18);
    message.set_margin_top(8);
    message.set_margin_bottom(8);
    root.append(&message);
    window.set_child(Some(&root));
    let ui = Ui {
        backend,
        window,
        list,
        search,
        filter,
        title,
        subtitle,
        health,
        connection,
        message,
        facts,
        definition,
        relations,
        diagnostics,
        logs,
        log_button,
        primary,
        restart,
        advanced,
        unlock,
        rows: Default::default(),
        services: Default::default(),
        selected: Default::default(),
        pending: Default::default(),
        refresh_busy: Default::default(),
        confirmed: Default::default(),
        generation: Default::default(),
        monitors: Default::default(),
        invalidated: Default::default(),
        event_busy: Default::default(),
        revision: Default::default(),
        advanced_actions: Default::default(),
    };
    let pop = gtk::Popover::new();
    let actions = padded(gtk::Orientation::Vertical, 4, 8);
    for action in backend
        .actions()
        .into_iter()
        .filter(|a| !matches!(a, Action::Start | Action::Stop | Action::Restart))
    {
        let button = gtk::Button::with_label(action.label());
        ui.advanced_actions
            .borrow_mut()
            .push((action, button.clone()));
        button.add_css_class("flat");
        actions.append(&button);
        let ui = ui.clone();
        let pop = pop.clone();
        button.connect_clicked(move |_| {
            pop.popdown();
            ui.action(action);
        });
    }
    let scroll = gtk::ScrolledWindow::builder()
        .child(&actions)
        .max_content_height(440)
        .propagate_natural_height(true)
        .build();
    if backend == Backend::Dinit {
        for (text, persistent) in [
            ("Choose activation target…", true),
            ("Change runtime dependency…", false),
        ] {
            let button = gtk::Button::with_label(text);
            button.add_css_class("flat");
            actions.append(&button);
            let u = ui.clone();
            let p = pop.clone();
            button.connect_clicked(move |_| {
                p.popdown();
                u.relation_dialog(persistent);
            });
        }
    }
    pop.set_child(Some(&scroll));
    ui.advanced.set_popover(Some(&pop));
    let u = ui.clone();
    refresh.connect_clicked(move |_| u.refresh(false));
    let u = ui.clone();
    create.connect_clicked(move |_| u.definition_dialog());
    let u = ui.clone();
    ui.unlock.connect_clicked(move |_| u.refresh(true));
    let u = ui.clone();
    ui.search.connect_search_changed(move |_| u.apply_filter());
    let u = ui.clone();
    ui.filter.connect_selected_notify(move |_| u.apply_filter());
    let u = ui.clone();
    ui.list
        .connect_row_selected(move |_, row| u.selection(row.map(|r| r.widget_name().to_string())));
    let u = ui.clone();
    ui.primary.connect_clicked(move |b| {
        u.action(if b.label().as_deref() == Some("Stop") {
            Action::Stop
        } else {
            Action::Start
        })
    });
    let u = ui.clone();
    ui.restart
        .connect_clicked(move |_| u.action(Action::Restart));
    let u = ui.clone();
    ui.log_button.connect_clicked(move |_| u.read_logs());
    let u = ui.clone();
    let weak = ui.window.downgrade();
    glib::timeout_add_local(Duration::from_secs(6), move || {
        if weak.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        if u.pending.borrow().is_empty() {
            u.refresh(false);
        }
        glib::ControlFlow::Continue
    });
    let u = ui.clone();
    let weak = ui.window.downgrade();
    glib::timeout_add_local(Duration::from_secs(1), move || {
        if weak.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        if u.pending.borrow().is_empty() && !u.refresh_busy.get() {
            if u.invalidated.replace(false) {
                u.refresh(false);
            } else if !u.event_busy.replace(true) {
                let backend = u.backend;
                u.job(
                    move || backend.event_revision(),
                    |ui, result| {
                        ui.event_busy.set(false);
                        if let Ok(Some(revision)) = result {
                            if ui
                                .revision
                                .replace(Some(revision))
                                .is_none_or(|old| old != revision)
                            {
                                ui.invalidated.set(true);
                            }
                        }
                    },
                );
            }
        }
        glib::ControlFlow::Continue
    });
    ui.window.present();
    ui.refresh(false);
}
fn main() -> glib::ExitCode {
    use std::os::unix::fs::MetadataExt;
    if std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0) {
        eprintln!("Pilot must run as a regular desktop user; system actions use its helper");
        return glib::ExitCode::FAILURE;
    }
    renderer::configure_at_startup();
    let mut args = std::env::args().skip(1);
    let mut profile = SystemManagerProfile::detect();
    if let Some(arg) = args.next() {
        if arg == "--help" {
            println!("Pilot — system service manager\n  --dev-backend dinit|runit   development override");
            return glib::ExitCode::SUCCESS;
        }
        if arg != "--dev-backend" {
            eprintln!("Unknown option: {arg}; use --help");
            return glib::ExitCode::FAILURE;
        }
        match args.next().and_then(|s| s.parse().ok()) {
            Some(backend) => {
                profile = SystemManagerProfile::probe(std::path::Path::new("/proc"), Some(backend))
            }
            None => {
                eprintln!("Expected dinit or runit");
                return glib::ExitCode::FAILURE;
            }
        }
        if args.next().is_some() {
            eprintln!("Unexpected extra arguments");
            return glib::ExitCode::FAILURE;
        }
    }
    let app = gtk::Application::builder()
        .application_id("org.voidlab.Pilot")
        .build();
    app.connect_activate(move |app| build(app, &profile));
    app.connect_shutdown(|_| pilot::shutdown_helper());
    app.run_with_args(&["pilot"])
}
