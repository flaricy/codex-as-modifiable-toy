use super::*;

#[test]
fn parses_status_and_stop() {
    assert_eq!(parse_timer_command(""), Ok(TimerCommand::Status));
    assert_eq!(parse_timer_command("status"), Ok(TimerCommand::Status));
    assert_eq!(parse_timer_command("STOP"), Ok(TimerCommand::Stop));
}

#[test]
fn parses_compact_spaced_and_bare_second_durations() {
    assert_eq!(
        parse_timer_command("30min check training"),
        Ok(TimerCommand::Start {
            mode: TimerMode::Recurring,
            interval: Duration::from_secs(30 * 60),
            notification: "check training".to_string(),
        })
    );
    assert_eq!(
        parse_timer_command("2 hours inspect logs"),
        Ok(TimerCommand::Start {
            mode: TimerMode::Recurring,
            interval: Duration::from_secs(2 * 60 * 60),
            notification: "inspect logs".to_string(),
        })
    );
    assert_eq!(
        parse_timer_command("45 inspect logs"),
        Ok(TimerCommand::Start {
            mode: TimerMode::Recurring,
            interval: Duration::from_secs(45),
            notification: "inspect logs".to_string(),
        })
    );
}

#[test]
fn parses_one_time_timer() {
    assert_eq!(
        parse_timer_command("once 30min check training"),
        Ok(TimerCommand::Start {
            mode: TimerMode::Once,
            interval: Duration::from_secs(30 * 60),
            notification: "check training".to_string(),
        })
    );
}

#[test]
fn rejects_invalid_or_incomplete_commands() {
    assert!(parse_timer_command("0s wake").is_err());
    assert!(parse_timer_command("1week wake").is_err());
    assert!(parse_timer_command("30min").is_err());
    assert!(parse_timer_command("once").is_err());
    assert!(parse_timer_command("once 30min").is_err());
    assert!(parse_timer_command("stop now").is_err());
}

#[tokio::test]
async fn stale_ticks_are_ignored_and_queue_is_bounded() {
    let mut state = UserTimerState::default();
    let timer_id = state.next_timer_id();
    let task = tokio::spawn(std::future::pending::<()>());
    state.active.push(ActiveUserTimer {
        timer_id,
        thread_id: ThreadId::new(),
        mode: TimerMode::Recurring,
        interval: Duration::from_secs(1),
        notification: "wake".to_string(),
        dropped: 0,
        last_delivery_error: None,
        task,
    });

    assert!(!state.enqueue_tick(timer_id.wrapping_add(1)));
    for _ in 0..MAX_PENDING_NOTIFICATIONS {
        assert!(state.enqueue_tick(timer_id));
    }
    assert!(!state.enqueue_tick(timer_id));
    assert_eq!(state.pending.len(), MAX_PENDING_NOTIFICATIONS);
    assert_eq!(state.active[0].dropped, 1);
}

#[tokio::test]
async fn stopping_one_timer_keeps_other_timers_and_their_notifications() {
    let mut state = UserTimerState::default();
    let first_timer_id = state.next_timer_id();
    state.active.push(ActiveUserTimer {
        timer_id: first_timer_id,
        thread_id: ThreadId::new(),
        mode: TimerMode::Recurring,
        interval: Duration::from_secs(1),
        notification: "first".to_string(),
        dropped: 0,
        last_delivery_error: None,
        task: tokio::spawn(std::future::pending::<()>()),
    });
    let second_timer_id = state.next_timer_id();
    state.active.push(ActiveUserTimer {
        timer_id: second_timer_id,
        thread_id: ThreadId::new(),
        mode: TimerMode::Once,
        interval: Duration::from_secs(2),
        notification: "second".to_string(),
        dropped: 0,
        last_delivery_error: None,
        task: tokio::spawn(std::future::pending::<()>()),
    });
    assert!(state.enqueue_tick(first_timer_id));
    assert!(state.enqueue_tick(second_timer_id));

    assert!(state.stop_timer(first_timer_id));

    assert_eq!(state.active_timer_ids(), vec![second_timer_id]);
    assert_eq!(
        state.pending,
        VecDeque::from([PendingNotification {
            timer_id: second_timer_id,
            text: "second".to_string(),
        }])
    );
}
