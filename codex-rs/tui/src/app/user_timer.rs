//! User-owned recurring notifications for the primary thread.
//!
//! This stays in the TUI rather than the model tool surface. Only a slash command parsed from the
//! user composer can mutate it. Each tick is delivered immediately: it steers a running regular
//! turn or starts a fresh turn when the primary thread is idle. Timer ticks are never accumulated
//! in a local follow-up queue.

use std::time::Duration;

use codex_app_server_protocol::UserInput;
use codex_protocol::ThreadId;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio::time::MissedTickBehavior;

use super::*;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimerSteerOutcome {
    Delivered,
    StartTurn,
    Failed,
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

#[derive(Default)]
pub(super) struct UserTimerState {
    generation: u64,
    active: Option<ActiveUserTimer>,
    delivered: u64,
    skipped: u64,
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
        self.delivered = 0;
        self.skipped = 0;
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

    #[cfg(test)]
    pub(super) fn active_generation(&self) -> Option<u64> {
        self.active.as_ref().map(|timer| timer.generation)
    }

    #[cfg(test)]
    pub(super) fn delivery_counts(&self) -> (u64, u64) {
        (self.delivered, self.skipped)
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
                        "A notification already delivered to a running turn is not interrupted."
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
                        "Main agent notification: {notification}\nBusy turns are steered immediately; idle threads start a new turn. Ticks are not queued.\nUse /timer status or /timer stop."
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
        let delivered = self.user_timer.delivered;
        let skipped = self.user_timer.skipped;
        let mut hint = format!(
            "Every {} on main thread {}: {}\nDelivery: steer while busy; start a new turn while idle; never queue.\nDelivered: {delivered}; skipped: {skipped}.",
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
        Box::pin(self.handle_user_timer_tick_inner(app_server, generation)).await;
    }

    async fn handle_user_timer_tick_inner(
        &mut self,
        app_server: &mut AppServerSession,
        generation: u64,
    ) {
        let Some((thread_id, notification)) = self
            .user_timer
            .active
            .as_ref()
            .filter(|timer| timer.generation == generation)
            .map(|timer| (timer.thread_id, timer.notification.clone()))
        else {
            return;
        };
        if self.primary_thread_id != Some(thread_id) {
            self.user_timer.stop();
            return;
        }

        let primary_is_visible = self.active_thread_id == Some(thread_id)
            && self.chat_widget.thread_id() == Some(thread_id);
        let items = vec![UserInput::Text {
            text: notification.clone(),
            text_elements: Vec::new(),
        }];

        if let Some(turn_id) = self.active_turn_id_for_thread(thread_id).await {
            match Box::pin(self.try_steer_user_timer_notification(
                app_server,
                thread_id,
                turn_id,
                items.clone(),
                primary_is_visible,
            ))
            .await
            {
                TimerSteerOutcome::Delivered | TimerSteerOutcome::Failed => return,
                TimerSteerOutcome::StartTurn => {}
            }
        }

        Box::pin(self.start_user_timer_notification_turn(
            app_server,
            thread_id,
            notification,
            items,
            primary_is_visible,
        ))
        .await;
    }

    async fn try_steer_user_timer_notification(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        mut steer_turn_id: String,
        items: Vec<UserInput>,
        primary_is_visible: bool,
    ) -> TimerSteerOutcome {
        let mut retried_after_turn_mismatch = false;
        loop {
            match app_server
                .turn_steer(thread_id, steer_turn_id.clone(), items.clone())
                .await
            {
                Ok(_) => {
                    self.record_user_timer_delivery_success();
                    return TimerSteerOutcome::Delivered;
                }
                Err(error) => {
                    if let Some(turn_error) = active_turn_not_steerable_turn_error(&error) {
                        self.record_user_timer_delivery_failure(
                            thread_id,
                            turn_error.message,
                            primary_is_visible,
                        );
                        return TimerSteerOutcome::Failed;
                    }
                    match active_turn_steer_race(&error) {
                        Some(ActiveTurnSteerRace::Missing) => {
                            if let Some(channel) = self.thread_event_channels.get(&thread_id) {
                                channel.store.lock().await.clear_active_turn_id();
                            }
                            return TimerSteerOutcome::StartTurn;
                        }
                        Some(ActiveTurnSteerRace::ExpectedTurnMismatch { actual_turn_id })
                            if !retried_after_turn_mismatch && actual_turn_id != steer_turn_id =>
                        {
                            if let Some(channel) = self.thread_event_channels.get(&thread_id) {
                                channel.store.lock().await.active_turn_id =
                                    Some(actual_turn_id.clone());
                            }
                            steer_turn_id = actual_turn_id;
                            retried_after_turn_mismatch = true;
                        }
                        Some(ActiveTurnSteerRace::ExpectedTurnMismatch { actual_turn_id }) => {
                            if let Some(channel) = self.thread_event_channels.get(&thread_id) {
                                channel.store.lock().await.active_turn_id = Some(actual_turn_id);
                            }
                            self.record_user_timer_delivery_failure(
                                thread_id,
                                format!("{error:#}"),
                                primary_is_visible,
                            );
                            return TimerSteerOutcome::Failed;
                        }
                        None => {
                            self.record_user_timer_delivery_failure(
                                thread_id,
                                format!("{error:#}"),
                                primary_is_visible,
                            );
                            return TimerSteerOutcome::Failed;
                        }
                    }
                }
            }
        }
    }

    async fn start_user_timer_notification_turn(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        notification: String,
        items: Vec<UserInput>,
        primary_is_visible: bool,
    ) {
        let Some(session) = self
            .primary_session_configured
            .as_ref()
            .filter(|session| session.thread_id == thread_id)
            .cloned()
        else {
            self.record_user_timer_delivery_failure(
                thread_id,
                "main thread configuration is unavailable".to_string(),
                primary_is_visible,
            );
            return;
        };
        let permissions_override = Self::turn_permissions_override_from_config(
            self.chat_widget.config_ref(),
            session.active_permission_profile.as_ref(),
            self.runtime_permission_profile_override
                .as_ref()
                .map(|profile| &profile.permission_profile),
        );
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
        let response = Box::pin(app_server.turn_start(
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
        ))
        .await;
        match response {
            Ok(response) => {
                self.record_user_timer_delivery_success();
                let turn_id = response.turn.id;
                let store = Arc::clone(&self.ensure_thread_channel(thread_id).store);
                store.lock().await.active_turn_id = Some(turn_id.clone());
                if primary_is_visible {
                    self.chat_widget
                        .note_user_timer_turn_started(turn_id, &turn, notification);
                }
            }
            Err(error) => {
                self.record_user_timer_delivery_failure(
                    thread_id,
                    format!("{error:#}"),
                    primary_is_visible,
                );
            }
        }
    }

    fn record_user_timer_delivery_success(&mut self) {
        self.user_timer.delivered = self.user_timer.delivered.saturating_add(1);
        self.user_timer.last_delivery_error = None;
    }

    fn record_user_timer_delivery_failure(
        &mut self,
        thread_id: ThreadId,
        error: String,
        primary_is_visible: bool,
    ) {
        self.user_timer.skipped = self.user_timer.skipped.saturating_add(1);
        let changed = self.user_timer.last_delivery_error.as_deref() != Some(&error);
        self.user_timer.last_delivery_error = Some(error.clone());
        tracing::warn!(thread_id = %thread_id, error, "failed to deliver user timer notification");
        if changed && primary_is_visible {
            self.chat_widget.add_error_message(format!(
                "Timer notification was skipped because immediate delivery failed: {error}"
            ));
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
