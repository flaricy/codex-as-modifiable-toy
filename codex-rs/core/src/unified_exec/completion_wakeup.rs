use std::sync::Arc;
use std::sync::Weak;
use std::time::Duration;

use codex_utils_output_truncation::TruncationPolicy;
use codex_utils_output_truncation::formatted_truncate_text;
use tokio::sync::Mutex;

use super::process::OutputBuffer;
use crate::context::ContextualUserFragment;
use crate::context::ExecCommandCompletion;
use crate::session::session::Session;

const COMPLETION_COMMAND_MAX_TOKENS: usize = 500;
const COMPLETION_OUTPUT_MAX_TOKENS: usize = 2_000;

#[derive(Clone)]
pub(crate) struct CompletionWakeRegistration(Arc<CompletionWakeRegistrationInner>);

struct CompletionWakeRegistrationInner {
    state: Mutex<CompletionWakeState>,
    output_buffer: OutputBuffer,
    session: Weak<Session>,
    call_id: String,
    process_id: i32,
    command: String,
}

#[derive(Default)]
struct CompletionWakeState {
    armed: bool,
    enqueued: bool,
    completed: Option<(i32, Duration)>,
    observed: bool,
}

impl CompletionWakeRegistration {
    pub(crate) fn new(
        output_buffer: OutputBuffer,
        session: Weak<Session>,
        call_id: String,
        process_id: i32,
        command: String,
    ) -> Self {
        Self(Arc::new(CompletionWakeRegistrationInner {
            state: Mutex::new(CompletionWakeState::default()),
            output_buffer,
            session,
            call_id,
            process_id,
            command,
        }))
    }

    pub(crate) async fn arm(&self) {
        let completed = {
            let mut state = self.0.state.lock().await;
            if state.observed || state.enqueued {
                return;
            }
            state.armed = true;
            let completed = state.completed.take();
            state.enqueued = completed.is_some();
            completed
        };
        if let Some((exit_code, duration)) = completed {
            self.enqueue(exit_code, duration).await;
        }
    }

    pub(crate) async fn complete(&self, exit_code: i32, duration: Duration) {
        let should_enqueue = {
            let mut state = self.0.state.lock().await;
            if state.observed || state.enqueued || state.completed.is_some() {
                false
            } else if state.armed {
                state.enqueued = true;
                true
            } else {
                state.completed = Some((exit_code, duration));
                false
            }
        };
        if should_enqueue {
            self.enqueue(exit_code, duration).await;
        }
    }

    pub(crate) async fn observed(&self) {
        {
            let mut state = self.0.state.lock().await;
            state.observed = true;
            state.completed = None;
        }
        if let Some(session) = self.0.session.upgrade() {
            session
                .input_queue
                .remove_exec_wakeup(self.0.process_id)
                .await;
        }
    }

    async fn enqueue(&self, exit_code: i32, duration: Duration) {
        let Some(session) = self.0.session.upgrade() else {
            return;
        };
        let output = {
            let drained = self.0.output_buffer.lock().await.drain();
            let bytes = drained.to_bytes_with_omission_marker();
            let text = String::from_utf8_lossy(&bytes);
            formatted_truncate_text(
                &text,
                TruncationPolicy::Tokens(COMPLETION_OUTPUT_MAX_TOKENS),
            )
        };
        let command = formatted_truncate_text(
            &self.0.command,
            TruncationPolicy::Tokens(COMPLETION_COMMAND_MAX_TOKENS),
        );
        let fragment = ExecCommandCompletion::new(
            self.0.call_id.clone(),
            self.0.process_id,
            command,
            exit_code,
            duration,
            output,
        );
        session
            .input_queue
            .enqueue_exec_wakeup(self.0.process_id, ContextualUserFragment::into(fragment))
            .await;
        if self.0.state.lock().await.observed {
            session
                .input_queue
                .remove_exec_wakeup(self.0.process_id)
                .await;
            return;
        }
        session.maybe_start_turn_for_pending_work().await;
    }
}
