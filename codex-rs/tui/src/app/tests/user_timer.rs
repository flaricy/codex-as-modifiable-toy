use super::*;
use crate::app::session_lifecycle::ThreadAttachPresentation;
use codex_app_server_client::AppServerEvent;
use codex_utils_absolute_path::test_support::PathExt;
use core_test_support::responses;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use tokio::sync::oneshot;

fn gated_timer_response() -> (Vec<StreamingSseChunk>, oneshot::Sender<()>) {
    let (release_tx, release_rx) = oneshot::channel();
    (
        vec![
            StreamingSseChunk {
                gate: None,
                body: responses::sse(vec![ev_response_created("timer-active-response")]),
            },
            StreamingSseChunk {
                gate: Some(release_rx),
                body: responses::sse(vec![ev_completed("timer-active-response")]),
            },
        ],
        release_tx,
    )
}

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
    Busy turns are steered immediately; idle threads start a new turn. Ticks are not queued.
    Use /timer status or /timer stop.
    ");

    while app_event_rx.try_recv().is_ok() {}
    app.handle_user_timer_command("status".to_string());
    let status_cell = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => Some(cell),
            _ => None,
        })
        .expect("timer status should add a status cell");
    let status = lines_to_single_string(&status_cell.display_lines(/*width*/ 120));
    assert!(status.contains("steer while busy; start a new turn while idle; never queue"));
    assert!(status.contains("Delivered: 0; skipped: 0"));
    assert!(!status.contains("Queued:"));

    assert!(app.user_timer.stop());
}

#[test]
fn timer_ticks_are_delivered_immediately_without_a_local_queue() -> Result<()> {
    const TEST_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;

    std::thread::Builder::new()
        .name("tui-user-timer-immediate-delivery".to_string())
        .stack_size(TEST_STACK_SIZE_BYTES)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async {
                let (response_chunks, release_response) = gated_timer_response();
                let (server, _completions) =
                    start_streaming_sse_server(vec![response_chunks]).await;
                let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
                let codex_home = tempdir()?;
                app.config.codex_home = codex_home.path().to_path_buf().abs();
                app.config.sqlite =
                    codex_state::SqliteConfig::new_for_testing(codex_home.path().abs());
                app.config.model_provider = ModelProviderInfo {
                    name: "Timer steer test".to_string(),
                    base_url: Some(format!("{}/v1", server.uri())),
                    request_max_retries: Some(0),
                    stream_max_retries: Some(0),
                    ..ModelProviderInfo::default()
                };
                let mut tui = crate::tui::test_support::make_test_tui()?;
                let mut app_server =
                    Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
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
                let store = Arc::clone(&app.thread_event_channels[&thread_id].store);

                let AppServerRequestId::Integer(before_tick) = app_server.next_request_id() else {
                    unreachable!("embedded app-server request IDs are integers");
                };
                app.handle_user_timer_tick(&mut app_server, generation)
                    .await;
                let AppServerRequestId::Integer(after_first_tick) = app_server.next_request_id()
                else {
                    unreachable!("embedded app-server request IDs are integers");
                };
                assert!(after_first_tick >= before_tick + 2);
                std::assert_eq!(app.user_timer.delivery_counts(), (1, 0));
                let first_turn_id = loop {
                    let event =
                        tokio::time::timeout(Duration::from_secs(5), app_server.next_event())
                            .await
                            .expect("timer turn should start")
                            .expect("app-server event stream should remain open");
                    let started_turn_id = if let AppServerEvent::ServerNotification(notification) =
                        &event
                        && let ServerNotification::TurnStarted(notification) = notification.as_ref()
                        && notification.thread_id == thread_id.to_string()
                    {
                        Some(notification.turn.id.clone())
                    } else {
                        None
                    };
                    app.handle_app_server_event(&app_server, event).await;
                    if let Some(turn_id) = started_turn_id {
                        break turn_id;
                    }
                };

                app.handle_user_timer_tick(&mut app_server, generation)
                    .await;
                let AppServerRequestId::Integer(after_second_tick) = app_server.next_request_id()
                else {
                    unreachable!("embedded app-server request IDs are integers");
                };
                assert!(after_second_tick > after_first_tick);
                std::assert_eq!(app.user_timer.delivery_counts(), (2, 0));
                std::assert_eq!(
                    store.lock().await.active_turn_id.as_deref(),
                    Some(first_turn_id.as_str()),
                    "second tick should steer the existing turn",
                );

                app.user_timer.stop();
                let _ = release_response.send(());
                app_server.shutdown().await?;
                Ok(())
            })
        })?
        .join()
        .expect("timer immediate-delivery test thread")
}
