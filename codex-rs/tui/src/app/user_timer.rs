//! User-owned scheduled notifications for the primary thread.
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

const MAX_ACTIVE_TIMERS: usize = 64;
const MAX_NOTIFICATION_CHARS: usize = 4_000;
const TIMER_USAGE: &str = "Usage: /timer <duration> <notification> | /timer once <duration> <notification> | /timer [status] | /timer stop";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimerMode {
    Recurring,
    Once,
}

#[derive(Debug, PartialEq, Eq)]
enum TimerCommand {
    Status,
    Stop,
    Start {
        mode: TimerMode,
        interval: Duration,
        notification: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum TimerSteerOutcome {
    Delivered,
    StartTurn,
    Failed(String),
}

struct ActiveUserTimer {
    timer_id: u64,
    thread_id: ThreadId,
    mode: TimerMode,
    interval: Duration,
    notification: String,
    delivered: u64,
    skipped: u64,
    last_delivery_error: Option<String>,
    task: JoinHandle<()>,
}

impl Drop for ActiveUserTimer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Default)]
pub(super) struct UserTimerState {
    next_timer_id: u64,
    active: Vec<ActiveUserTimer>,
}

impl UserTimerState {
    fn next_timer_id(&mut self) -> u64 {
        loop {
            self.next_timer_id = self.next_timer_id.wrapping_add(1);
            if self.next_timer_id != 0
                && !self
                    .active
                    .iter()
                    .any(|timer| timer.timer_id == self.next_timer_id)
            {
                return self.next_timer_id;
            }
        }
    }

    pub(super) fn stop_all(&mut self) -> usize {
        let stopped = self.active.len();
        self.active.clear();
        stopped
    }

    fn stop_timer(&mut self, timer_id: u64) -> bool {
        let Some(index) = self
            .active
            .iter()
            .position(|timer| timer.timer_id == timer_id)
        else {
            return false;
        };
        self.active.remove(index);
        true
    }

    fn cancel_if_target_changed(&mut self, thread_id: ThreadId) {
        if self.active.iter().any(|timer| timer.thread_id != thread_id) {
            self.stop_all();
        }
    }

    #[cfg(test)]
    pub(super) fn active_timer_ids(&self) -> Vec<u64> {
        self.active.iter().map(|timer| timer.timer_id).collect()
    }

    #[cfg(test)]
    pub(super) fn delivery_counts(&self, timer_id: u64) -> Option<(u64, u64)> {
        self.active
            .iter()
            .find(|timer| timer.timer_id == timer_id)
            .map(|timer| (timer.delivered, timer.skipped))
    }

    #[cfg(test)]
    pub(super) fn notification(&self, timer_id: u64) -> Option<&str> {
        self.active
            .iter()
            .find(|timer| timer.timer_id == timer_id)
            .map(|timer| timer.notification.as_str())
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
            TimerCommand::Status => self.show_user_timer_picker(),
            TimerCommand::Stop => {
                let stopped = self.user_timer.stop_all();
                let title = match stopped {
                    0 => "No timers are active.".to_string(),
                    1 => "Timer stopped.".to_string(),
                    _ => format!("{stopped} timers stopped."),
                };
                self.chat_widget.add_info_message(
                    title,
                    Some(
                        "A notification already delivered to a running turn is not interrupted."
                            .to_string(),
                    ),
                );
            }
            TimerCommand::Start {
                mode,
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

                if self.user_timer.active.len() >= MAX_ACTIVE_TIMERS {
                    self.chat_widget.add_error_message(format!(
                        "Cannot start more than {MAX_ACTIVE_TIMERS} timers. Cancel one with /timer and try again."
                    ));
                    return;
                }

                let timer_id = self.user_timer.next_timer_id();
                let app_event_tx = self.app_event_tx.clone();
                let task = match mode {
                    TimerMode::Once => tokio::spawn(async move {
                        tokio::time::sleep(interval).await;
                        app_event_tx.send(AppEvent::UserTimerTick { timer_id });
                    }),
                    TimerMode::Recurring => tokio::spawn(async move {
                        let start = Instant::now() + interval;
                        let mut ticker = tokio::time::interval_at(start, interval);
                        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
                        loop {
                            ticker.tick().await;
                            app_event_tx.send(AppEvent::UserTimerTick { timer_id });
                        }
                    }),
                };
                self.user_timer.active.push(ActiveUserTimer {
                    timer_id,
                    thread_id,
                    mode,
                    interval,
                    notification: notification.clone(),
                    delivered: 0,
                    skipped: 0,
                    last_delivery_error: None,
                    task,
                });
                let title = match mode {
                    TimerMode::Recurring => {
                        format!(
                            "Timer #{timer_id} set for every {}.",
                            display_duration(interval)
                        )
                    }
                    TimerMode::Once => {
                        format!(
                            "One-time timer #{timer_id} set for {}.",
                            display_duration(interval)
                        )
                    }
                };
                self.chat_widget.add_info_message(
                    title,
                    Some(format!(
                        "Main agent notification: {notification}\nBusy turns are steered immediately; idle threads start a new turn. Ticks are not queued.\nUse /timer to manage timers or /timer stop to stop all."
                    )),
                );
            }
        }
    }

    fn show_user_timer_picker(&mut self) {
        if self.user_timer.active.is_empty() {
            self.chat_widget.add_info_message(
                "No timers are active.".to_string(),
                Some(TIMER_USAGE.to_string()),
            );
            return;
        }

        let timer_ids = self
            .user_timer
            .active
            .iter()
            .map(|timer| timer.timer_id)
            .collect::<Vec<_>>();
        let items = self
            .user_timer
            .active
            .iter()
            .map(|timer| {
                let timer_id = timer.timer_id;
                let schedule = match timer.mode {
                    TimerMode::Recurring => {
                        format!("Every {}", display_duration(timer.interval))
                    }
                    TimerMode::Once => {
                        format!("Once after {}", display_duration(timer.interval))
                    }
                };
                let mut description = format!(
                    "{} · {} delivered · {} skipped",
                    timer.notification, timer.delivered, timer.skipped
                );
                if let Some(error) = &timer.last_delivery_error {
                    description.push_str(&format!(" · last delivery error: {error}"));
                }
                let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                    tx.send(AppEvent::OpenUserTimerActions { timer_id });
                })];
                SelectionItem {
                    name: format!("Timer #{timer_id} · {schedule}"),
                    description: Some(description),
                    actions,
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();
        let count = self.user_timer.active.len();
        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some("Manage Timers".to_string()),
            subtitle: Some(format!(
                "{count} active. Enter manages the selected timer; x stops it."
            )),
            footer_hint: Some("↑↓ select · enter manage · x stop · esc close".dim().into()),
            items,
            on_key: Some(Box::new(move |key_event, selected_idx, tx| {
                if key_event.code == crossterm::event::KeyCode::Char('x')
                    && key_event.modifiers == crossterm::event::KeyModifiers::NONE
                    && let Some(timer_id) =
                        selected_idx.and_then(|index| timer_ids.get(index).copied())
                {
                    tx.send(AppEvent::CancelUserTimer { timer_id });
                    return true;
                }
                false
            })),
            ..Default::default()
        });
    }

    pub(super) fn show_user_timer_actions(&mut self, timer_id: u64) {
        let Some(timer) = self
            .user_timer
            .active
            .iter()
            .find(|timer| timer.timer_id == timer_id)
        else {
            self.chat_widget.add_info_message(
                format!("Timer #{timer_id} is no longer active."),
                /*hint*/ None,
            );
            return;
        };
        let notification = timer.notification.clone();
        let edit_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            tx.send(AppEvent::EditUserTimerNotification { timer_id });
        })];
        let stop_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            tx.send(AppEvent::CancelUserTimer { timer_id });
        })];
        self.chat_widget.show_selection_view(SelectionViewParams {
            title: Some(format!("Timer #{timer_id}")),
            subtitle: Some(notification),
            footer_hint: Some(standard_popup_hint_line()),
            initial_selected_idx: Some(0),
            items: vec![
                SelectionItem {
                    name: "Edit notification".to_string(),
                    description: Some("Change the message delivered on future ticks".to_string()),
                    actions: edit_actions,
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: "Stop timer".to_string(),
                    description: Some("Cancel this timer only".to_string()),
                    actions: stop_actions,
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
    }

    pub(super) fn edit_user_timer_notification(&mut self, timer_id: u64) {
        let Some(notification) = self
            .user_timer
            .active
            .iter()
            .find(|timer| timer.timer_id == timer_id)
            .map(|timer| timer.notification.clone())
        else {
            self.chat_widget.add_info_message(
                format!("Timer #{timer_id} is no longer active."),
                /*hint*/ None,
            );
            return;
        };
        self.chat_widget
            .show_user_timer_edit_prompt(timer_id, notification);
    }

    pub(super) fn update_user_timer_notification(&mut self, timer_id: u64, notification: String) {
        let notification = notification.trim().to_string();
        if notification.is_empty() {
            self.chat_widget
                .add_error_message("Timer notification cannot be empty.".to_string());
            return;
        }
        if notification.chars().count() > MAX_NOTIFICATION_CHARS {
            self.chat_widget.add_error_message(format!(
                "Timer notification must be at most {MAX_NOTIFICATION_CHARS} characters."
            ));
            return;
        }
        let Some(timer) = self
            .user_timer
            .active
            .iter_mut()
            .find(|timer| timer.timer_id == timer_id)
        else {
            self.chat_widget.add_info_message(
                format!("Timer #{timer_id} is no longer active."),
                /*hint*/ None,
            );
            return;
        };
        timer.notification = notification.clone();
        self.chat_widget.add_info_message(
            format!("Timer #{timer_id} updated."),
            Some(format!("Main agent notification: {notification}")),
        );
    }

    pub(super) fn cancel_user_timer(&mut self, timer_id: u64) {
        let stopped = self.user_timer.stop_timer(timer_id);
        let title = if stopped {
            format!("Timer #{timer_id} stopped.")
        } else {
            format!("Timer #{timer_id} is no longer active.")
        };
        self.chat_widget.add_info_message(
            title,
            Some(
                "A notification already delivered to a running turn is not interrupted."
                    .to_string(),
            ),
        );
    }

    pub(super) fn cancel_user_timer_if_primary_changed(&mut self, thread_id: ThreadId) {
        self.user_timer.cancel_if_target_changed(thread_id);
    }

    pub(super) fn cancel_user_timer_for_thread(&mut self, thread_id: ThreadId) {
        let timer_ids = self
            .user_timer
            .active
            .iter()
            .filter_map(|timer| (timer.thread_id == thread_id).then_some(timer.timer_id))
            .collect::<Vec<_>>();
        for timer_id in timer_ids {
            self.user_timer.stop_timer(timer_id);
        }
    }

    pub(super) async fn handle_user_timer_tick(
        &mut self,
        app_server: &mut AppServerSession,
        timer_id: u64,
    ) {
        Box::pin(self.handle_user_timer_tick_inner(app_server, timer_id)).await;
    }

    async fn handle_user_timer_tick_inner(
        &mut self,
        app_server: &mut AppServerSession,
        timer_id: u64,
    ) {
        let Some((thread_id, mode, notification)) = self
            .user_timer
            .active
            .iter()
            .find(|timer| timer.timer_id == timer_id)
            .map(|timer| (timer.thread_id, timer.mode, timer.notification.clone()))
        else {
            return;
        };
        if self.primary_thread_id != Some(thread_id) {
            self.user_timer.stop_all();
            return;
        }

        let primary_is_visible = self.active_thread_id == Some(thread_id)
            && self.chat_widget.thread_id() == Some(thread_id);
        let items = vec![UserInput::Text {
            text: notification.clone(),
            text_elements: Vec::new(),
        }];

        let steer_outcome = if let Some(turn_id) = self.active_turn_id_for_thread(thread_id).await {
            Box::pin(self.try_steer_user_timer_notification(
                app_server,
                thread_id,
                turn_id,
                items.clone(),
            ))
            .await
        } else {
            TimerSteerOutcome::StartTurn
        };

        let result = match steer_outcome {
            TimerSteerOutcome::Delivered => Ok(()),
            TimerSteerOutcome::Failed(error) => Err(error),
            TimerSteerOutcome::StartTurn => {
                Box::pin(self.start_user_timer_notification_turn(
                    app_server,
                    thread_id,
                    notification,
                    items,
                    primary_is_visible,
                ))
                .await
            }
        };
        match result {
            Ok(()) => self.record_user_timer_delivery_success(timer_id),
            Err(error) => self.record_user_timer_delivery_failure(
                timer_id,
                thread_id,
                error,
                primary_is_visible,
            ),
        }
        if mode == TimerMode::Once {
            self.user_timer.stop_timer(timer_id);
        }
    }

    async fn try_steer_user_timer_notification(
        &mut self,
        app_server: &mut AppServerSession,
        thread_id: ThreadId,
        mut steer_turn_id: String,
        items: Vec<UserInput>,
    ) -> TimerSteerOutcome {
        let mut retried_after_turn_mismatch = false;
        loop {
            match app_server
                .turn_steer(thread_id, steer_turn_id.clone(), items.clone())
                .await
            {
                Ok(_) => return TimerSteerOutcome::Delivered,
                Err(error) => {
                    if let Some(turn_error) = active_turn_not_steerable_turn_error(&error) {
                        return TimerSteerOutcome::Failed(turn_error.message);
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
                            return TimerSteerOutcome::Failed(format!("{error:#}"));
                        }
                        None => return TimerSteerOutcome::Failed(format!("{error:#}")),
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
    ) -> Result<(), String> {
        let Some(session) = self
            .primary_session_configured
            .as_ref()
            .filter(|session| session.thread_id == thread_id)
            .cloned()
        else {
            return Err("main thread configuration is unavailable".to_string());
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
        .await
        .map_err(|error| format!("{error:#}"))?;
        let turn_id = response.turn.id;
        let store = Arc::clone(&self.ensure_thread_channel(thread_id).store);
        store.lock().await.active_turn_id = Some(turn_id.clone());
        if primary_is_visible {
            self.chat_widget
                .note_user_timer_turn_started(turn_id, &turn, notification);
        }
        Ok(())
    }

    fn record_user_timer_delivery_success(&mut self, timer_id: u64) {
        if let Some(timer) = self
            .user_timer
            .active
            .iter_mut()
            .find(|timer| timer.timer_id == timer_id)
        {
            timer.delivered = timer.delivered.saturating_add(1);
            timer.last_delivery_error = None;
        }
    }

    fn record_user_timer_delivery_failure(
        &mut self,
        timer_id: u64,
        thread_id: ThreadId,
        error: String,
        primary_is_visible: bool,
    ) {
        let changed = self
            .user_timer
            .active
            .iter()
            .find(|timer| timer.timer_id == timer_id)
            .is_some_and(|timer| timer.last_delivery_error.as_deref() != Some(&error));
        if let Some(timer) = self
            .user_timer
            .active
            .iter_mut()
            .find(|timer| timer.timer_id == timer_id)
        {
            timer.skipped = timer.skipped.saturating_add(1);
            timer.last_delivery_error = Some(error.clone());
        }
        tracing::warn!(thread_id = %thread_id, timer_id, error, "failed to deliver user timer notification");
        if changed && primary_is_visible {
            self.chat_widget.add_error_message(format!(
                "Timer #{timer_id} notification was skipped because immediate delivery failed: {error}"
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

    let (mode, duration_start) = match words.first() {
        Some(command) if command.eq_ignore_ascii_case("once") => (TimerMode::Once, 1),
        _ => (TimerMode::Recurring, 0),
    };
    let (interval, consumed) = parse_duration_prefix(&words[duration_start..])?;
    let notification = words[duration_start + consumed..].join(" ");
    if notification.is_empty() {
        return Err("A notification message is required.".to_string());
    }
    if notification.chars().count() > MAX_NOTIFICATION_CHARS {
        return Err(format!(
            "Notification is too long (maximum {MAX_NOTIFICATION_CHARS} characters)."
        ));
    }
    Ok(TimerCommand::Start {
        mode,
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
