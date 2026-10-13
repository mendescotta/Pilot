use gtk::{glib, prelude::*};
use pilot::{Backend, Scope, Service};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Ui {
    backend: Backend,
    window: gtk::ApplicationWindow,
    body: gtk::Box,
    scope: gtk::DropDown,
    search: gtk::SearchEntry,
    list: gtk::ListBox,
    title: gtk::Label,
    subtitle: gtk::Label,
    detail: gtk::TextBuffer,
    message: gtk::Label,
    controls: gtk::Box,
    delete: gtk::Button,
    services: Rc<RefCell<Vec<Service>>>,
    rows: Rc<RefCell<Vec<(Service, gtk::ListBoxRow)>>>,
}

impl Ui {
    fn scope(&self) -> Scope {
        if self.scope.selected() == 0 {
            Scope::User
        } else {
            Scope::System
        }
    }
    fn selected(&self) -> Option<Service> {
        let selected_row = self.list.selected_row()?;
        self.rows
            .borrow()
            .iter()
            .find(|(_, row)| *row == selected_row)
            .map(|(service, _)| service.clone())
    }
    fn job<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, String> + Send + 'static,
        done: impl FnOnce(&Ui, T) + 'static,
    ) {
        self.body.set_sensitive(false);
        self.message.set_text("Working…");
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(work());
        });
        let ui = self.clone();
        let mut done = Some(done);
        glib::timeout_add_local(Duration::from_millis(50), move || {
            match receiver.try_recv() {
                Ok(result) => {
                    ui.body.set_sensitive(true);
                    match result {
                        Ok(text) => {
                            ui.message.set_text("");
                            ui.message.remove_css_class("error");
                            done.take().unwrap()(&ui, text);
                        }
                        Err(error) => {
                            ui.message.set_text(&error);
                            ui.message.add_css_class("error");
                        }
                    }
                    glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(_) => {
                    ui.body.set_sensitive(true);
                    ui.message.set_text("Worker disconnected");
                    glib::ControlFlow::Break
                }
            }
        });
    }
    /// Rebuilds row widgets from `services` (call when the underlying data
    /// changes). Row visibility is then handed to `filter`, so a search
    /// keystroke never has to tear down and recreate the list.
    fn rebuild(&self) {
        let previous = self.selected().map(|s| s.name);
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let mut rows = Vec::new();
        for service in self.services.borrow().iter().cloned() {
            let row = gtk::ListBoxRow::new();
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            content.set_margin_top(12);
            content.set_margin_bottom(12);
            content.set_margin_start(12);
            content.set_margin_end(12);
            let name = gtk::Label::new(Some(&service.name));
            name.set_xalign(0.0);
            name.set_hexpand(true);
            let state = gtk::Label::new(Some(&service.state));
            state.add_css_class(match service.state.as_str() {
                "Failed" => "error",
                "Unknown" => "warning",
                _ => "dim-label",
            });
            content.append(&name);
            content.append(&state);
            row.set_child(Some(&content));
            row.set_tooltip_text(Some(&service.summary));
            self.list.append(&row);
            rows.push((service, row));
        }
        *self.rows.borrow_mut() = rows;
        self.filter();
        self.controls.set_sensitive(false);
        self.title.set_text("Select a service");
        self.subtitle.set_text("");
        self.detail.set_text(if self.backend == Backend::Dinit {
            "Choose a service to inspect its status or buffered log.\nEnable and Disable change persistent dependencies; they do not imply a running or stopped state."
        } else {
            "Choose a service to inspect its status. Enable links an available service into the supervised directory; Disable removes that link."
        });
        if let Some(name) = previous {
            let row = self
                .rows
                .borrow()
                .iter()
                .find(|(s, _)| s.name == name)
                .map(|(_, row)| row.clone());
            if let Some(row) = row {
                self.list.select_row(Some(&row));
            }
        }
    }
    /// Shows or hides existing rows per the search query; never rebuilds them.
    fn filter(&self) {
        let query = self.search.text().to_lowercase();
        for (service, row) in self.rows.borrow().iter() {
            let matches = service.name.to_lowercase().contains(&query)
                || service.state.to_lowercase().contains(&query);
            row.set_visible(matches);
        }
    }
    fn refresh(&self) {
        let scope = self.scope();
        let backend = self.backend;
        self.job(
            move || {
                if backend == Backend::Dinit {
                    let listing = pilot::request(scope, "list", None);
                    let services = pilot::discover(
                        scope,
                        listing.as_ref().map(String::as_str).map_err(String::as_str),
                    );
                    match listing {
                        Ok(_) => Ok((services, None)),
                        Err(error) if !services.is_empty() => Ok((services, Some(error))),
                        Err(error) => Err(error),
                    }
                } else {
                    backend.list(scope).map(|services| (services, None))
                }
            },
            move |ui, (services, warning)| {
                *ui.services.borrow_mut() = services;
                ui.rebuild();
                if let Some(warning) = warning {
                    ui.message.set_text(&format!(
                        "Showing available definitions; service manager unavailable: {warning}"
                    ));
                    ui.message.add_css_class("error");
                } else {
                    ui.message.set_text(&format!(
                        "{} services · {} manager",
                        ui.services.borrow().len(),
                        if scope == Scope::User {
                            "User"
                        } else {
                            "System"
                        }
                    ));
                }
            },
        );
    }
    fn inspect(&self, action: &'static str) {
        let Some(service) = self.selected() else {
            return;
        };
        let scope = self.scope();
        let backend = self.backend;
        self.job(
            move || backend.request(scope, action, Some(&service.name)),
            |ui, text| {
                ui.detail.set_text(if text.is_empty() {
                    "No buffered output. Logs require log-type = buffer in the service definition."
                } else {
                    &text
                });
            },
        );
    }
    fn execute(&self, action: &'static str, name: String) {
        let scope = self.scope();
        let backend = self.backend;
        self.job(
            move || {
                let text = backend.request(scope, action, Some(&name))?;
                // start/stop/restart use --no-wait; give the transition a
                // moment to settle so the refresh below shows the final
                // state instead of "Starting"/"Stopping".
                if backend == Backend::Dinit && matches!(action, "start" | "stop" | "restart") {
                    wait_for_settled(backend, scope, &name);
                }
                Ok(text)
            },
            |ui, text| {
                ui.detail.set_text(&text);
                ui.refresh();
            },
        );
    }
    fn action(&self, action: &'static str) {
        let Some(service) = self.selected() else {
            return;
        };
        if matches!(action, "stop" | "restart" | "disable") {
            let explanation = if action == "disable" {
                if self.backend == Backend::Dinit {
                    "This removes the persistent dependency selected by dinit (enable-via or boot). Other dependencies can still start this service."
                } else {
                    "This removes the service link from the active runit directory. Runit will stop supervising it."
                }
            } else {
                if self.backend == Backend::Dinit {
                    "This changes the running service. Dinit will refuse operations that require forcing dependent services to stop."
                } else {
                    "This changes the running runit service."
                }
            };
            let dialog = gtk::MessageDialog::builder()
                .transient_for(&self.window)
                .modal(true)
                .message_type(gtk::MessageType::Question)
                .buttons(gtk::ButtonsType::OkCancel)
                .text(format!("{} {}?", action, service.name))
                .secondary_text(explanation)
                .build();
            let ui = self.clone();
            dialog.connect_response(move |dialog, response| {
                dialog.close();
                if response == gtk::ResponseType::Ok {
                    ui.execute(action, service.name.clone());
                }
            });
            dialog.present();
        } else {
            self.execute(action, service.name);
        }
    }
    /// User scope only: confirms removal of a stopped, disabled service.
    fn delete_selected(&self) {
        let Some(service) = self.selected() else {
            return;
        };
        let backend = self.backend;
        let path = backend
            .user_definition_path(&service.name)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(no definition file found)".into());
        let dialog = gtk::MessageDialog::builder()
            .transient_for(&self.window)
            .modal(true)
            .message_type(gtk::MessageType::Question)
            .buttons(gtk::ButtonsType::OkCancel)
            .text(format!("Delete {}?", service.name))
            .secondary_text(if backend == Backend::Dinit {
                format!(
                    "This will run `dinitctl unload {}`, then delete {path}.",
                    service.name
                )
            } else {
                format!("This will delete the disabled user service at {path}.")
            })
            .build();
        let ui = self.clone();
        dialog.connect_response(move |dialog, response| {
            dialog.close();
            if response == gtk::ResponseType::Ok {
                let name = service.name.clone();
                ui.job(
                    move || {
                        if backend == Backend::Dinit {
                            backend.request(Scope::User, "unload", Some(&name))?;
                        }
                        backend.delete_user_service(&name)?;
                        Ok(())
                    },
                    |ui, ()| {
                        ui.detail.set_text("Deleted.");
                        ui.refresh();
                    },
                );
            }
        });
        dialog.present();
    }
    /// User scope only: the exact service file is previewed before writing.
    fn show_add_dialog(&self) {
        if self.scope() != Scope::User {
            return;
        }
        let window = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Add service")
            .default_width(480)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_start(18);
        root.set_margin_end(18);
        root.set_margin_top(18);
        root.set_margin_bottom(18);

        let name = gtk::Entry::new();
        name.set_placeholder_text(Some("worker"));
        let command = gtk::Entry::new();
        command.set_placeholder_text(Some("/usr/bin/your-program --flag"));
        let enable_via = gtk::Entry::new();
        enable_via.set_text("boot");
        let buffered = gtk::CheckButton::with_label("Buffered log (viewable in Pilot)");
        buffered.set_active(true);
        let start_now = gtk::CheckButton::with_label(if self.backend == Backend::Dinit {
            "Start now"
        } else {
            "Start now (also enables)"
        });
        let enable_now = gtk::CheckButton::with_label("Enable (start on boot)");

        for (label, widget) in [
            ("Name", name.clone().upcast::<gtk::Widget>()),
            ("Command", command.clone().upcast()),
            ("Enable via", enable_via.clone().upcast()),
        ] {
            if self.backend == Backend::Runit && label == "Enable via" {
                continue;
            }
            let field = gtk::Box::new(gtk::Orientation::Vertical, 4);
            let field_label = gtk::Label::new(Some(label));
            field_label.set_xalign(0.0);
            field_label.add_css_class("dim-label");
            field.append(&field_label);
            field.append(&widget);
            root.append(&field);
        }
        if self.backend == Backend::Dinit {
            root.append(&buffered);
        }
        root.append(&start_now);
        root.append(&enable_now);

        let preview_label = gtk::Label::new(Some("Preview"));
        preview_label.set_xalign(0.0);
        preview_label.add_css_class("dim-label");
        root.append(&preview_label);
        let preview_view = gtk::TextView::new();
        preview_view.set_editable(false);
        preview_view.set_monospace(true);
        preview_view.set_wrap_mode(gtk::WrapMode::WordChar);
        let preview_buffer = preview_view.buffer();
        let preview_scroll = gtk::ScrolledWindow::builder()
            .child(&preview_view)
            .min_content_height(100)
            .build();
        root.append(&preview_scroll);

        let error_label = gtk::Label::new(None);
        error_label.set_xalign(0.0);
        error_label.set_wrap(true);
        error_label.add_css_class("error");
        root.append(&error_label);

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        let create = gtk::Button::with_label("Create");
        create.add_css_class("suggested-action");
        create.set_sensitive(false);
        buttons.append(&cancel);
        buttons.append(&create);
        root.append(&buttons);
        window.set_child(Some(&root));

        let refresh_preview: Rc<dyn Fn()> = {
            let name = name.clone();
            let command = command.clone();
            let enable_via = enable_via.clone();
            let buffered = buffered.clone();
            let preview_buffer = preview_buffer.clone();
            let error_label = error_label.clone();
            let create = create.clone();
            let services = self.services.clone();
            let backend = self.backend;
            Rc::new(move || {
                let name_text = name.text().to_string();
                let result: Result<String, String> = (|| {
                    if name_text.is_empty() {
                        return Err("Enter a name".to_string());
                    }
                    pilot::validate("status", Some(name_text.as_str()))?;
                    if services.borrow().iter().any(|s| s.name == name_text) {
                        return Err(format!("A service named \"{name_text}\" already exists"));
                    }
                    backend.render_user_service(
                        &command.text(),
                        buffered.is_active(),
                        &enable_via.text(),
                    )
                })();
                match result {
                    Ok(text) => {
                        preview_buffer.set_text(&text);
                        error_label.set_text("");
                        create.set_sensitive(true);
                    }
                    Err(message) => {
                        preview_buffer.set_text("");
                        error_label.set_text(&message);
                        create.set_sensitive(false);
                    }
                }
            })
        };
        for signal in [&name, &command, &enable_via] {
            let f = refresh_preview.clone();
            signal.connect_changed(move |_| f());
        }
        {
            let f = refresh_preview.clone();
            buffered.connect_toggled(move |_| f());
        }
        refresh_preview();

        {
            let window = window.clone();
            cancel.connect_clicked(move |_| window.close());
        }
        {
            let ui = self.clone();
            let window = window.clone();
            create.connect_clicked(move |_| {
                let name_text = name.text().to_string();
                let contents = match ui.backend.render_user_service(
                    &command.text(),
                    buffered.is_active(),
                    &enable_via.text(),
                ) {
                    Ok(text) => text,
                    Err(e) => {
                        error_label.set_text(&e);
                        return;
                    }
                };
                let existing = ui.services.borrow().clone();
                match ui
                    .backend
                    .create_user_service(&name_text, &contents, &existing)
                {
                    Ok(_) => {
                        window.close();
                        let start = start_now.is_active();
                        let enable =
                            enable_now.is_active() || (ui.backend == Backend::Runit && start);
                        let backend = ui.backend;
                        ui.job(
                            move || {
                                if enable {
                                    backend.request(Scope::User, "enable", Some(&name_text))?;
                                }
                                if start {
                                    backend.request(Scope::User, "start", Some(&name_text))?;
                                }
                                Ok(())
                            },
                            |ui, ()| ui.refresh(),
                        );
                    }
                    Err(e) => error_label.set_text(&e),
                }
            });
        }
        window.present();
    }
}

/// Polls `list` for up to 5s until `name` is no longer Starting/Stopping (or
/// the query itself fails, in which case there's nothing left to wait on).
fn wait_for_settled(backend: Backend, scope: Scope, name: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let Ok(services) = backend.list(scope) else {
            return;
        };
        let settled = match services.iter().find(|s| s.name == name) {
            Some(s) => !matches!(s.state.as_str(), "Starting" | "Stopping"),
            None => true,
        };
        if settled {
            return;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn build(app: &gtk::Application, backend: Backend) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(format!("Pilot — {} Services", backend.label()))
        .default_width(1000)
        .default_height(680)
        .build();
    let header = gtk::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some(&format!(
        "{} Services",
        backend.label()
    )))));
    window.set_titlebar(Some(&header));
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let scope = gtk::DropDown::from_strings(&["User services", "System services"]);
    if backend == Backend::Runit {
        scope.set_selected(1);
    }
    let search = gtk::SearchEntry::new();
    search.set_hexpand(true);
    search.set_placeholder_text(Some("Search services or states"));
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.set_tooltip_text(Some("Refresh services"));
    let add = gtk::Button::with_label("Add…");
    add.set_tooltip_text(Some("Add a user service"));
    toolbar.append(&scope);
    toolbar.append(&search);
    toolbar.append(&refresh);
    toolbar.append(&add);
    body.append(&toolbar);
    let pane = gtk::Paned::new(gtk::Orientation::Horizontal);
    pane.set_vexpand(true);
    pane.set_position(360);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .min_content_width(280)
        .build();
    pane.set_start_child(Some(&scroll));
    let right = gtk::Box::new(gtk::Orientation::Vertical, 12);
    right.set_margin_start(18);
    let title = gtk::Label::new(Some("Select a service"));
    title.set_xalign(0.0);
    title.add_css_class("title-2");
    right.append(&title);
    let subtitle = gtk::Label::new(None);
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("dim-label");
    right.append(&subtitle);
    let controls = gtk::Box::new(gtk::Orientation::Vertical, 8);
    for actions in [
        [("Start", "start"), ("Stop", "stop"), ("Restart", "restart")],
        [
            ("Enable", "enable"),
            ("Disable", "disable"),
            ("Reload", "reload"),
        ],
    ] {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for (label, action) in actions {
            let button =
                gtk::Button::with_label(if backend == Backend::Runit && action == "reload" {
                    "Send HUP"
                } else {
                    label
                });
            button.set_widget_name(action);
            row.append(&button);
        }
        controls.append(&row);
    }
    let info = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let status = gtk::Button::with_label("Status");
    let logs = gtk::Button::with_label("Buffered log");
    logs.set_visible(backend == Backend::Dinit);
    let unpin = gtk::Button::with_label("Unpin");
    unpin.set_visible(backend == Backend::Dinit);
    unpin.set_widget_name("unpin");
    let delete = gtk::Button::with_label("Delete");
    delete.add_css_class("destructive-action");
    info.append(&status);
    info.append(&logs);
    info.append(&unpin);
    info.append(&delete);
    controls.append(&info);
    controls.set_sensitive(false);
    right.append(&controls);
    let view = gtk::TextView::new();
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    let detail = view.buffer();
    detail.set_text("Select a service to inspect it.");
    let detail_scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .build();
    right.append(&detail_scroll);
    pane.set_end_child(Some(&right));
    body.append(&pane);
    root.append(&body);
    let message = gtk::Label::new(None);
    message.set_xalign(0.0);
    message.set_wrap(true);
    message.set_selectable(true);
    root.append(&message);
    window.set_child(Some(&root));
    let ui = Ui {
        backend,
        window,
        body,
        scope,
        search,
        list,
        title,
        subtitle,
        detail,
        message,
        controls,
        delete: delete.clone(),
        services: Default::default(),
        rows: Default::default(),
    };
    let clone = ui.clone();
    refresh.connect_clicked(move |_| clone.refresh());
    let clone = ui.clone();
    add.connect_clicked(move |_| clone.show_add_dialog());
    {
        let add = add.clone();
        add.set_sensitive(ui.scope() == Scope::User);
        let clone = ui.clone();
        ui.scope.connect_selected_notify(move |_| {
            clone.services.borrow_mut().clear();
            clone.rebuild();
            clone.refresh();
            add.set_sensitive(clone.scope() == Scope::User);
        });
    }
    let clone = ui.clone();
    ui.search.connect_search_changed(move |_| clone.filter());
    let clone = ui.clone();
    ui.list.connect_row_selected(move |_, row| {
        clone.controls.set_sensitive(row.is_some());
        if let Some(service) = clone.selected() {
            clone
                .title
                .set_text(&format!("{} · {}", service.name, service.state));
            clone.subtitle.set_text(
                &clone
                    .backend
                    .enabled(clone.scope(), &service.name)
                    .unwrap_or_default(),
            );
            let deletable = clone.scope() == Scope::User
                && if clone.backend == Backend::Runit {
                    service.state == "Disabled"
                } else {
                    !matches!(service.state.as_str(), "Started" | "Starting" | "Stopping")
                };
            clone.delete.set_sensitive(deletable);
            clone.inspect("status");
        } else {
            clone.subtitle.set_text("");
            clone.delete.set_sensitive(false);
        }
    });
    let clone = ui.clone();
    status.connect_clicked(move |_| clone.inspect("status"));
    let clone = ui.clone();
    logs.connect_clicked(move |_| clone.inspect("catlog"));
    let clone = ui.clone();
    delete.connect_clicked(move |_| clone.delete_selected());
    let mut row = ui.controls.first_child();
    while let Some(container) = row {
        let mut child = container.first_child();
        while let Some(widget) = child {
            if let Ok(button) = widget.clone().downcast::<gtk::Button>() {
                let action = button.widget_name();
                if let Some(&action) = pilot::ACTIONS.iter().find(|&&a| a == action) {
                    let clone = ui.clone();
                    button.connect_clicked(move |_| clone.action(action));
                }
            }
            child = widget.next_sibling();
        }
        row = container.next_sibling();
    }
    ui.window.present();
    ui.refresh();
}

fn main() -> glib::ExitCode {
    let backend = match Backend::detect() {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("pilot: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let app = gtk::Application::builder()
        .application_id("org.voidlab.Pilot")
        .build();
    app.connect_activate(move |app| build(app, backend));
    app.connect_shutdown(|_| pilot::shutdown_helper());
    app.run()
}
