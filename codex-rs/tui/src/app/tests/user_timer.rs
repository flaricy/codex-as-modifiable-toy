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
    • Timer #1 set for every 30min. Main agent notification: check training progress
    Busy turns are steered immediately; idle threads start a new turn. Ticks are not queued.
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
    Busy turns are steered immediately; idle threads start a new turn. Ticks are not queued.
    Use /timer to manage timers or /timer stop to stop all.
    ");

    std::assert_eq!(app.user_timer.stop_all(), 1);
}

#[tokio::test]
async fn timer_picker_lists_all_timers_and_supports_manage_and_direct_stop() {
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
      2 active. Enter manages the selected timer; x stops it.

    › 1. Timer #1 · Every 30min    check training progress · 0 delivered · 0
                                   skipped
      2. Timer #2 · Once after 2h  inspect logs · 0 delivered · 0 skipped

      ↑↓ select · enter manage · x stop · esc close
    ");

    app.chat_widget
        .handle_key_event(KeyEvent::from(KeyCode::Enter));
    let timer_id = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::OpenUserTimerActions { timer_id } => Some(timer_id),
            _ => None,
        })
        .expect("selecting a timer should open its actions");
    app.show_user_timer_actions(timer_id);
    let actions_popup = render_bottom_popup(&app.chat_widget, /*width*/ 80);
    assert_snapshot!(actions_popup, @r"
      Timer #1
      check training progress

    › 1. Edit notification  Change the message delivered on future ticks
      2. Stop timer         Cancel this timer only

      Press enter to confirm or esc to go back
    ");

    app.chat_widget
        .handle_key_event(KeyEvent::from(KeyCode::Esc));
    app.handle_user_timer_command(String::new());
    app.chat_widget
        .handle_key_event(KeyEvent::from(KeyCode::Down));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    let timer_id = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::CancelUserTimer { timer_id } => Some(timer_id),
            _ => None,
        })
        .expect("x should stop the selected timer");
    app.cancel_user_timer(timer_id);

    std::assert_eq!(app.user_timer.active_timer_ids(), vec![1]);
    std::assert_eq!(app.user_timer.stop_all(), 1);
}

#[tokio::test]
async fn updating_one_timer_notification_keeps_other_timers_unchanged() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    app.primary_thread_id = Some(thread_id);
    app.primary_session_configured =
        Some(test_thread_session(thread_id, app.config.cwd.to_path_buf()));

    app.handle_user_timer_command("30min first".to_string());
    app.handle_user_timer_command("1h second".to_string());
    app.update_user_timer_notification(1, " updated first ".to_string());

    std::assert_eq!(app.user_timer.notification(1), Some("updated first"));
    std::assert_eq!(app.user_timer.notification(2), Some("second"));
    std::assert_eq!(app.user_timer.stop_all(), 2);
}

#[test]
fn timer_ticks_start_when_idle_and_steer_the_active_turn_without_a_local_queue() -> Result<()> {
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
                let timer_id = app
                    .user_timer
                    .active_timer_ids()
                    .into_iter()
                    .next()
                    .expect("timer should be active");
                let store = Arc::clone(&app.thread_event_channels[&thread_id].store);

                app.handle_user_timer_tick(&mut app_server, timer_id).await;
                std::assert_eq!(app.user_timer.delivery_counts(timer_id), Some((1, 0)));
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

                app.handle_user_timer_tick(&mut app_server, timer_id).await;
                std::assert_eq!(app.user_timer.delivery_counts(timer_id), Some((2, 0)));
                std::assert_eq!(
                    store.lock().await.active_turn_id.as_deref(),
                    Some(first_turn_id.as_str()),
                    "second tick should steer the existing turn",
                );

                app.user_timer.stop_all();
                let _ = release_response.send(());
                app_server.shutdown().await?;
                Ok(())
            })
        })?
        .join()
        .expect("timer immediate-delivery test thread")
}

#[test]
fn one_time_timer_becomes_inactive_after_starting_notification_turn() -> Result<()> {
    const TEST_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;

    std::thread::Builder::new()
        .name("tui-one-time-user-timer".to_string())
        .stack_size(TEST_STACK_SIZE_BYTES)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            runtime.block_on(async {
                let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
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

                app.handle_user_timer_command("once 1d inspect training".to_string());
                let timer_id = app
                    .user_timer
                    .active_timer_ids()
                    .into_iter()
                    .next()
                    .expect("timer should be active");

                app.handle_user_timer_tick(&mut app_server, timer_id).await;

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
            })
        })?
        .join()
        .expect("one-time timer test thread")
}
