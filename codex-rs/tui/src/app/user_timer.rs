//! User-owned recurring notifications for the primary thread.
//!
//! This stays in the TUI rather than the model tool surface. Only a slash command parsed from the
//! user composer can mutate it. Timer ticks are kept in a dedicated queue and use `turn/start`
//! only after both the UI and app-server state say the primary thread is idle, so they can never
//! become a steer of the current turn.

use std::collections::VecDeque;
use std::time::Duration;

use codex_app_server_protocol::UserInput;
use codex_protocol::ThreadId;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio::time::MissedTickBehavior;

use super::*;

const MAX_PENDING_NOTIFICATIONS: usize = 64;
const MAX_NOTIFICATION_CHARS: usize = 4_000;
const TIMER_USAGE: &str = "Usage: /timer <duration> <notification> | /timer status | /timer stop";

#[derive(Debug, PartialEq, Eq)]
enum TimerCommand {
    Status,
    Stop,
    Start {
        interval: Duration,
        notification: String,
    },
}

struct ActiveUserTimer {
    generation: u64,
    thread_id: ThreadId,
    interval: Duration,
    notification: String,
    task: JoinHandle<()>,
}

impl Drop for ActiveUserTimer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingNotification {
    generation: u64,
    text: String,
}

#[derive(Default)]
pub(super) struct UserTimerState {
    generation: u64,
    active: Option<ActiveUserTimer>,
    pending: VecDeque<PendingNotification>,
    dropped: u64,
    last_delivery_error: Option<String>,
}

impl UserTimerState {
    fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    pub(super) fn stop(&mut self) -> bool {
        let was_active = self.active.take().is_some();
        self.next_generation();
        self.pending.clear();
        self.dropped = 0;
        self.last_delivery_error = None;
        was_active
    }

    fn cancel_if_target_changed(&mut self, thread_id: ThreadId) {
        if self
            .active
            .as_ref()
            .is_some_and(|timer| timer.thread_id != thread_id)
        {
            self.stop();
        }
    }

    pub(super) fn enqueue_tick(&mut self, generation: u64) -> bool {
        let Some(timer) = self
            .active
            .as_ref()
            .filter(|timer| timer.generation == generation)
        else {
            return false;
        };
        if self.pending.len() >= MAX_PENDING_NOTIFICATIONS {
            self.dropped = self.dropped.saturating_add(1);
            return false;
        }
        self.pending.push_back(PendingNotification {
            generation,
            text: timer.notification.clone(),
        });
        true
    }

    #[cfg(test)]
    pub(super) fn active_generation(&self) -> Option<u64> {
        self.active.as_ref().map(|timer| timer.generation)
    }

    #[cfg(test)]
    pub(super) fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

impl App {
    pub(super) fn handle_user_timer_command(&mut self, args: String) {
        let command = match parse_timer_command(&args) {
            Ok(command) => command,
            Err(error) => {
                self.chat_widget
                    .add_error_message(format!("{error}\n{TIMER_USAGE}"));
                return;
            }
        };

        match command {
            TimerCommand::Status => self.show_user_timer_status(),
            TimerCommand::Stop => {
                let was_active = self.user_timer.stop();
                let title = if was_active {
                    "Timer stopped."
                } else {
                    "No timer is active."
                };
                self.chat_widget.add_info_message(
                    title.to_string(),
                    Some(
                        "Any timer notification turn already running is not interrupted."
                            .to_string(),
                    ),
                );
            }
            TimerCommand::Start {
                interval,
                notification,
            } => {
                let Some(thread_id) = self.primary_thread_id else {
                    self.chat_widget.add_error_message(
                        "Cannot start a timer before the main thread is ready.".to_string(),
                    );
                    return;
                };
                if self.primary_session_configured.is_none() {
                    self.chat_widget.add_error_message(
                        "Cannot start a timer before the main thread is configured.".to_string(),
                    );
                    return;
                }

                self.user_timer.stop();
                let generation = self.user_timer.next_generation();
                let app_event_tx = self.app_event_tx.clone();
                let task = tokio::spawn(async move {
                    let start = Instant::now() + interval;
                    let mut ticker = tokio::time::interval_at(start, interval);
                    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
                    loop {
                        ticker.tick().await;
                        app_event_tx.send(AppEvent::UserTimerTick { generation });
                    }
                });
                self.user_timer.active = Some(ActiveUserTimer {
                    generation,
                    thread_id,
                    interval,
                    notification: notification.clone(),
                    task,
                });
                self.chat_widget.add_info_message(
                    format!("Timer set for every {}.", display_duration(interval)),
                    Some(format!(
                        "Main agent notification: {notification}\nUse /timer status or /timer stop."
                    )),
                );
            }
        }
    }

    fn show_user_timer_status(&mut self) {
        let Some(timer) = self.user_timer.active.as_ref() else {
            self.chat_widget.add_info_message(
                "No timer is active.".to_string(),
                Some(TIMER_USAGE.to_string()),
            );
            return;
        };
        let queued = self.user_timer.pending.len();
        let dropped = self.user_timer.dropped;
        let mut hint = format!(
            "Every {} on main thread {}: {}\nQueued: {queued}; dropped after queue limit: {dropped}.",
            display_duration(timer.interval),
            timer.thread_id,
            timer.notification
        );
        if let Some(error) = &self.user_timer.last_delivery_error {
            hint.push_str(&format!("\nLast delivery error: {error}"));
        }
        self.chat_widget
            .add_info_message("Timer active.".to_string(), Some(hint));
    }

    pub(super) fn cancel_user_timer_if_primary_changed(&mut self, thread_id: ThreadId) {
        self.user_timer.cancel_if_target_changed(thread_id);
    }

    pub(super) fn cancel_user_timer_for_thread(&mut self, thread_id: ThreadId) {
        if self
            .user_timer
            .active
            .as_ref()
            .is_some_and(|timer| timer.thread_id == thread_id)
        {
            self.user_timer.stop();
        }
    }

    pub(super) async fn handle_user_timer_tick(
        &mut self,
        app_server: &mut AppServerSession,
        generation: u64,
    ) {
        if self.user_timer.enqueue_tick(generation) {
            self.maybe_start_next_user_timer_notification(app_server)
                .await;
        }
    }

    pub(super) async fn maybe_start_next_user_timer_notification(
        &mut self,
        app_server: &mut AppServerSession,
    ) {
        let Some(pending) = self.user_timer.pending.front().cloned() else {
            return;
        };
        let Some(timer) = self
            .user_timer
            .active
            .as_ref()
            .filter(|timer| timer.generation == pending.generation)
        else {
            self.user_timer.pending.pop_front();
            return;
        };
        let thread_id = timer.thread_id;
        if self.primary_thread_id != Some(thread_id) {
            self.user_timer.stop();
            return;
        }

        let primary_is_visible = self.active_thread_id == Some(thread_id)
            && self.chat_widget.thread_id() == Some(thread_id);
        if primary_is_visible && self.chat_widget.is_user_turn_pending_or_running() {
            return;
        }
        if self.active_turn_id_for_thread(thread_id).await.is_some() {
            return;
        }

        let Some(session) = self
            .primary_session_configured
            .as_ref()
            .filter(|session| session.thread_id == thread_id)
            .cloned()
        else {
            return;
        };
        let permissions_override = Self::turn_permissions_override_from_config(
            self.chat_widget.config_ref(),
            session.active_permission_profile.as_ref(),
            self.runtime_permission_profile_override
                .as_ref()
                .map(|profile| &profile.permission_profile),
        );
        let items = vec![UserInput::Text {
            text: pending.text.clone(),
            text_elements: Vec::new(),
        }];
        let turn = AppCommand::UserTurn {
            items: items.clone(),
            cwd: session.cwd.to_path_buf(),
            approval_policy: session.approval_policy,
            approvals_reviewer: Some(session.approvals_reviewer),
            active_permission_profile: session.active_permission_profile.clone(),
            model: session.model.clone(),
            effort: session.reasoning_effort.clone(),
            summary: None,
            service_tier: Some(session.service_tier.clone()),
            final_output_json_schema: None,
            collaboration_mode: session.collaboration_mode.as_deref().cloned(),
            personality: session.personality,
        };
        let response = app_server
            .turn_start(
                thread_id,
                items,
                session.cwd.to_path_buf(),
                session.approval_policy,
                session.approvals_reviewer,
                permissions_override,
                &session.runtime_workspace_roots,
                session.model,
                session.reasoning_effort,
                /*summary*/ None,
                Some(session.service_tier),
                session.collaboration_mode.map(|mode| *mode),
                session.personality,
                /*output_schema*/ None,
            )
            .await;
        match response {
            Ok(response) => {
                self.user_timer.pending.pop_front();
                self.user_timer.last_delivery_error = None;
                let turn_id = response.turn.id;
                let store = Arc::clone(&self.ensure_thread_channel(thread_id).store);
                store.lock().await.active_turn_id = Some(turn_id.clone());
                if primary_is_visible {
                    self.chat_widget
                        .note_user_timer_turn_started(turn_id, &turn, pending.text);
                }
            }
            Err(error) => {
                let error = format!("{error:#}");
                let changed = self.user_timer.last_delivery_error.as_deref() != Some(&error);
                self.user_timer.last_delivery_error = Some(error.clone());
                tracing::warn!(thread_id = %thread_id, error, "failed to deliver user timer notification");
                if changed && primary_is_visible {
                    self.chat_widget.add_error_message(format!(
                        "Timer notification is still queued because turn/start failed: {error}"
                    ));
                }
            }
        }
    }
}

fn parse_timer_command(args: &str) -> Result<TimerCommand, String> {
    let words = args.split_whitespace().collect::<Vec<_>>();
    match words.as_slice() {
        [] => return Ok(TimerCommand::Status),
        [command] if command.eq_ignore_ascii_case("status") => return Ok(TimerCommand::Status),
        [command] if command.eq_ignore_ascii_case("stop") => return Ok(TimerCommand::Stop),
        _ => {}
    }

    let (interval, consumed) = parse_duration_prefix(&words)?;
    let notification = words[consumed..].join(" ");
    if notification.is_empty() {
        return Err("A notification message is required.".to_string());
    }
    if notification.chars().count() > MAX_NOTIFICATION_CHARS {
        return Err(format!(
            "Notification is too long (maximum {MAX_NOTIFICATION_CHARS} characters)."
        ));
    }
    Ok(TimerCommand::Start {
        interval,
        notification,
    })
}

fn parse_duration_prefix(words: &[&str]) -> Result<(Duration, usize), String> {
    let Some(first) = words.first() else {
        return Err("A duration is required.".to_string());
    };
    let digit_end = first
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit())
        .last()
        .map_or(0, |(index, ch)| index + ch.len_utf8());
    if digit_end == 0 {
        return Err(format!("Invalid duration: {first}"));
    }
    let value = first[..digit_end]
        .parse::<u64>()
        .map_err(|_| format!("Invalid duration: {first}"))?;
    if value == 0 {
        return Err("Duration must be greater than zero.".to_string());
    }

    let inline_unit = &first[digit_end..];
    let (unit, consumed) = if inline_unit.is_empty() {
        match words
            .get(1)
            .copied()
            .filter(|word| duration_unit_seconds(word).is_some())
        {
            Some(unit) => (unit, 2),
            None => ("s", 1),
        }
    } else {
        (inline_unit, 1)
    };
    let unit_seconds =
        duration_unit_seconds(unit).ok_or_else(|| format!("Unsupported duration unit: {unit}"))?;
    let seconds = value
        .checked_mul(unit_seconds)
        .ok_or_else(|| "Duration is too large.".to_string())?;
    Ok((Duration::from_secs(seconds), consumed))
}

fn duration_unit_seconds(unit: &str) -> Option<u64> {
    match unit.to_ascii_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(1),
        "m" | "min" | "mins" | "minute" | "minutes" => Some(60),
        "h" | "hr" | "hrs" | "hour" | "hours" => Some(60 * 60),
        "d" | "day" | "days" => Some(24 * 60 * 60),
        _ => None,
    }
}

fn display_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds.is_multiple_of(24 * 60 * 60) {
        format!("{}d", seconds / (24 * 60 * 60))
    } else if seconds.is_multiple_of(60 * 60) {
        format!("{}h", seconds / (60 * 60))
    } else if seconds.is_multiple_of(60) {
        format!("{}min", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
#[path = "user_timer_tests.rs"]
mod tests;
