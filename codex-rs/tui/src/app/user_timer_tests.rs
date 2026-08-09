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
            interval: Duration::from_secs(30 * 60),
            notification: "check training".to_string(),
        })
    );
    assert_eq!(
        parse_timer_command("2 hours inspect logs"),
        Ok(TimerCommand::Start {
            interval: Duration::from_secs(2 * 60 * 60),
            notification: "inspect logs".to_string(),
        })
    );
    assert_eq!(
        parse_timer_command("45 inspect logs"),
        Ok(TimerCommand::Start {
            interval: Duration::from_secs(45),
            notification: "inspect logs".to_string(),
        })
    );
}

#[test]
fn rejects_invalid_or_incomplete_commands() {
    assert!(parse_timer_command("0s wake").is_err());
    assert!(parse_timer_command("1week wake").is_err());
    assert!(parse_timer_command("30min").is_err());
    assert!(parse_timer_command("stop now").is_err());
}

#[tokio::test]
async fn stale_ticks_are_ignored_and_queue_is_bounded() {
    let mut state = UserTimerState::default();
    let generation = state.next_generation();
    let task = tokio::spawn(std::future::pending::<()>());
    state.active = Some(ActiveUserTimer {
        generation,
        thread_id: ThreadId::new(),
        interval: Duration::from_secs(1),
        notification: "wake".to_string(),
        task,
    });

    assert!(!state.enqueue_tick(generation.wrapping_add(1)));
    for _ in 0..MAX_PENDING_NOTIFICATIONS {
        assert!(state.enqueue_tick(generation));
    }
    assert!(!state.enqueue_tick(generation));
    assert_eq!(state.pending.len(), MAX_PENDING_NOTIFICATIONS);
    assert_eq!(state.dropped, 1);
}
