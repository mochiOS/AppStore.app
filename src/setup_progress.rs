//! Standard Store UI for the broker's device-wide setup queue. Polling stays
//! off the UI thread; this adapter renders ordinary ViewKit settings rows.
use mochios_package_protocol::provisioning as wire;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use viewkit::{
    event::{EventContext, EventResult, ViewEvent},
    prelude::*,
    view::{Constraints, MeasureContext, PaintContext},
};

#[derive(Clone, Debug)]
struct Row {
    phase: wire::Phase,
    percent: u8,
    error: String,
}

type Snapshot = Result<Vec<Row>, String>;

pub(super) struct SetupProgress {
    pending: Arc<Mutex<Option<Snapshot>>>,
    current: RefCell<Option<Snapshot>>,
    stop: Arc<AtomicBool>,
}

impl SetupProgress {
    pub(super) fn new() -> Rc<Self> {
        let pending = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        #[cfg(target_os = "mochios")]
        {
            let stop_worker = stop.clone();
            let worker_pending = pending.clone();
            let spawned = std::thread::Builder::new()
                .name("store-setup-status".into())
                .spawn(move || {
                    let _ = mochi_user_platform::ipc::create();
                    while !stop_worker.load(Ordering::Relaxed) {
                        let snapshot =
                            mochios_package_client::standard_applications(wire::Request::Query)
                                .map(|rows| {
                                    rows.into_iter()
                                        .map(|row| Row {
                                            phase: row.phase,
                                            percent: row.percent,
                                            error: row.error,
                                        })
                                        .collect()
                                })
                                .map_err(|error| error.to_string());
                        let complete = snapshot.as_ref().is_ok_and(|rows: &Vec<Row>| {
                            rows.iter().all(|row| row.phase == wire::Phase::Complete)
                        });
                        // Keep only the newest snapshot, even while Installed
                        // is not visible. Completion stops polling immediately.
                        if let Ok(mut slot) = worker_pending.lock() {
                            *slot = Some(snapshot);
                        } else {
                            return;
                        }
                        if complete {
                            break;
                        }
                        for _ in 0..10 {
                            if stop_worker.load(Ordering::Relaxed) {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(200));
                        }
                    }
                });
            if let Err(error) = spawned {
                if let Ok(mut slot) = pending.lock() {
                    *slot = Some(Err(error.to_string()));
                }
            }
        }
        #[cfg(not(target_os = "mochios"))]
        {
            *pending.lock().unwrap() = Some(Ok(Vec::new()));
        }
        Rc::new(Self {
            pending,
            current: RefCell::new(None),
            stop,
        })
    }

    fn content(&self) -> VStack {
        // The mutex only protects a move of five small records; no IPC or
        // filesystem operation runs while the UI can hold this lock.
        if let Ok(mut slot) = self.pending.lock() {
            if let Some(snapshot) = slot.take() {
                *self.current.borrow_mut() = Some(snapshot);
            }
        }
        let mut content = VStack::new()
            .alignment(StackAlignment::Stretch)
            .gap(StackGap::Small);
        let current = self.current.borrow();
        match current.as_ref() {
            Some(Ok(rows)) if rows.iter().any(|row| row.phase != wire::Phase::Complete) => {
                content = content
                    .child(SettingsSection::new("Setting Up Your Apps").rows(
                        rows.iter().zip(wire::APPLICATIONS).map(|(row, (_, name))| {
                            let value = match row.phase {
                                wire::Phase::Pending => "Waiting".into(),
                                wire::Phase::Downloading => format!("{}%", row.percent),
                                wire::Phase::Installing => "Installing…".into(),
                                wire::Phase::Failed => "Will Retry".into(),
                                wire::Phase::Complete => "Installed".into(),
                            };
                            settings_value(name, &row.error, value)
                        }),
                    ))
                    .child(Text::metadata(
                        "Setup continues in the background. You can keep using mochiOS.",
                    ));
            }
            Some(Err(error)) => {
                content = content.child(
                    SettingsSection::new("Application Setup").row(settings_value(
                        "Setup status is unavailable",
                        error,
                        "",
                    )),
                );
            }
            None => {
                content = content.child(Text::metadata("Checking application setup…"));
            }
            _ => {}
        }
        content
    }
}

impl Drop for SetupProgress {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl View for SetupProgress {
    fn measure(&self, constraints: Constraints, context: &mut MeasureContext<'_>) -> Size {
        self.content().measure(constraints, context)
    }

    fn paint(&self, bounds: Rect, context: &mut PaintContext<'_>) {
        self.content().paint(bounds, context);
        if self.current.borrow().as_ref().is_none_or(|snapshot| {
            snapshot.as_ref().map_or(true, |rows| {
                rows.iter().any(|row| row.phase != wire::Phase::Complete)
            })
        }) {
            context.request_redraw_at(Instant::now() + Duration::from_millis(500));
        }
    }

    fn handle_event(
        &self,
        bounds: Rect,
        event: &ViewEvent,
        context: &mut EventContext<'_>,
    ) -> EventResult {
        self.content().handle_event(bounds, event, context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viewkit::{
        draw_command::{DisplayList, DrawCommand},
        typography::TextMeasurer,
    };

    fn panel(rows: Snapshot) -> SetupProgress {
        SetupProgress {
            pending: Arc::new(Mutex::new(Some(rows))),
            current: RefCell::new(None),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    fn text(panel: &SetupProgress, theme: &Theme) -> Vec<String> {
        let mut list = DisplayList::new();
        let mut text = TextMeasurer::new();
        panel.paint(
            Rect::new(0., 0., 700., 500.),
            &mut PaintContext::new(&mut list, theme, &theme.typography, &mut text),
        );
        list.commands()
            .iter()
            .filter_map(|command| match command {
                DrawCommand::DrawText { command } => Some(command.text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn progress_and_failure_reasons_use_standard_rows_in_both_themes() {
        for theme in [Theme::LIGHT, Theme::DARK] {
            let mut rows = vec![
                Row {
                    phase: wire::Phase::Complete,
                    percent: 100,
                    error: String::new()
                };
                5
            ];
            rows[0] = Row {
                phase: wire::Phase::Failed,
                percent: 0,
                error: "No compatible release is available.".into(),
            };
            rows[1] = Row {
                phase: wire::Phase::Downloading,
                percent: 45,
                error: String::new(),
            };
            let labels = text(&panel(Ok(rows)), &theme);
            for expected in [
                "Terra",
                "Edit",
                "Will Retry",
                "45%",
                "No compatible release is available.",
            ] {
                assert!(
                    labels.iter().any(|label| label == expected),
                    "Missing {expected}"
                );
            }
        }
    }

    #[test]
    fn complete_queue_is_hidden_but_backend_errors_remain_visible() {
        let rows = vec![
            Row {
                phase: wire::Phase::Complete,
                percent: 100,
                error: String::new()
            };
            5
        ];
        assert!(text(&panel(Ok(rows)), &Theme::LIGHT).is_empty());
        assert!(
            text(
                &panel(Err("The device ledger could not be read.".into())),
                &Theme::LIGHT
            )
            .iter()
            .any(|label| label == "The device ledger could not be read.")
        );
    }
}
