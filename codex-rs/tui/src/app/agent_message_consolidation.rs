//! Transcript consolidation for finalized streaming agent messages.
//!
//! During streaming, the chat widget emits transient `AgentMessageCell`s so it
//! can animate stable lines into scrollback while keeping the active mutable
//! tail in the bottom pane. Once the answer finishes, the app replaces that
//! trailing run with a single source-backed `AgentMarkdownCell`. This makes the
//! transcript the canonical owner of the raw markdown source used for future
//! resize re-renders.

use std::path::PathBuf;
use std::sync::Arc;

use super::App;
use super::PendingDisplayMathRender;
use super::resize_reflow::trailing_run_start;
use crate::app_event::ConsolidationScrollbackReflow;
use crate::history_cell;
use crate::history_cell::HistoryCell;
use crate::inline_visualization::InlineVisualizationContext;
use crate::pager_overlay::Overlay;
use crate::tui;
use color_eyre::eyre::Result;

impl App {
    pub(super) fn handle_consolidate_agent_message(
        &mut self,
        tui: &mut tui::Tui,
        source: String,
        cwd: PathBuf,
        inline_visualization_context: Option<InlineVisualizationContext>,
        scrollback_reflow: ConsolidationScrollbackReflow,
        deferred_history_cell: Option<Box<dyn HistoryCell>>,
    ) -> Result<()> {
        // Some finalize paths must preserve a last provisional stream cell long
        // enough for queue ordering, then fold it into the canonical
        // source-backed cell during consolidation.
        if let Some(cell) = deferred_history_cell {
            let cell: Arc<dyn HistoryCell> = cell.into();
            if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                t.insert_cell(cell.clone());
            }
            self.transcript_cells.push(cell);
        }

        // Walk backward to find the contiguous run of streaming AgentMessageCells that
        // belong to the just-finalized stream.
        let end = self.transcript_cells.len();
        tracing::debug!(
            "ConsolidateAgentMessage: transcript_cells.len()={end}, source_len={}",
            source.len()
        );
        let start = trailing_run_start::<history_cell::AgentMessageCell>(&self.transcript_cells);
        if start < end {
            let display_math_job = if self.config.tui_display_math {
                crate::display_math::job_for_source(
                    source.clone(),
                    &self.config.codex_home,
                    tui.display_math_cell_size(),
                )
            } else {
                None
            };
            tracing::debug!(
                "ConsolidateAgentMessage: replacing cells [{start}..{end}] with AgentMarkdownCell"
            );
            let consolidated = Arc::new(
                history_cell::AgentMarkdownCell::new_with_inline_visualizations(
                    source,
                    &cwd,
                    inline_visualization_context,
                ),
            );
            let consolidated_cell: Arc<dyn HistoryCell> = consolidated.clone();
            self.transcript_cells
                .splice(start..end, std::iter::once(consolidated_cell.clone()));

            if let Some(Overlay::Transcript(t)) = &mut self.overlay {
                t.consolidate_cells(start..end, consolidated_cell);
                tui.frame_requester().schedule_frame();
            }

            self.finish_agent_message_consolidation(tui, scrollback_reflow)?;
            if let Some(job) = display_math_job {
                self.queue_display_math_render(PendingDisplayMathRender {
                    cell: consolidated,
                    job,
                });
            }
        } else {
            tracing::debug!(
                "ConsolidateAgentMessage: no cells to consolidate(start={start}, end={end})",
            );
            self.maybe_finish_stream_reflow(tui)?;
        }

        Ok(())
    }

    fn queue_display_math_render(&mut self, pending: PendingDisplayMathRender) {
        if let Some(buffer) = self.initial_history_replay_buffer.as_mut() {
            buffer.display_math_jobs.push(pending);
            return;
        }
        self.spawn_display_math_render_batch(vec![pending]);
    }

    pub(super) fn spawn_display_math_render_batch(&self, pending: Vec<PendingDisplayMathRender>) {
        if pending.is_empty() {
            return;
        }
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            for pending in pending {
                tasks.spawn(async move { (pending.cell, pending.job.render().await) });
            }
            let mut renders = Vec::new();
            while let Some(result) = tasks.join_next().await {
                match result {
                    Ok(render) => renders.push(render),
                    Err(err) => {
                        tracing::warn!(error = %err, "display-math render task failed");
                    }
                }
            }
            app_event_tx.send(crate::app_event::AppEvent::DisplayMathRendered { renders });
        });
    }

    fn finish_agent_message_consolidation(
        &mut self,
        tui: &mut tui::Tui,
        scrollback_reflow: ConsolidationScrollbackReflow,
    ) -> Result<()> {
        match scrollback_reflow {
            ConsolidationScrollbackReflow::IfResizeReflowRan => {
                self.maybe_finish_stream_reflow(tui)?;
            }
            ConsolidationScrollbackReflow::Required => {
                self.finish_required_stream_reflow(tui)?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "agent_message_consolidation_tests.rs"]
mod tests;
