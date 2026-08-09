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
    • Timer set for every 30min. Main agent notification: check training progress
    Use /timer status or /timer stop.
    ");

    assert!(app.user_timer.stop());
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
    let generation = app
        .user_timer
        .active_generation()
        .expect("timer should be active");
    assert!(app.user_timer.enqueue_tick(generation));
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

    app.user_timer.stop();
    app_server.shutdown().await?;
    Ok(())
}
