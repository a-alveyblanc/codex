//! Finalized Markdown math rendering for Kitty-compatible terminals.
//!
//! Streaming output remains ordinary Markdown. Once an assistant message is finalized, top-level
//! `$$...$$` blocks and inline `$...$` spans are rendered asynchronously and replaced with Kitty
//! Unicode placeholder cells. The original source remains on the history cell for raw output,
//! copying, and graceful fallback.

use std::fmt;
use std::ops::Range;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose;
use codex_terminal_detection::Multiplexer;
use codex_terminal_detection::TerminalInfo;
use codex_terminal_detection::TerminalName;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use sha2::Digest;
use sha2::Sha256;

use crate::terminal_hyperlinks::HyperlinkLine;

mod parser;

use parser::parse_math;

const MAX_PLACEHOLDER_ROWS: u16 = 32;
const PLACEHOLDER: char = '\u{10eeee}';
const KITTY_CHUNK_SIZE: usize = 4096;
const NEOVIM_BRIDGE_ENV: &str = "CODEX_NEOVIM_KITTY_BRIDGE";
const NEOVIM_OUTER_TMUX_ENV: &str = "CODEX_NEOVIM_OUTER_TMUX";

// The first entries from Kitty's canonical row/column diacritic table. We cap rendered formulas at
// 32 rows, and use explicit row/column metadata on the first cell of each row.
const DIACRITICS: [char; 32] = [
    '\u{0305}', '\u{030d}', '\u{030e}', '\u{0310}', '\u{0312}', '\u{033d}', '\u{033e}', '\u{033f}',
    '\u{0346}', '\u{034a}', '\u{034b}', '\u{034c}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035b}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036a}', '\u{036b}', '\u{036c}', '\u{036d}', '\u{036e}', '\u{036f}', '\u{0483}', '\u{0484}',
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CellSizePixels {
    pub(crate) width: u16,
    pub(crate) height: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MathStyle {
    Display,
    Inline,
}

impl Default for CellSizePixels {
    fn default() -> Self {
        Self {
            width: 8,
            height: 16,
        }
    }
}

#[derive(Clone)]
pub(crate) struct TerminalImage {
    pub(crate) image_id: u32,
    pub(crate) columns: u16,
    pub(crate) rows: u16,
    pub(crate) png: Arc<[u8]>,
    fingerprint: [u8; 32],
}

impl fmt::Debug for TerminalImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalImage")
            .field("image_id", &self.image_id)
            .field("columns", &self.columns)
            .field("rows", &self.rows)
            .field("png_bytes", &self.png.len())
            .finish()
    }
}

impl TerminalImage {
    pub(crate) fn same_content(&self, other: &Self) -> bool {
        self.fingerprint == other.fingerprint
            && self.columns == other.columns
            && self.rows == other.rows
    }
}

#[derive(Clone)]
pub(crate) struct RenderedMath {
    pub(crate) png: Arc<[u8]>,
    pub(crate) width_px: u32,
    pub(crate) height_px: u32,
    pub(crate) fingerprint: [u8; 32],
}

impl fmt::Debug for RenderedMath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RenderedMath")
            .field("png_bytes", &self.png.len())
            .field("width_px", &self.width_px)
            .field("height_px", &self.height_px)
            .finish()
    }
}

#[derive(Clone, Debug)]
struct ParsedBlock {
    range: Range<usize>,
    formula: String,
    style: MathStyle,
}

#[derive(Clone, Debug)]
struct PreparedBlock {
    range: Range<usize>,
    style: MathStyle,
    rendered: Option<RenderedMath>,
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedDisplayMath {
    blocks: Vec<PreparedBlock>,
    cell_size: CellSizePixels,
}

#[derive(Debug)]
pub(crate) struct DisplayMathJob {
    blocks: Vec<ParsedBlock>,
    cache_dir: PathBuf,
    foreground: (u8, u8, u8),
    background: (u8, u8, u8),
    cell_size: CellSizePixels,
}

impl DisplayMathJob {
    pub(crate) async fn render(self) -> PreparedDisplayMath {
        let mut prepared = Vec::with_capacity(self.blocks.len());
        for block in self.blocks {
            let rendered = match crate::display_math_renderer::render_formula(
                &block.formula,
                block.style,
                &self.cache_dir,
                self.foreground,
                self.background,
            )
            .await
            {
                Ok(rendered) => Some(rendered),
                Err(err) => {
                    tracing::warn!(error = %err, "failed to render display math; keeping source");
                    None
                }
            };
            prepared.push(PreparedBlock {
                range: block.range,
                style: block.style,
                rendered,
            });
        }
        PreparedDisplayMath {
            blocks: prepared,
            cell_size: self.cell_size,
        }
    }
}

pub(crate) fn job_for_source(
    source: &str,
    codex_home: &Path,
    cell_size: CellSizePixels,
) -> Option<DisplayMathJob> {
    if !terminal_supports_display_math(&codex_terminal_detection::terminal_info()) {
        return None;
    }
    let blocks = parse_math(source);
    if blocks.is_empty() {
        return None;
    }
    Some(DisplayMathJob {
        blocks,
        cache_dir: codex_home.join("cache").join("tui-latex"),
        foreground: crate::terminal_palette::default_fg().unwrap_or((235, 235, 235)),
        background: crate::terminal_palette::default_bg().unwrap_or((0, 0, 0)),
        cell_size,
    })
}

fn terminal_supports_display_math(info: &TerminalInfo) -> bool {
    if matches!(info.multiplexer, Some(Multiplexer::Zellij { .. })) {
        return false;
    }
    neovim_bridge_enabled()
        || matches!(info.name, TerminalName::Kitty | TerminalName::Ghostty)
        || terminal_field_contains(info.term.as_deref(), "kitty")
        || terminal_field_contains(info.term.as_deref(), "ghostty")
        || terminal_field_contains(info.term_program.as_deref(), "kitty")
        || terminal_field_contains(info.term_program.as_deref(), "ghostty")
}

pub(crate) fn tmux_passthrough_required() -> bool {
    let info = codex_terminal_detection::terminal_info();
    if !matches!(info.multiplexer, Some(Multiplexer::Tmux { .. })) {
        return false;
    }

    // A tmux layer outside Neovim must be wrapped by the bridge when it forwards the APC packet.
    // A tmux layer launched inside the Neovim terminal must still unwrap Codex's packet first.
    let current_tmux = std::env::var_os("TMUX").unwrap_or_default();
    let outer_tmux = std::env::var_os(NEOVIM_OUTER_TMUX_ENV);
    !neovim_bridge_owns_current_tmux(
        neovim_bridge_enabled(),
        current_tmux.as_os_str(),
        outer_tmux.as_deref(),
    )
}

fn neovim_bridge_enabled() -> bool {
    std::env::var_os(NEOVIM_BRIDGE_ENV).is_some_and(|value| value == "1")
}

fn neovim_bridge_owns_current_tmux(
    bridge_enabled: bool,
    current_tmux: &std::ffi::OsStr,
    outer_tmux: Option<&std::ffi::OsStr>,
) -> bool {
    bridge_enabled && outer_tmux.is_some_and(|outer_tmux| outer_tmux == current_tmux)
}

fn terminal_field_contains(value: Option<&str>, needle: &str) -> bool {
    value.is_some_and(|value| value.to_ascii_lowercase().contains(needle))
}

impl PreparedDisplayMath {
    pub(crate) fn has_images(&self) -> bool {
        self.blocks.iter().any(|block| block.rendered.is_some())
    }

    pub(crate) fn render_markdown(
        &self,
        source: &str,
        width: usize,
        cwd: Option<&Path>,
        inline_visualization_context: Option<
            &crate::inline_visualization::InlineVisualizationContext,
        >,
    ) -> Vec<HyperlinkLine> {
        let mut transformed = String::with_capacity(source.len());
        let mut cursor = 0;
        let mut markers = Vec::new();
        for (index, block) in self.blocks.iter().enumerate() {
            transformed.push_str(&source[cursor..block.range.start]);
            if block.rendered.is_some()
                && let Some(layout) = self.layout_for(index, width)
            {
                let marker = marker_for(index);
                let marker_width = match block.style {
                    MathStyle::Display => 1,
                    MathStyle::Inline => usize::from(layout.image.columns),
                };
                transformed.extend(std::iter::repeat_n(marker, marker_width));
                markers.push(MathMarker {
                    symbol: marker,
                    index,
                    style: block.style,
                });
            } else {
                transformed.push_str(&source[block.range.clone()]);
            }
            cursor = block.range.end;
        }
        transformed.push_str(&source[cursor..]);

        let rendered = crate::markdown::render_markdown_agent_with_links_cwd_and_visualizations(
            &transformed,
            Some(width),
            cwd,
            inline_visualization_context,
        );
        let mut projected = Vec::new();
        for mut line in rendered {
            let text = line
                .line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            let replacement = markers
                .iter()
                .filter(|marker| marker.style == MathStyle::Display)
                .find_map(|marker| {
                    (text.trim() == marker.symbol.to_string()).then_some(marker.index)
                });
            if let Some(index) = replacement
                && let Some(layout) = self.layout_for(index, width)
            {
                projected.extend(placeholder_lines(layout, width));
            } else {
                self.project_inline_markers(&mut line, &markers, width);
                projected.push(line);
            }
        }
        projected
    }

    pub(crate) fn terminal_images(&self, width: usize) -> Vec<TerminalImage> {
        self.blocks
            .iter()
            .enumerate()
            .filter_map(|(index, _)| self.layout_for(index, width))
            .map(|layout| layout.image)
            .collect()
    }

    fn layout_for(&self, index: usize, width: usize) -> Option<DisplayMathLayout> {
        let block = self.blocks.get(index)?;
        let rendered = block.rendered.as_ref()?;
        let max_columns = u16::try_from(width).unwrap_or(u16::MAX).max(1);
        let max_width_px = f64::from(max_columns) * f64::from(self.cell_size.width.max(1));
        let max_rows = match block.style {
            MathStyle::Display => MAX_PLACEHOLDER_ROWS,
            MathStyle::Inline => 1,
        };
        let max_height_px = f64::from(max_rows) * f64::from(self.cell_size.height.max(1));
        let scale = (max_width_px / f64::from(rendered.width_px))
            .min(max_height_px / f64::from(rendered.height_px))
            .min(1.0);
        let columns = ((f64::from(rendered.width_px) * scale)
            / f64::from(self.cell_size.width.max(1)))
        .ceil()
        .clamp(1.0, f64::from(max_columns)) as u16;
        let rows = ((f64::from(rendered.height_px) * scale)
            / f64::from(self.cell_size.height.max(1)))
        .ceil()
        .clamp(1.0, f64::from(max_rows)) as u16;
        let image_id = image_id(rendered.fingerprint, columns, rows);
        Some(DisplayMathLayout {
            image: TerminalImage {
                image_id,
                columns,
                rows,
                png: rendered.png.clone(),
                fingerprint: rendered.fingerprint,
            },
        })
    }

    fn project_inline_markers(
        &self,
        line: &mut HyperlinkLine,
        markers: &[MathMarker],
        width: usize,
    ) {
        let layouts = markers
            .iter()
            .filter(|marker| marker.style == MathStyle::Inline)
            .filter_map(|marker| {
                self.layout_for(marker.index, width)
                    .map(|layout| (marker.symbol, layout.image))
            })
            .collect::<Vec<_>>();
        if layouts.is_empty() {
            return;
        }

        let mut started = Vec::new();
        let mut projected = Vec::new();
        for span in std::mem::take(&mut line.line.spans) {
            let content = span.content.into_owned();
            let mut text_start = 0;
            for (byte, ch) in content.char_indices() {
                let Some((_, image)) = layouts.iter().find(|(symbol, _)| *symbol == ch) else {
                    continue;
                };
                if text_start < byte {
                    projected.push(Span::styled(
                        content[text_start..byte].to_string(),
                        span.style,
                    ));
                }
                let first = !started.contains(&ch);
                if first {
                    started.push(ch);
                }
                projected.push(Span::styled(
                    placeholder_cell(/*row*/ 0, /*column*/ first),
                    span.style.fg(image_id_color(image.image_id)),
                ));
                text_start = byte + ch.len_utf8();
            }
            if text_start < content.len() {
                projected.push(Span::styled(content[text_start..].to_string(), span.style));
            }
        }
        line.line.spans = projected;
    }
}

struct DisplayMathLayout {
    image: TerminalImage,
}

#[derive(Clone, Copy)]
struct MathMarker {
    symbol: char,
    index: usize,
    style: MathStyle,
}

fn placeholder_lines(layout: DisplayMathLayout, width: usize) -> Vec<HyperlinkLine> {
    let image = layout.image;
    let left_padding = width.saturating_sub(usize::from(image.columns)) / 2;
    (0..image.rows)
        .map(|row| {
            let mut cells = String::with_capacity(
                usize::from(image.columns).saturating_mul(PLACEHOLDER.len_utf8()),
            );
            cells.push_str(&placeholder_cell(row, /*column*/ true));
            cells.extend(std::iter::repeat_n(
                PLACEHOLDER,
                usize::from(image.columns.saturating_sub(1)),
            ));
            HyperlinkLine::new(Line::from(vec![
                Span::raw(" ".repeat(left_padding)),
                Span::styled(cells, Style::default().fg(image_id_color(image.image_id))),
            ]))
        })
        .collect()
}

fn placeholder_cell(row: u16, column: bool) -> String {
    let mut cell = String::from(PLACEHOLDER);
    if column {
        cell.push(DIACRITICS[usize::from(row)]);
        cell.push(DIACRITICS[0]);
    }
    cell
}

fn image_id_color(image_id: u32) -> Color {
    let (red, green, blue) = (
        ((image_id >> 16) & 0xff) as u8,
        ((image_id >> 8) & 0xff) as u8,
        (image_id & 0xff) as u8,
    );
    // Kitty's Unicode placeholder protocol encodes the 24-bit image ID in the exact foreground
    // RGB value. Theme-adaptive ANSI colors cannot represent that protocol field.
    #[allow(clippy::disallowed_methods)]
    Color::Rgb(red, green, blue)
}

fn marker_for(index: usize) -> char {
    char::from_u32(0xf0000 + u32::try_from(index).unwrap_or(0)).unwrap_or('\u{f0000}')
}

fn image_id(fingerprint: [u8; 32], columns: u16, rows: u16) -> u32 {
    let mut hasher = Sha256::new();
    hasher.update(fingerprint);
    hasher.update(columns.to_le_bytes());
    hasher.update(rows.to_le_bytes());
    let digest = hasher.finalize();
    let id = u32::from_be_bytes([0, digest[0], digest[1], digest[2]]);
    match id {
        // Keep display math out of the fixed image-ID range used by terminal pets.
        0 | 0xc0de | 0xc0df => id ^ 0x800000,
        _ => id,
    }
}

pub(crate) fn kitty_upload_command(image: &TerminalImage, tmux: bool) -> String {
    let payload = general_purpose::STANDARD.encode(image.png.as_ref());
    let chunks = payload
        .as_bytes()
        .chunks(KITTY_CHUNK_SIZE)
        .collect::<Vec<_>>();
    let mut command = String::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let chunk = std::str::from_utf8(chunk).unwrap_or_default();
        let more = u8::from(index + 1 < chunks.len());
        let sequence = if index == 0 {
            format!(
                "\x1b_Ga=T,t=d,f=100,i={},U=1,c={},r={},q=2,m={more};{chunk}\x1b\\",
                image.image_id, image.columns, image.rows
            )
        } else {
            format!("\x1b_Gm={more};{chunk}\x1b\\")
        };
        command.push_str(&wrap_for_tmux(&sequence, tmux));
    }
    command
}

pub(crate) fn kitty_delete_command(image_id: u32, tmux: bool) -> String {
    wrap_for_tmux(&format!("\x1b_Ga=d,d=I,i={image_id},q=2;\x1b\\"), tmux)
}

fn wrap_for_tmux(command: &str, tmux: bool) -> String {
    if !tmux {
        return command.to_string();
    }
    format!("\x1bPtmux;{}\x1b\\", command.replace('\x1b', "\x1b\x1b"))
}

#[cfg(test)]
pub(crate) fn parse_for_test(source: &str) -> Vec<(Range<usize>, String)> {
    parse_math(source)
        .into_iter()
        .map(|block| (block.range, block.formula))
        .collect()
}

#[cfg(test)]
pub(crate) fn parse_styles_for_test(source: &str) -> Vec<MathStyle> {
    parse_math(source)
        .into_iter()
        .map(|block| block.style)
        .collect()
}

#[cfg(test)]
pub(crate) fn bridge_owns_tmux_for_test(
    bridge_enabled: bool,
    current_tmux: &str,
    outer_tmux: Option<&str>,
) -> bool {
    neovim_bridge_owns_current_tmux(
        bridge_enabled,
        std::ffi::OsStr::new(current_tmux),
        outer_tmux.map(std::ffi::OsStr::new),
    )
}

#[cfg(test)]
pub(crate) fn prepared_for_test(
    source: &str,
    width_px: u32,
    height_px: u32,
) -> PreparedDisplayMath {
    let blocks = parse_math(source)
        .into_iter()
        .map(|block| {
            let mut hasher = Sha256::new();
            hasher.update(match block.style {
                MathStyle::Display => b"display".as_slice(),
                MathStyle::Inline => b"inline".as_slice(),
            });
            hasher.update(block.formula.as_bytes());
            let fingerprint: [u8; 32] = hasher.finalize().into();
            PreparedBlock {
                range: block.range,
                style: block.style,
                rendered: Some(RenderedMath {
                    png: Arc::from(Vec::from(b"test-png".as_slice())),
                    width_px,
                    height_px,
                    fingerprint,
                }),
            }
        })
        .collect();
    PreparedDisplayMath {
        blocks,
        cell_size: CellSizePixels::default(),
    }
}
