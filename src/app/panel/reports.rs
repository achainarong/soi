//! Reports from agents: the socket agent integrations tell their state on.

use super::*;

const MAX_REPORT_CONNECTIONS: usize = 32;
const REPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Own the listener instead of leaking a reference into the main context.
pub(in crate::app) struct ReportListener(gio::SocketService);

impl Drop for ReportListener {
    fn drop(&mut self) {
        self.0.stop();
        self.0.close();
    }
}

struct ReportSlot(Rc<Cell<usize>>);

impl Drop for ReportSlot {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

/// One bounded JSON line, not a read-all waiting for the sender to close.
async fn socket_report(input: &gio::InputStream) -> Option<agents::Report> {
    let mut data = Vec::new();
    loop {
        let remaining = agents::MAX_REPORT_BYTES + 1 - data.len();
        let bytes = input
            .read_bytes_future(remaining.min(4096), glib::Priority::DEFAULT)
            .await
            .ok()?;
        let newline = bytes.iter().position(|b| *b == b'\n');
        data.extend_from_slice(&bytes[..newline.unwrap_or(bytes.len())]);
        if data.len() > agents::MAX_REPORT_BYTES {
            return None;
        }
        if newline.is_some() || bytes.is_empty() {
            return agents::Report::parse(&data);
        }
    }
}

/// Listens for reports from agents (see `crate::agents`).
pub(in crate::app) fn listen(shared: &Rc<Shared>) {
    let Some(path) = agents::socket_path() else {
        crate::log::write("no runtime directory: agents can't report their state");
        return;
    };
    shared.report_listener.take();
    // Only one soi runs per user (GTK makes sure), so an old socket file
    // is left over from a crash.
    let _ = fs::remove_file(&path);
    let service = gio::SocketService::new();
    let address = gio::UnixSocketAddress::new(&path);
    if let Err(err) = service.add_address(
        &address,
        gio::SocketType::Stream,
        gio::SocketProtocol::Default,
        None::<&glib::Object>,
    ) {
        crate::log::write(&format!("cannot listen on {}: {err}", path.display()));
        return;
    }
    // Only this user may report.
    if let Err(err) =
        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
    {
        service.close();
        crate::log::write(&format!(
            "cannot protect agent socket {}: {err}",
            path.display()
        ));
        return;
    }
    let weak = Rc::downgrade(shared);
    let active = Rc::new(Cell::new(0));
    service.connect_incoming(move |_, connection, _| {
        if active.get() >= MAX_REPORT_CONNECTIONS {
            let _ = connection.close(gio::Cancellable::NONE);
            return true;
        }
        active.set(active.get() + 1);
        let slot = ReportSlot(active.clone());
        let weak = weak.clone();
        let connection = connection.clone();
        glib::spawn_future_local(async move {
            let _slot = slot;
            let input = connection.input_stream();
            let result = glib::future_with_timeout(REPORT_TIMEOUT, socket_report(&input)).await;
            let _ = connection.close(gio::Cancellable::NONE);
            let Ok(Some(report)) = result else {
                return;
            };
            if let (Some(shared), Some(state)) = (weak.upgrade(), AgentState::parse(&report.state))
            {
                apply_report(&shared, (report.tab, state, report.message), report.usage);
                if let Some(metadata) = report.session
                    && let Some(tab) = shared
                        .tabs
                        .borrow()
                        .get(&report.tab)
                        .and_then(Weak::upgrade)
                {
                    if !metadata.id.is_empty() && metadata.resume_command().is_ok() {
                        tab.resume.replace(Some(metadata.clone()));
                    }
                    tasks::record_report(&shared, &tab, metadata);
                }
            }
        });
        true
    });
    service.start();
    shared
        .report_listener
        .replace(Some(ReportListener(service)));
}

fn apply_report(
    shared: &Rc<Shared>,
    (id, state, message): (Uuid, AgentState, String),
    usage: Option<agents::Usage>,
) {
    let Some(tab) = shared.tabs.borrow().get(&id).and_then(Weak::upgrade) else {
        return;
    };
    let now = Instant::now();
    let visible = is_visible(&tab);
    let mut changed = tab.agent.borrow_mut().report(state, &message, visible, now);
    // Kept when a report leaves it out, e.g. before the first reply.
    if let Some(usage) = usage {
        let mut agent = tab.agent.borrow_mut();
        changed |= agent.usage.as_ref() != Some(&usage);
        agent.usage = Some(usage);
    }
    // Learn the agent's program right away.
    let programs = tab.terminal.running_programs();
    changed |= observe(&tab, &programs, &agent_names(shared), now);
    if changed {
        changed_agent(shared, &tab);
    }
}
