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
async fn delivery_counts_are_tracked_per_timer() {
    let mut state = UserTimerState::default();
    let timer_id = state.next_timer_id();
    state.active.push(ActiveUserTimer {
        timer_id,
        thread_id: ThreadId::new(),
        mode: TimerMode::Recurring,
        interval: Duration::from_secs(1),
        notification: "wake".to_string(),
        delivered: 3,
        skipped: 2,
        last_delivery_error: Some("not steerable".to_string()),
        task: tokio::spawn(std::future::pending::<()>()),
    });

    assert_eq!(state.delivery_counts(timer_id), Some((3, 2)));
    assert_eq!(state.delivery_counts(timer_id.wrapping_add(1)), None);
}

#[tokio::test]
async fn stopping_one_timer_keeps_other_timers_and_their_delivery_status() {
    let mut state = UserTimerState::default();
    let first_timer_id = state.next_timer_id();
    state.active.push(ActiveUserTimer {
        timer_id: first_timer_id,
        thread_id: ThreadId::new(),
        mode: TimerMode::Recurring,
        interval: Duration::from_secs(1),
        notification: "first".to_string(),
        delivered: 1,
        skipped: 0,
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
        delivered: 4,
        skipped: 2,
        last_delivery_error: Some("busy".to_string()),
        task: tokio::spawn(std::future::pending::<()>()),
    });

    assert!(state.stop_timer(first_timer_id));

    assert_eq!(state.active_timer_ids(), vec![second_timer_id]);
    assert_eq!(state.delivery_counts(second_timer_id), Some((4, 2)));
    assert_eq!(state.active[0].last_delivery_error.as_deref(), Some("busy"));
}
