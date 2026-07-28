//! Transcript consolidation for finalized streaming agent messages.
//!
//! During streaming, the chat widget emits transient `AgentMessageCell`s so it
//! can animate stable lines into scrollback while keeping the active mutable
//! tail in the bottom pane. Once the answer finishes, the app replaces that
//! trailing run with a single source-backed `AgentMarkdownCell`. This makes the
//! transcript the canonical owner of the raw markdown source used for future
//! resize re-renders.

use std::collections::HashSet;
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

const DISPLAY_MATH_RENDER_CONCURRENCY: usize = 4;

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
            if self.config.tui_display_math {
                self.queue_display_math_render(tui, consolidated);
            }
        } else {
            tracing::debug!(
                "ConsolidateAgentMessage: no cells to consolidate(start={start}, end={end})",
            );
            self.maybe_finish_stream_reflow(tui)?;
        }

        Ok(())
    }

    fn queue_display_math_render(
        &mut self,
        tui: &mut tui::Tui,
        cell: Arc<history_cell::AgentMarkdownCell>,
    ) {
        if !cell.display_math_source().contains('$') {
            return;
        }
        if let Some(buffer) = self.initial_history_replay_buffer.as_mut() {
            buffer.display_math_cells.push(cell);
            return;
        }
        self.spawn_display_math_renders_for_cells(tui, vec![cell]);
    }

    pub(super) fn spawn_display_math_renders_for_cells(
        &self,
        tui: &mut tui::Tui,
        cells: Vec<Arc<history_cell::AgentMarkdownCell>>,
    ) {
        if cells.is_empty() {
            return;
        }
        let cell_size = tui.display_math_cell_size();
        let pending = cells
            .into_iter()
            .filter_map(|cell| {
                crate::display_math::job_for_source(
                    cell.display_math_source(),
                    &self.config.codex_home,
                    cell_size,
                )
                .map(|job| PendingDisplayMathRender { cell, job })
            })
            .collect();
        self.spawn_display_math_render_batch(pending);
    }

    /// Render equations that were intentionally skipped outside the retained startup tail.
    ///
    /// The cells already own the original Markdown, so retaining them costs only one additional
    /// `Arc` each. Rebuilding jobs here avoids storing a second copy of every parsed formula for the
    /// lifetime of a large resumed thread.
    pub(crate) fn spawn_deferred_display_math_renders(&mut self, tui: &mut tui::Tui) {
        if !self.config.tui_display_math || self.deferred_display_math_cells.is_empty() {
            self.deferred_display_math_cells.clear();
            return;
        }

        let transcript_cells = self
            .transcript_cells
            .iter()
            .map(|cell| Arc::as_ptr(cell) as *const ())
            .collect::<HashSet<_>>();
        let cells = std::mem::take(&mut self.deferred_display_math_cells)
            .into_iter()
            .filter(|cell| transcript_cells.contains(&(Arc::as_ptr(cell) as *const ())))
            .collect();
        self.spawn_display_math_renders_for_cells(tui, cells);
    }

    pub(super) fn spawn_display_math_render_batch(&self, pending: Vec<PendingDisplayMathRender>) {
        if pending.is_empty() {
            return;
        }
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            let mut pending = pending.into_iter();
            for pending in pending.by_ref().take(DISPLAY_MATH_RENDER_CONCURRENCY) {
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
                if let Some(pending) = pending.next() {
                    tasks.spawn(async move { (pending.cell, pending.job.render().await) });
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
