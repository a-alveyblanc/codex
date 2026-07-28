use std::path::Path;
use std::sync::Arc;

use super::PendingDisplayMathRender;
use crate::history_cell::AgentMarkdownCell;

#[tokio::test]
async fn initial_replay_collects_math_jobs_in_one_batch() {
    let mut app = crate::app::test_support::make_test_app().await;
    app.begin_initial_history_replay_buffer();

    for source in ["$first$", "$second$"] {
        app.queue_display_math_render(PendingDisplayMathRender {
            cell: Arc::new(AgentMarkdownCell::new(
                source.to_string(),
                Path::new("/tmp"),
            )),
            job: crate::display_math::empty_job_for_test(),
        });
    }

    assert_eq!(
        app.initial_history_replay_buffer
            .as_ref()
            .expect("initial replay buffer")
            .display_math_jobs
            .len(),
        2,
    );
}
