use std::path::Path;
use std::sync::Arc;

use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::HistoryCell;
use crate::legacy_core::config::TerminalResizeReflowMaxRows;

#[tokio::test]
async fn initial_replay_collects_math_cells_without_building_jobs() {
    let mut app = crate::app::test_support::make_test_app().await;
    let mut tui = crate::tui::test_support::make_test_tui().expect("test TUI");
    app.begin_initial_history_replay_buffer();

    for source in ["$first$", "$second$"] {
        app.queue_display_math_render(
            &mut tui,
            Arc::new(AgentMarkdownCell::new(
                source.to_string(),
                Path::new("/tmp"),
            )),
        );
    }
    app.queue_display_math_render(
        &mut tui,
        Arc::new(AgentMarkdownCell::new(
            "plain markdown".to_string(),
            Path::new("/tmp"),
        )),
    );

    assert_eq!(
        app.initial_history_replay_buffer
            .as_ref()
            .expect("initial replay buffer")
            .display_math_cells
            .len(),
        2,
    );
}

#[tokio::test]
async fn capped_initial_replay_only_starts_math_in_retained_tail() {
    let mut app = crate::app::test_support::make_test_app().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(2);
    let cells = (0..6)
        .map(|index| {
            Arc::new(AgentMarkdownCell::new(
                format!("cell {index}"),
                Path::new("/tmp"),
            ))
        })
        .collect::<Vec<_>>();
    app.transcript_cells = cells
        .iter()
        .map(|cell| {
            let cell: Arc<dyn HistoryCell> = cell.clone();
            cell
        })
        .collect();

    let immediate =
        app.partition_initial_replay_display_math_cells(cells.clone(), /*width*/ 80);

    assert_eq!(immediate.len(), 3);
    assert!(
        immediate
            .iter()
            .zip(&cells[3..])
            .all(|(immediate, cell)| Arc::ptr_eq(immediate, cell))
    );
    assert_eq!(app.deferred_display_math_cells.len(), 3);
    assert!(
        app.deferred_display_math_cells
            .iter()
            .zip(&cells[..3])
            .all(|(deferred, cell)| Arc::ptr_eq(deferred, cell))
    );
}

#[tokio::test]
async fn uncapped_initial_replay_starts_all_math_immediately() {
    let mut app = crate::app::test_support::make_test_app().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Disabled;
    let cells = (0..3)
        .map(|index| {
            Arc::new(AgentMarkdownCell::new(
                format!("cell {index}"),
                Path::new("/tmp"),
            ))
        })
        .collect::<Vec<_>>();
    app.transcript_cells = cells
        .iter()
        .map(|cell| {
            let cell: Arc<dyn HistoryCell> = cell.clone();
            cell
        })
        .collect();

    let immediate =
        app.partition_initial_replay_display_math_cells(cells.clone(), /*width*/ 80);

    assert_eq!(immediate.len(), cells.len());
    assert!(app.deferred_display_math_cells.is_empty());
}
