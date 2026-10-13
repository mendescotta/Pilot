use gtk::{glib, prelude::*};
use pilot::{Backend, Scope, Service};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

/// A displayed list row, holding onto the one label whose text/style actually changes
/// (`state_label`) so `apply_service` can update it in place instead of rebuilding the row.
struct Row {
    service: Service,
    widget: gtk::ListBoxRow,
    state_label: gtk::Label,
}

#[derive(Clone)]
struct Ui {
    system_backend: Backend,
    user_backend: Backend,
    window: gtk::ApplicationWindow,
    header_title: gtk::Label,
    body: gtk::Box,
    scope: gtk::DropDown,
    search: gtk::SearchEntry,
    status_filter: gtk::DropDown,
    list: gtk::ListBox,
    title: gtk::Label,
    subtitle: gtk::Label,
    detail: gtk::TextBuffer,
    message: gtk::Label,
    controls: gtk::Box,
    console_actions: gtk::Box,
    signal_btn: gtk::Button,
    deps_btn: gtk::Button,
    enable_log_btn: gtk::Button,
    environment_btn: gtk::Button,
    logs: gtk::Button,
    unpin: gtk::Button,
    reload: gtk::Button,
    delete: gtk::Button,
    services: Rc<RefCell<Vec<Service>>>,
    rows: Rc<RefCell<Vec<Row>>>,
}

impl Ui {
    fn scope(&self) -> Scope {
        if self.scope.selected() == 0 {
            Scope::User
        } else {
            Scope::System
        }
    }
    /// The backend that applies to the *currently selected* scope — the two can differ (e.g. a
    /// runit PID 1 with a per-user dinit instance via turnstile).
    fn backend(&self) -> Backend {
        if self.scope() == Scope::User {
            self.user_backend
        } else {
            self.system_backend
        }
    }
    /// Re-applies backend-dependent widget state (visibility, labels, header) for the current
    /// scope's backend. Call on build and whenever the scope selector changes — the two scopes
    /// can now disagree on backend, so this can no longer be decided once at startup.
    fn apply_backend_dependent_ui(&self) {
        let dinit = self.backend() == Backend::Dinit;
        self.logs.set_visible(dinit);
        self.unpin.set_visible(dinit);
        self.signal_btn.set_visible(dinit);
        self.deps_btn.set_visible(dinit);
        self.enable_log_btn.set_visible(dinit);
        // Also user-scope only: setenv/unsetenv never reach the system dinit instance (see
        // lib.rs's setenv/unsetenv doc comments) — hidden here so the button never leads to a
        // guaranteed-refusal dialog rather than erroring only after Apply is clicked.
        self.environment_btn
            .set_sensitive(self.scope() == Scope::User);
        // Enabling buffered logs edits the definition file — user scope only, same reason as
        // Add/Delete and Environment (no privileged-helper write action exists for System).
        self.enable_log_btn
            .set_sensitive(self.scope() == Scope::User);
        self.reload
            .set_label(if dinit { "Reload" } else { "Send HUP" });
        let scope_label = if self.scope() == Scope::User {
            "User"
        } else {
            "System"
        };
        self.header_title.set_text(&format!(
            "{scope_label} Services — {}",
            self.backend().label()
        ));
    }
    /// The distinct `state` values currently present in the loaded list, in a stable display
    /// order, for the status filter dropdown.
    fn known_states(&self) -> Vec<String> {
        let mut seen = Vec::new();
        for service in self.services.borrow().iter() {
            if !seen.contains(&service.state) {
                seen.push(service.state.clone());
            }
        }
        seen
    }
    fn selected(&self) -> Option<Service> {
        let selected_row = self.list.selected_row()?;
        self.rows
            .borrow()
            .iter()
            .find(|row| row.widget == selected_row)
            .map(|row| row.service.clone())
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
    /// Like `job`, but for the silent periodic auto-refresh: no "Working…"/insensitive chrome
    /// on the whole window, since this should be invisible unless something is actually wrong —
    /// a failure still surfaces in the message bar, same as any other failure.
    fn job_quiet<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, String> + Send + 'static,
        done: impl FnOnce(&Ui, T) + 'static,
    ) {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(work());
        });
        let ui = self.clone();
        let mut done = Some(done);
        glib::timeout_add_local(Duration::from_millis(50), move || {
            match receiver.try_recv() {
                Ok(Ok(result)) => {
                    done.take().unwrap()(&ui, result);
                    glib::ControlFlow::Break
                }
                Ok(Err(error)) => {
                    ui.message.set_text(&error);
                    ui.message.add_css_class("error");
                    glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
                Err(_) => glib::ControlFlow::Break,
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
            let widget = gtk::ListBoxRow::new();
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            content.set_margin_top(12);
            content.set_margin_bottom(12);
            content.set_margin_start(12);
            content.set_margin_end(12);
            let name = gtk::Label::new(Some(&service.name));
            name.set_xalign(0.0);
            name.set_hexpand(true);
            let state_label = gtk::Label::new(Some(&service.state));
            state_label.add_css_class(match service.state.as_str() {
                "Failed" => "error",
                "Unknown" => "warning",
                _ => "dim-label",
            });
            content.append(&name);
            content.append(&state_label);
            widget.set_child(Some(&content));
            widget.set_tooltip_text(Some(&service.summary));
            self.list.append(&widget);
            rows.push(Row {
                service,
                widget,
                state_label,
            });
        }
        *self.rows.borrow_mut() = rows;
        self.apply_backend_dependent_ui();
        self.refresh_status_filter();
        self.filter();
        self.controls.set_sensitive(false);
        self.console_actions.set_sensitive(false);
        self.title.set_text("Select a service");
        self.subtitle.set_text("");
        self.detail.set_text(if self.backend() == Backend::Dinit {
            "Choose a service to inspect its status or buffered log.\nEnable and Disable change persistent dependencies; they do not imply a running or stopped state."
        } else {
            "Choose a service to inspect its status. Enable links an available service into the supervised directory; Disable removes that link."
        });
        if let Some(name) = previous {
            let widget = self
                .rows
                .borrow()
                .iter()
                .find(|row| row.service.name == name)
                .map(|row| row.widget.clone());
            if let Some(widget) = widget {
                self.list.select_row(Some(&widget));
            }
        }
    }
    /// Shows or hides existing rows per the search query and status filter; never rebuilds them.
    fn filter(&self) {
        let query = self.search.text().to_lowercase();
        let status = self.selected_status_filter();
        let status_filter_active = status.as_deref().is_some_and(|s| s != "All");
        for row in self.rows.borrow().iter() {
            let text_matches = row.service.name.to_lowercase().contains(&query)
                || row.service.state.to_lowercase().contains(&query);
            let status_matches =
                !status_filter_active || status.as_deref() == Some(row.service.state.as_str());
            row.widget.set_visible(text_matches && status_matches);
        }
    }
    fn selected_status_filter(&self) -> Option<String> {
        self.status_filter
            .selected_item()
            .and_then(|obj| obj.downcast::<gtk::StringObject>().ok())
            .map(|s| s.string().to_string())
    }
    /// Rebuilds the status filter's options from the states actually present in the current
    /// list (so it never offers a state the current backend/scope can't produce), keeping the
    /// current selection if it's still one of the options, else resetting to "All".
    fn refresh_status_filter(&self) {
        let previous = self.selected_status_filter();
        let mut values = vec!["All".to_string()];
        values.extend(self.known_states());
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        self.status_filter
            .set_model(Some(&gtk::StringList::new(&refs)));
        let index = previous
            .and_then(|prev| values.iter().position(|v| *v == prev))
            .unwrap_or(0);
        self.status_filter.set_selected(index as u32);
    }
    fn refresh(&self) {
        let scope = self.scope();
        let backend = self.backend();
        let work = move || {
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
        };
        let done = move |ui: &Ui, (services, warning): (Vec<Service>, Option<String>)| {
            *ui.services.borrow_mut() = services;
            ui.rebuild();
            if let Some(warning) = warning {
                ui.message.set_text(&format!(
                    "Showing available definitions; {}",
                    pilot::friendly_unavailable_reason(&warning)
                ));
                ui.message.add_css_class("error");
            } else {
                ui.message.set_text(&format!(
                    "{} services · {} manager ({})",
                    ui.services.borrow().len(),
                    if scope == Scope::User {
                        "User"
                    } else {
                        "System"
                    },
                    backend.label()
                ));
                ui.message.remove_css_class("error");
            }
        };
        self.job(work, done);
    }
    fn show_log(&self) {
        let Some(service) = self.selected() else {
            return;
        };
        let scope = self.scope();
        let backend = self.backend();
        self.job(
            move || backend.request(scope, "catlog", Some(&service.name)),
            |ui, text| {
                ui.detail.set_text(if text.is_empty() {
                    "No buffered output. Logs require log-type = buffer in the service definition."
                } else {
                    &text
                });
            },
        );
    }
    /// Refreshes only the selected service: its detailed status text for the console panel, and
    /// (separately, via `Backend::service` — see its doc comment for why this isn't derived from
    /// the same status text) its row's state/tooltip and the title badge. Replaces relying on a
    /// periodic full-list auto-refresh to keep the row everyone's actually looking at current.
    fn refresh_selected(&self) {
        let Some(service) = self.selected() else {
            return;
        };
        let scope = self.scope();
        let backend = self.backend();
        let name = service.name.clone();
        self.job_quiet(
            move || {
                let text = backend.request(scope, "status", Some(&name))?;
                let service = backend.service(scope, &name)?;
                Ok((text, service))
            },
            |ui, (text, service): (String, Service)| {
                ui.detail.set_text(&text);
                ui.apply_service(service);
            },
        );
    }
    /// Patches one row's state/tooltip and the title badge from a freshly fetched `Service`,
    /// without touching any other row — the "refresh only that service" counterpart to
    /// `rebuild()`'s whole-list rebuild. A no-op if the service is no longer in the list (e.g.
    /// deleted, or the scope changed under the in-flight request).
    fn apply_service(&self, service: Service) {
        let previous_states = self.known_states();
        if let Some(existing) = self
            .services
            .borrow_mut()
            .iter_mut()
            .find(|s| s.name == service.name)
        {
            *existing = service.clone();
        }
        {
            let mut rows = self.rows.borrow_mut();
            let Some(row) = rows.iter_mut().find(|r| r.service.name == service.name) else {
                return;
            };
            if row.service.state != service.state {
                row.state_label.set_text(&service.state);
                for class in ["error", "warning", "dim-label"] {
                    row.state_label.remove_css_class(class);
                }
                row.state_label.add_css_class(match service.state.as_str() {
                    "Failed" => "error",
                    "Unknown" => "warning",
                    _ => "dim-label",
                });
            }
            if row.service.summary != service.summary {
                row.widget.set_tooltip_text(Some(&service.summary));
            }
            row.service = service.clone();
        }
        if self.known_states() != previous_states {
            self.refresh_status_filter();
        }
        self.filter();
        if self.selected().is_some_and(|s| s.name == service.name) {
            self.title
                .set_text(&format!("{} · {}", service.name, service.state));
        }
    }
    fn execute(&self, action: &'static str, name: String) {
        let scope = self.scope();
        let backend = self.backend();
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
    /// A lightweight Cancel/Confirm window, built and fully populated before it's ever shown —
    /// unlike the legacy `gtk::MessageDialog` this replaced, there's no separate "set the text
    /// after the window exists" step that could show a blank window for a frame first.
    fn confirm(&self, title: &str, body: &str, on_confirm: impl Fn(&Ui) + 'static) {
        let window = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title(title)
            .default_width(380)
            .resizable(false)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_start(18);
        root.set_margin_end(18);
        root.set_margin_top(18);
        root.set_margin_bottom(18);
        let title_label = gtk::Label::new(Some(title));
        title_label.set_xalign(0.0);
        title_label.add_css_class("title-4");
        title_label.set_wrap(true);
        root.append(&title_label);
        let body_label = gtk::Label::new(Some(body));
        body_label.set_xalign(0.0);
        body_label.set_wrap(true);
        body_label.add_css_class("dim-label");
        root.append(&body_label);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        let confirm = gtk::Button::with_label("Confirm");
        confirm.add_css_class("destructive-action");
        buttons.append(&cancel);
        buttons.append(&confirm);
        root.append(&buttons);
        window.set_child(Some(&root));
        {
            let window = window.clone();
            cancel.connect_clicked(move |_| window.close());
        }
        {
            let window = window.clone();
            let ui = self.clone();
            confirm.connect_clicked(move |_| {
                window.close();
                on_confirm(&ui);
            });
        }
        window.present();
    }
    fn action(&self, action: &'static str) {
        let Some(service) = self.selected() else {
            return;
        };
        if matches!(action, "stop" | "restart" | "disable") {
            let explanation = if action == "disable" {
                if self.backend() == Backend::Dinit {
                    "This removes the persistent dependency selected by dinit (enable-via or boot). Other dependencies can still start this service."
                } else {
                    "This removes the service link from the active runit directory. Runit will stop supervising it."
                }
            } else if self.backend() == Backend::Dinit {
                "This changes the running service. Dinit will refuse operations that require forcing dependent services to stop."
            } else {
                "This changes the running runit service."
            };
            let name = service.name.clone();
            self.confirm(
                &format!("{action} {}?", service.name),
                explanation,
                move |ui| ui.execute(action, name.clone()),
            );
        } else {
            self.execute(action, service.name);
        }
    }
    /// User scope only: confirms removal of a stopped, disabled service.
    fn delete_selected(&self) {
        let Some(service) = self.selected() else {
            return;
        };
        let backend = self.backend();
        let path = backend
            .user_definition_path(&service.name)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(no definition file found)".into());
        let body = if backend == Backend::Dinit {
            format!(
                "This will run `dinitctl unload {}`, then delete {path}.",
                service.name
            )
        } else {
            format!("This will delete the disabled user service at {path}.")
        };
        let name = service.name.clone();
        self.confirm(&format!("Delete {}?", service.name), &body, move |ui| {
            let name = name.clone();
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
        });
    }
    /// User scope only: edits the selected service's definition to turn on buffered logging,
    /// then reloads it. Button visibility/sensitivity already restrict this to Dinit + User
    /// scope; `pilot::enable_buffered_log` hardcodes `Scope::User` internally too either way.
    fn enable_buffered_log(&self) {
        let Some(service) = self.selected() else {
            return;
        };
        let name = service.name;
        self.job(
            move || pilot::enable_buffered_log(&name),
            |ui, text| {
                ui.detail.set_text(&text);
                ui.refresh();
            },
        );
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
        let start_now = gtk::CheckButton::with_label(if self.backend() == Backend::Dinit {
            "Start now"
        } else {
            "Start now (also enables)"
        });
        let enable_now = gtk::CheckButton::with_label("Enable (start on boot)");
        let dont_autostart = gtk::CheckButton::with_label("Don't start automatically (down file)");

        for (label, widget) in [
            ("Name", name.clone().upcast::<gtk::Widget>()),
            ("Command", command.clone().upcast()),
            ("Enable via", enable_via.clone().upcast()),
        ] {
            if self.backend() == Backend::Runit && label == "Enable via" {
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
        if self.backend() == Backend::Dinit {
            root.append(&buffered);
        }
        root.append(&start_now);
        root.append(&enable_now);
        if self.backend() == Backend::Runit {
            root.append(&dont_autostart);
        }

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
            let backend = self.backend();
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
                let contents = match ui.backend().render_user_service(
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
                    .backend()
                    .create_user_service(&name_text, &contents, &existing)
                {
                    Ok(_) => {
                        window.close();
                        let start = start_now.is_active();
                        let enable =
                            enable_now.is_active() || (ui.backend() == Backend::Runit && start);
                        let down = dont_autostart.is_active();
                        let backend = ui.backend();
                        ui.job(
                            move || {
                                backend.set_down(&name_text, down)?;
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
    /// Dinit only: sends an arbitrary signal to the selected service's process.
    fn show_signal_dialog(&self) {
        let Some(service) = self.selected() else {
            return;
        };
        let scope = self.scope();
        let window = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Send signal")
            .default_width(360)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_start(18);
        root.set_margin_end(18);
        root.set_margin_top(18);
        root.set_margin_bottom(18);

        let label = gtk::Label::new(Some(&format!("Signal for {}", service.name)));
        label.set_xalign(0.0);
        root.append(&label);
        let entry = gtk::Entry::new();
        entry.set_placeholder_text(Some(
            "TERM, HUP, USR1, USR2, INT, KILL, CONT, STOP, or a number",
        ));
        root.append(&entry);
        let error_label = gtk::Label::new(None);
        error_label.set_xalign(0.0);
        error_label.set_wrap(true);
        error_label.add_css_class("error");
        root.append(&error_label);

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        let send = gtk::Button::with_label("Send");
        send.add_css_class("suggested-action");
        buttons.append(&cancel);
        buttons.append(&send);
        root.append(&buttons);
        window.set_child(Some(&root));

        {
            let window = window.clone();
            cancel.connect_clicked(move |_| window.close());
        }
        {
            let window = window.clone();
            let ui = self.clone();
            let name = service.name.clone();
            send.connect_clicked(move |_| {
                let signal_id = entry.text().to_string();
                if signal_id.trim().is_empty() {
                    error_label.set_text("Enter a signal");
                    return;
                }
                window.close();
                let name = name.clone();
                ui.job(
                    move || pilot::signal(scope, &signal_id, &name),
                    |ui, text| {
                        ui.detail
                            .set_text(if text.is_empty() { "Done." } else { &text });
                        ui.refresh();
                    },
                );
            });
        }
        window.present();
    }
    /// Dinit only: ad hoc `add-dep`/`rm-dep` with the selected service as "from", separate from
    /// the persistent `enable-via` mechanism.
    fn show_dependencies_dialog(&self) {
        let Some(service) = self.selected() else {
            return;
        };
        let scope = self.scope();
        let window = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Dependencies")
            .default_width(400)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_start(18);
        root.set_margin_end(18);
        root.set_margin_top(18);
        root.set_margin_bottom(18);

        let from_label = gtk::Label::new(Some(&format!("From: {}", service.name)));
        from_label.set_xalign(0.0);
        root.append(&from_label);

        // Read-only: dinitctl has no query for either direction, only add-dep/rm-dep to modify,
        // so both are read straight from the definition files on disk (same approach already
        // used for enable-via). Dependents is a real scan of every file in the search path, so
        // this only runs when the dialog opens or a change is made here, not continuously.
        let depends_on_label = gtk::Label::new(None);
        depends_on_label.set_xalign(0.0);
        depends_on_label.set_wrap(true);
        root.append(&depends_on_label);
        let depended_on_by_label = gtk::Label::new(None);
        depended_on_by_label.set_xalign(0.0);
        depended_on_by_label.set_wrap(true);
        root.append(&depended_on_by_label);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let refresh_relationships: Rc<dyn Fn()> = {
            let depends_on_label = depends_on_label.clone();
            let depended_on_by_label = depended_on_by_label.clone();
            let name = service.name.clone();
            Rc::new(move || {
                depends_on_label.set_text(&format!(
                    "Depends on: {}",
                    format_dependencies(&pilot::dependencies(scope, &name))
                ));
                depended_on_by_label.set_text(&format!(
                    "Depended on by: {}",
                    format_dependencies(&pilot::dependents(scope, &name))
                ));
            })
        };
        refresh_relationships();

        let type_label = gtk::Label::new(Some("Dependency type"));
        type_label.set_xalign(0.0);
        type_label.add_css_class("dim-label");
        root.append(&type_label);
        let dep_type = gtk::DropDown::from_strings(pilot::DEP_TYPES);
        root.append(&dep_type);
        let to_label = gtk::Label::new(Some("To (service name)"));
        to_label.set_xalign(0.0);
        to_label.add_css_class("dim-label");
        root.append(&to_label);
        let to_entry = gtk::Entry::new();
        to_entry.set_placeholder_text(Some("service name"));
        root.append(&to_entry);
        let error_label = gtk::Label::new(None);
        error_label.set_xalign(0.0);
        error_label.set_wrap(true);
        error_label.add_css_class("error");
        root.append(&error_label);

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        let close = gtk::Button::with_label("Close");
        let remove = gtk::Button::with_label("Remove");
        remove.add_css_class("destructive-action");
        let add = gtk::Button::with_label("Add");
        add.add_css_class("suggested-action");
        buttons.append(&close);
        buttons.append(&remove);
        buttons.append(&add);
        root.append(&buttons);
        window.set_child(Some(&root));

        {
            let window = window.clone();
            close.connect_clicked(move |_| window.close());
        }
        let from = service.name.clone();
        type Apply = fn(Scope, &str, &str, &str) -> Result<String, String>;
        for (button, apply) in [
            (&add, pilot::add_dep as Apply),
            (&remove, pilot::rm_dep as Apply),
        ] {
            let ui = self.clone();
            let from = from.clone();
            let to_entry = to_entry.clone();
            let dep_type = dep_type.clone();
            let error_label = error_label.clone();
            let refresh_relationships = refresh_relationships.clone();
            button.connect_clicked(move |_| {
                let to = to_entry.text().to_string();
                if to.trim().is_empty() {
                    error_label.set_text("Enter a service name");
                    return;
                }
                error_label.set_text("");
                let dep_type_name = pilot::DEP_TYPES[dep_type.selected() as usize];
                let from = from.clone();
                let refresh_relationships = refresh_relationships.clone();
                ui.job(
                    move || apply(scope, dep_type_name, &from, &to),
                    move |ui, text| {
                        ui.detail
                            .set_text(if text.is_empty() { "Done." } else { &text });
                        ui.refresh();
                        refresh_relationships();
                    },
                );
            });
        }
        window.present();
    }
    /// Dinit only, not tied to a selected service: sets/unsets activation-environment variables
    /// (affects services started or restarted after the call, not ones already running).
    fn show_environment_dialog(&self) {
        let scope = self.scope();
        let window = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("Environment")
            .default_width(420)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_start(18);
        root.set_margin_end(18);
        root.set_margin_top(18);
        root.set_margin_bottom(18);

        let set_label = gtk::Label::new(Some("Set (one NAME or NAME=value per line)"));
        set_label.set_xalign(0.0);
        set_label.add_css_class("dim-label");
        root.append(&set_label);
        let set_view = gtk::TextView::new();
        set_view.set_monospace(true);
        let set_buffer = set_view.buffer();
        let set_scroll = gtk::ScrolledWindow::builder()
            .child(&set_view)
            .min_content_height(80)
            .build();
        root.append(&set_scroll);

        let unset_label = gtk::Label::new(Some("Unset (one NAME per line)"));
        unset_label.set_xalign(0.0);
        unset_label.add_css_class("dim-label");
        root.append(&unset_label);
        let unset_view = gtk::TextView::new();
        unset_view.set_monospace(true);
        let unset_buffer = unset_view.buffer();
        let unset_scroll = gtk::ScrolledWindow::builder()
            .child(&unset_view)
            .min_content_height(60)
            .build();
        root.append(&unset_scroll);

        let error_label = gtk::Label::new(None);
        error_label.set_xalign(0.0);
        error_label.set_wrap(true);
        error_label.add_css_class("error");
        root.append(&error_label);

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        let apply = gtk::Button::with_label("Apply");
        apply.add_css_class("suggested-action");
        buttons.append(&cancel);
        buttons.append(&apply);
        root.append(&buttons);
        window.set_child(Some(&root));

        {
            let window = window.clone();
            cancel.connect_clicked(move |_| window.close());
        }
        {
            let window = window.clone();
            let ui = self.clone();
            apply.connect_clicked(move |_| {
                let set_lines: Vec<String> = set_buffer
                    .text(&set_buffer.start_iter(), &set_buffer.end_iter(), false)
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(String::from)
                    .collect();
                let unset_lines: Vec<String> = unset_buffer
                    .text(&unset_buffer.start_iter(), &unset_buffer.end_iter(), false)
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(String::from)
                    .collect();
                if set_lines.is_empty() && unset_lines.is_empty() {
                    error_label.set_text("Enter at least one variable to set or unset");
                    return;
                }
                error_label.set_text("");
                window.close();
                ui.job(
                    move || {
                        let mut applied = Vec::new();
                        if !set_lines.is_empty() {
                            applied.push(pilot::setenv(scope, &set_lines)?);
                        }
                        if !unset_lines.is_empty() {
                            applied.push(pilot::unsetenv(scope, &unset_lines)?);
                        }
                        Ok(applied.join("\n"))
                    },
                    |ui, text| {
                        ui.detail
                            .set_text(if text.is_empty() { "Done." } else { &text });
                    },
                );
            });
        }
        window.present();
    }
}

fn format_dependencies(deps: &[pilot::Dependency]) -> String {
    if deps.is_empty() {
        "(none)".to_string()
    } else {
        deps.iter()
            .map(|d| format!("{} ({})", d.name, d.kind))
            .collect::<Vec<_>>()
            .join(", ")
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

fn build(app: &gtk::Application, system_backend: Backend, user_backend: Backend) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Pilot")
        .icon_name("org.voidlab.Pilot")
        .default_width(1000)
        .default_height(680)
        .build();
    let header = gtk::HeaderBar::new();
    let header_title = gtk::Label::new(None);
    header.set_title_widget(Some(&header_title));
    window.set_titlebar(Some(&header));
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let scope = gtk::DropDown::from_strings(&["User services", "System services"]);
    if system_backend == Backend::Runit {
        scope.set_selected(1);
    }
    let search = gtk::SearchEntry::new();
    search.set_hexpand(true);
    search.set_placeholder_text(Some("Search services or states"));
    let status_filter = gtk::DropDown::from_strings(&["All"]);
    status_filter.set_tooltip_text(Some("Filter by status"));
    let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh.set_tooltip_text(Some("Refresh services"));
    let environment_btn = gtk::Button::with_label("Environment…");
    environment_btn.set_tooltip_text(Some("Set or unset dinit's activation environment"));
    let add = gtk::Button::with_label("Add…");
    add.set_tooltip_text(Some("Add a user service"));
    toolbar.append(&scope);
    toolbar.append(&search);
    toolbar.append(&status_filter);
    toolbar.append(&refresh);
    toolbar.append(&environment_btn);
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
    // Lifecycle actions (change the service's actual state) live here; Status/Buffered
    // log/Unpin are view-only console controls and sit in the console's own header instead.
    let controls = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let mut reload = None;
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
            let button = gtk::Button::with_label(label);
            button.set_widget_name(action);
            if action == "reload" {
                reload = Some(button.clone());
            }
            row.append(&button);
        }
        controls.append(&row);
    }
    // Relabeled per scope's backend by apply_backend_dependent_ui(); "Reload" is the default.
    let reload = reload.expect("reload button is always created above");
    let lifecycle_extra = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let signal_btn = gtk::Button::with_label("Signal…");
    let deps_btn = gtk::Button::with_label("Dependencies…");
    let enable_log_btn = gtk::Button::with_label("Enable buffered log");
    enable_log_btn.set_tooltip_text(Some(
        "Edit this service's definition to turn on buffered logging, then reload it \
         (user-scope services only)",
    ));
    let delete = gtk::Button::with_label("Delete");
    delete.add_css_class("destructive-action");
    lifecycle_extra.append(&signal_btn);
    lifecycle_extra.append(&deps_btn);
    lifecycle_extra.append(&enable_log_btn);
    lifecycle_extra.append(&delete);
    controls.append(&lifecycle_extra);
    controls.set_sensitive(false);
    right.append(&controls);

    let console = gtk::Box::new(gtk::Orientation::Vertical, 0);
    console.add_css_class("card");
    let console_header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    console_header.set_margin_start(8);
    console_header.set_margin_end(8);
    console_header.set_margin_top(6);
    console_header.set_margin_bottom(6);
    let console_label = gtk::Label::new(Some("Console"));
    console_label.set_xalign(0.0);
    console_label.set_hexpand(true);
    console_label.add_css_class("dim-label");
    let console_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let status = gtk::Button::with_label("Status");
    let logs = gtk::Button::with_label("Buffered log");
    let unpin = gtk::Button::with_label("Unpin");
    console_actions.append(&status);
    console_actions.append(&logs);
    console_actions.append(&unpin);
    console_actions.set_sensitive(false); // gated with `controls` by row selection
    console_header.append(&console_label);
    console_header.append(&console_actions);
    console.append(&console_header);
    console.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let view = gtk::TextView::new();
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_top_margin(8);
    view.set_bottom_margin(8);
    view.set_left_margin(8);
    view.set_right_margin(8);
    let detail = view.buffer();
    detail.set_text("Select a service to inspect it.");
    let detail_scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .vexpand(true)
        .build();
    console.append(&detail_scroll);
    right.append(&console);
    pane.set_end_child(Some(&right));
    body.append(&pane);
    root.append(&body);
    let message = gtk::Label::new(None);
    message.set_xalign(0.0);
    message.set_wrap(true);
    message.set_selectable(true);
    message.add_css_class("dim-label");
    root.append(&message);
    window.set_child(Some(&root));
    let ui = Ui {
        system_backend,
        user_backend,
        window,
        header_title,
        body,
        scope,
        search,
        status_filter,
        list,
        title,
        subtitle,
        detail,
        message,
        controls,
        console_actions,
        signal_btn: signal_btn.clone(),
        deps_btn: deps_btn.clone(),
        enable_log_btn: enable_log_btn.clone(),
        environment_btn: environment_btn.clone(),
        logs,
        unpin,
        reload,
        delete: delete.clone(),
        services: Default::default(),
        rows: Default::default(),
    };
    ui.apply_backend_dependent_ui();
    let clone = ui.clone();
    refresh.connect_clicked(move |_| clone.refresh());
    let clone = ui.clone();
    environment_btn.connect_clicked(move |_| clone.show_environment_dialog());
    let clone = ui.clone();
    signal_btn.connect_clicked(move |_| clone.show_signal_dialog());
    let clone = ui.clone();
    deps_btn.connect_clicked(move |_| clone.show_dependencies_dialog());
    let clone = ui.clone();
    enable_log_btn.connect_clicked(move |_| clone.enable_buffered_log());
    let clone = ui.clone();
    ui.status_filter
        .connect_selected_notify(move |_| clone.filter());
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
        clone.console_actions.set_sensitive(row.is_some());
        if let Some(service) = clone.selected() {
            clone
                .title
                .set_text(&format!("{} · {}", service.name, service.state));
            let enabled = clone
                .backend()
                .enabled(clone.scope(), &service.name)
                .unwrap_or_default();
            let path = clone
                .backend()
                .definition_path(clone.scope(), &service.name)
                .map(|p| format!(" · {}", p.display()))
                .unwrap_or_default();
            clone.subtitle.set_text(&format!("{enabled}{path}"));
            let deletable = clone.scope() == Scope::User
                && if clone.backend() == Backend::Runit {
                    service.state == "Disabled"
                } else {
                    !matches!(service.state.as_str(), "Started" | "Starting" | "Stopping")
                };
            clone.delete.set_sensitive(deletable);
            clone.refresh_selected();
        } else {
            clone.subtitle.set_text("");
            clone.delete.set_sensitive(false);
        }
    });
    let clone = ui.clone();
    status.connect_clicked(move |_| clone.refresh_selected());
    let clone = ui.clone();
    ui.logs.connect_clicked(move |_| clone.show_log());
    let clone = ui.clone();
    ui.unpin.connect_clicked(move |_| clone.action("unpin"));
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
    let (system_backend, user_backend) = match Backend::detect() {
        Ok(backends) => backends,
        Err(error) => {
            eprintln!("pilot: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let app = gtk::Application::builder()
        .application_id("org.voidlab.Pilot")
        .build();
    app.connect_activate(move |app| build(app, system_backend, user_backend));
    app.connect_shutdown(|_| pilot::shutdown_helper());
    // `--dinit`/`--runit` are Pilot's own flags, already consumed by `Backend::detect()` above.
    // `run()` would hand the full argv to GLib's own option parser, which rejects them as
    // unknown; pass only argv[0] through so GLib has nothing left to reject.
    let program = std::env::args().next().unwrap_or_else(|| "pilot".into());
    app.run_with_args(&[program])
}
