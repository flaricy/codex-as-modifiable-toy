use super::*;
use crate::app::session_lifecycle::ThreadAttachPresentation;

#[tokio::test]
async fn timer_start_has_a_clear_user_visible_status() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    app.primary_thread_id = Some(thread_id);
    app.primary_session_configured =
        Some(test_thread_session(thread_id, app.config.cwd.to_path_buf()));
    while app_event_rx.try_recv().is_ok() {}

    app.handle_user_timer_command("30min check training progress".to_string());

    let cell = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => Some(cell),
            _ => None,
        })
        .expect("timer start should add a status cell");
    let rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert_snapshot!(rendered, @r"
    • Timer #1 set for every 30min. Main agent notification: check training progress
    Use /timer to manage timers or /timer stop to stop all.
    ");

    std::assert_eq!(app.user_timer.stop_all(), 1);
}

#[tokio::test]
async fn one_time_timer_start_has_a_clear_user_visible_status() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    app.primary_thread_id = Some(thread_id);
    app.primary_session_configured =
        Some(test_thread_session(thread_id, app.config.cwd.to_path_buf()));
    while app_event_rx.try_recv().is_ok() {}

    app.handle_user_timer_command("once 30min check training progress".to_string());

    let cell = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => Some(cell),
            _ => None,
        })
        .expect("timer start should add a status cell");
    let rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert_snapshot!(rendered, @r"
    • One-time timer #1 set for 30min. Main agent notification: check training progress
    Use /timer to manage timers or /timer stop to stop all.
    ");

    std::assert_eq!(app.user_timer.stop_all(), 1);
}

#[tokio::test]
async fn timer_picker_lists_all_timers_and_selecting_one_cancels_only_that_timer() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    app.primary_thread_id = Some(thread_id);
    app.primary_session_configured =
        Some(test_thread_session(thread_id, app.config.cwd.to_path_buf()));
    while app_event_rx.try_recv().is_ok() {}

    app.handle_user_timer_command("30min check training progress".to_string());
    app.handle_user_timer_command("once 2h inspect logs".to_string());
    std::assert_eq!(app.user_timer.active_timer_ids(), vec![1, 2]);
    while app_event_rx.try_recv().is_ok() {}

    app.handle_user_timer_command(String::new());

    let popup = render_bottom_popup(&app.chat_widget, /*width*/ 80);
    assert_snapshot!(popup, @r"
      Manage Timers
      2 active. Select a timer to cancel it.

    › 1. Timer #1 · Every 30min    check training progress
      2. Timer #2 · Once after 2h  inspect logs

      Press enter to confirm or esc to go back
    ");

    app.chat_widget
        .handle_key_event(KeyEvent::from(KeyCode::Enter));
    let timer_id = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::CancelUserTimer { timer_id } => Some(timer_id),
            _ => None,
        })
        .expect("selecting a timer should request cancellation");
    app.cancel_user_timer(timer_id);

    std::assert_eq!(app.user_timer.active_timer_ids(), vec![2]);
    std::assert_eq!(app.user_timer.stop_all(), 1);
}

#[tokio::test]
async fn busy_main_thread_keeps_notification_queued_then_idle_starts_new_turn() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let started = app_server.start_thread(&app.config).await?;
    let thread_id = started.session.thread_id;
    app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        started,
        ThreadAttachPresentation::SessionLineage,
        /*initial_user_message*/ None,
    )
    .await?;
    while app_event_rx.try_recv().is_ok() {}
    app.clear_active_thread().await;

    app.handle_user_timer_command("1d inspect training".to_string());
    let timer_id = app
        .user_timer
        .active_timer_ids()
        .into_iter()
        .next()
        .expect("timer should be active");
    assert!(app.user_timer.enqueue_tick(timer_id));
    let store = Arc::clone(&app.thread_event_channels[&thread_id].store);
    store.lock().await.active_turn_id = Some("busy-turn".to_string());

    let AppServerRequestId::Integer(before_busy) = app_server.next_request_id() else {
        unreachable!("embedded app-server request IDs are integers");
    };
    app.maybe_start_next_user_timer_notification(&mut app_server)
        .await;
    let AppServerRequestId::Integer(after_busy) = app_server.next_request_id() else {
        unreachable!("embedded app-server request IDs are integers");
    };
    std::assert_eq!(after_busy, before_busy + 1);
    std::assert_eq!(app.user_timer.pending_len(), 1);
    std::assert_eq!(
        store.lock().await.active_turn_id.as_deref(),
        Some("busy-turn")
    );

    store.lock().await.active_turn_id = None;
    app.maybe_start_next_user_timer_notification(&mut app_server)
        .await;
    let AppServerRequestId::Integer(after_idle) = app_server.next_request_id() else {
        unreachable!("embedded app-server request IDs are integers");
    };
    std::assert_eq!(after_idle, after_busy + 2);
    std::assert_eq!(app.user_timer.pending_len(), 0);
    assert!(store.lock().await.active_turn_id.is_some());

    app.user_timer.stop_all();
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn one_time_timer_becomes_inactive_after_starting_notification_turn() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let started = app_server.start_thread(&app.config).await?;
    let thread_id = started.session.thread_id;
    app.replace_chat_widget_with_app_server_thread(
        &mut tui,
        started,
        ThreadAttachPresentation::SessionLineage,
        /*initial_user_message*/ None,
    )
    .await?;
    while app_event_rx.try_recv().is_ok() {}
    app.clear_active_thread().await;

    app.handle_user_timer_command("once 1d inspect training".to_string());
    let timer_id = app
        .user_timer
        .active_timer_ids()
        .into_iter()
        .next()
        .expect("timer should be active");
    assert!(app.user_timer.enqueue_tick(timer_id));

    app.maybe_start_next_user_timer_notification(&mut app_server)
        .await;

    std::assert_eq!(app.user_timer.pending_len(), 0);
    assert!(app.user_timer.active_timer_ids().is_empty());
    assert!(
        app.thread_event_channels[&thread_id]
            .store
            .lock()
            .await
            .active_turn_id
            .is_some()
    );

    app_server.shutdown().await?;
    Ok(())
}
