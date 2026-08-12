//! User-only timer notification editor.

use super::*;

impl ChatWidget {
    pub(crate) fn show_user_timer_edit_prompt(&mut self, timer_id: u64, notification: String) {
        let tx = self.app_event_tx.clone();
        let view = CustomPromptView::new(
            format!("Edit timer #{timer_id}"),
            "Type the notification and press Enter".to_string(),
            notification,
            /*context_label*/ None,
            Box::new(move |notification: String| {
                tx.send(AppEvent::UpdateUserTimerNotification {
                    timer_id,
                    notification,
                });
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
    }
}
