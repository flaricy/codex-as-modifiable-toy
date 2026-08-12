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
async fn stopping_timer_invalidates_generation_and_resets_delivery_status() {
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
    state.delivered = 3;
    state.skipped = 2;
    state.last_delivery_error = Some("not steerable".to_string());

    assert!(state.stop());
    assert_ne!(state.generation, generation);
    assert!(state.active.is_none());
    assert_eq!(state.delivery_counts(), (0, 0));
    assert!(state.last_delivery_error.is_none());
}
