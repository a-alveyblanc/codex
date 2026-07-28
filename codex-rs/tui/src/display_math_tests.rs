use pretty_assertions::assert_eq;
use ratatui::style::Color;

use super::display_math;

fn visible_text(line: &crate::terminal_hyperlinks::HyperlinkLine) -> String {
    line.line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
fn parses_top_level_display_math_but_not_fenced_code() {
    let source = concat!(
        "Before\n\n",
        "$$\n",
        "\\int_0^1 x^2\\,dx\n",
        "$$\n\n",
        "```markdown\n",
        "$$\n",
        "not math\n",
        "$$\n",
        "```\n",
        "$$ e^{i\\pi} + 1 = 0 $$\n",
    );

    let blocks = display_math::parse_for_test(source);

    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].1, "\\int_0^1 x^2\\,dx");
    assert_eq!(&source[blocks[0].0.clone()], "$$\n\\int_0^1 x^2\\,dx\n$$");
    assert_eq!(blocks[1].1, "e^{i\\pi} + 1 = 0");
}

#[test]
fn parses_inline_math_but_not_code_escapes_or_currency() {
    let source = concat!(
        "Inline $x^2$ and $\\alpha + \\beta$.\n",
        "Code `$not_math$` and escaped \\$not_math$.\n",
        "Price $5 and $10 stays currency.\n",
        "```markdown\n",
        "$also_not_math$\n",
        "```\n",
        "$$\n",
        "y = mx + b\n",
        "$$\n",
    );

    let blocks = display_math::parse_for_test(source);

    assert_eq!(
        blocks
            .iter()
            .map(|(_, formula)| formula.as_str())
            .collect::<Vec<_>>(),
        vec!["x^2", "\\alpha + \\beta", "y = mx + b"],
    );
    assert_eq!(
        display_math::parse_styles_for_test(source),
        vec![
            display_math::MathStyle::Inline,
            display_math::MathStyle::Inline,
            display_math::MathStyle::Display,
        ],
    );
    assert_eq!(&source[blocks[0].0.clone()], "$x^2$");
}

#[test]
fn malformed_indented_and_oversized_blocks_stay_as_source() {
    let oversized = "x".repeat(8 * 1024 + 1);
    assert!(display_math::parse_for_test("$$\nunclosed").is_empty());
    assert!(display_math::parse_for_test("    $$ x $$\n").is_empty());
    assert!(display_math::parse_for_test(&format!("$$\n{oversized}\n$$\n")).is_empty());
}

#[test]
fn projected_markdown_replaces_math_with_centered_placeholder_rows() {
    let source = "A result:\n\n$$\nx^2 + y^2 = z^2\n$$\n\nAfter.";
    let prepared =
        display_math::prepared_for_test(source, /*width_px*/ 120, /*height_px*/ 40);

    let lines = prepared.render_markdown(
        source, /*width*/ 38, /*cwd*/ None, /*inline_visualization_context*/ None,
    );
    let projected = lines
        .iter()
        .map(|line| {
            let text = visible_text(line);
            if text.contains('\u{10eeee}') {
                let color = line.line.spans.iter().find_map(|span| match span.style.fg {
                    Some(Color::Rgb(red, green, blue)) => Some((red, green, blue)),
                    _ => None,
                });
                format!(
                    "<display-math width={} image-color={color:?}>",
                    line.line.width()
                )
            } else {
                text
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    insta::assert_snapshot!(projected);
}

#[test]
fn projected_markdown_reserves_inline_math_width() {
    let source = "Energy $E = mc^2$ stays inline.";
    let prepared =
        display_math::prepared_for_test(source, /*width_px*/ 56, /*height_px*/ 14);

    let lines = prepared.render_markdown(
        source, /*width*/ 40, /*cwd*/ None, /*inline_visualization_context*/ None,
    );
    let projected = lines
        .iter()
        .map(|line| {
            let mut output = String::new();
            let mut columns = 0;
            let mut color = None;
            for span in &line.line.spans {
                let span_columns = span
                    .content
                    .chars()
                    .filter(|ch| *ch == '\u{10eeee}')
                    .count();
                if span_columns > 0 {
                    columns += span_columns;
                    if let Some(Color::Rgb(red, green, blue)) = span.style.fg {
                        color = Some((red, green, blue));
                    }
                } else {
                    if columns > 0 {
                        output.push_str(&format!(
                            "<inline-math columns={columns} image-color={color:?}>"
                        ));
                        columns = 0;
                    }
                    output.push_str(span.content.as_ref());
                }
            }
            if columns > 0 {
                output.push_str(&format!(
                    "<inline-math columns={columns} image-color={color:?}>"
                ));
            }
            output
        })
        .collect::<Vec<_>>()
        .join("\n");

    insta::assert_snapshot!(projected);
    let image = prepared
        .terminal_images(/*width*/ 40)
        .into_iter()
        .next()
        .expect("inline image");
    assert_eq!((image.columns, image.rows), (7, 1));

    let wrapped = prepared.render_markdown(
        source, /*width*/ 12, /*cwd*/ None, /*inline_visualization_context*/ None,
    );
    assert!(wrapped.iter().all(|line| line.line.width() <= 12));
    assert_eq!(
        wrapped
            .iter()
            .map(|line| {
                visible_text(line)
                    .chars()
                    .filter(|ch| *ch == '\u{10eeee}')
                    .count()
            })
            .filter(|columns| *columns > 0)
            .collect::<Vec<_>>(),
        vec![7],
    );
}

#[test]
fn kitty_command_uses_virtual_placement_and_tmux_passthrough() {
    let source = "$$x$$";
    let prepared =
        display_math::prepared_for_test(source, /*width_px*/ 16, /*height_px*/ 16);
    let image = prepared
        .terminal_images(/*width*/ 20)
        .into_iter()
        .next()
        .expect("image");

    let direct = display_math::kitty_upload_command(&image, /*tmux*/ false);
    assert!(direct.starts_with("\x1b_Ga=T,t=d,f=100,i="));
    assert!(direct.contains(",U=1,c=2,r=1,q=2,m=0;"));
    assert!(direct.ends_with("\x1b\\"));

    let tmux = display_math::kitty_upload_command(&image, /*tmux*/ true);
    assert!(tmux.starts_with("\x1bPtmux;\x1b\x1b_G"));
    assert!(tmux.ends_with("\x1b\x1b\\\x1b\\"));
}

#[test]
fn neovim_bridge_only_owns_the_tmux_layer_outside_neovim() {
    assert!(display_math::bridge_owns_tmux_for_test(
        /*bridge_enabled*/ true,
        "/tmp/tmux,1,0",
        Some("/tmp/tmux,1,0"),
    ));
    assert!(!display_math::bridge_owns_tmux_for_test(
        /*bridge_enabled*/ true,
        "/tmp/inner-tmux,2,0",
        Some("/tmp/outer-tmux,1,0"),
    ));
    assert!(!display_math::bridge_owns_tmux_for_test(
        /*bridge_enabled*/ false,
        "/tmp/tmux,1,0",
        Some("/tmp/tmux,1,0"),
    ));
}

#[tokio::test]
#[ignore = "requires Linux, bubblewrap, latex, and dvipng"]
async fn sandboxed_renderer_produces_and_reuses_a_png() {
    let cache = tempfile::tempdir().expect("cache");
    let first = crate::display_math_renderer::render_formula(
        r"\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}",
        display_math::MathStyle::Display,
        cache.path(),
        (235, 235, 235),
        (0, 0, 0),
    )
    .await
    .expect("render formula");
    assert!(first.png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(first.width_px > 0);
    assert!(first.height_px > 0);
    let raster = image::load_from_memory_with_format(first.png.as_ref(), image::ImageFormat::Png)
        .expect("decode rendered formula");
    assert_eq!(
        (first.width_px, first.height_px),
        (raster.width().div_ceil(2), raster.height().div_ceil(2)),
    );

    let cached = crate::display_math_renderer::render_formula(
        r"\int_0^\infty e^{-x^2}\,dx = \frac{\sqrt{\pi}}{2}",
        display_math::MathStyle::Display,
        cache.path(),
        (235, 235, 235),
        (0, 0, 0),
    )
    .await
    .expect("read cached formula");
    assert_eq!(
        (cached.png.as_ref(), cached.width_px, cached.height_px),
        (first.png.as_ref(), first.width_px, first.height_px),
    );

    let inline = crate::display_math_renderer::render_formula(
        r"E = mc^2",
        display_math::MathStyle::Inline,
        cache.path(),
        (235, 235, 235),
        (0, 0, 0),
    )
    .await
    .expect("render inline formula");
    assert!(inline.png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_ne!(inline.fingerprint, first.fingerprint);
}
