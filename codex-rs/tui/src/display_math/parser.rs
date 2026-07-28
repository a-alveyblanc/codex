use super::MathStyle;
use super::ParsedBlock;

const MAX_EXPRESSIONS: usize = 32;
const MAX_SOURCE_BYTES: usize = 8 * 1024;

pub(super) fn parse_math(source: &str) -> Vec<ParsedBlock> {
    let mut blocks = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut open: Option<(usize, usize)> = None;
    let mut code_span: Option<usize> = None;
    let mut offset = 0;

    for raw_line in source.split_inclusive('\n') {
        let line_with_cr = raw_line.strip_suffix('\n').unwrap_or(raw_line);
        let line = line_with_cr.strip_suffix('\r').unwrap_or(line_with_cr);
        let line_start = offset;
        let line_end = line_start + line_with_cr.len();
        offset += raw_line.len();

        if let Some((block_start, formula_start)) = open {
            if top_level_line(line).is_some_and(|line| line.trim_end() == "$$") {
                let formula = source[formula_start..line_start].trim().to_string();
                if !formula.is_empty() && formula.len() <= MAX_SOURCE_BYTES {
                    push_expression(
                        &mut blocks,
                        ParsedBlock {
                            range: block_start..line_end,
                            formula,
                            style: MathStyle::Display,
                        },
                    );
                }
                open = None;
            }
            continue;
        }
        if let Some((fence_char, fence_len)) = fence {
            if fence_closes(line, fence_char, fence_len) {
                fence = None;
            }
            continue;
        }
        if let Some((fence_char, fence_len)) = fence_opens(line) {
            fence = Some((fence_char, fence_len));
            code_span = None;
            continue;
        }

        if let Some(top_level) = top_level_line(line) {
            let candidate = top_level.trim_end();
            if candidate == "$$" {
                open = Some((line_start, offset));
                code_span = None;
                continue;
            }
            if let Some(inner) = candidate
                .strip_prefix("$$")
                .and_then(|rest| rest.strip_suffix("$$"))
                .map(str::trim)
                .filter(|inner| !inner.is_empty() && inner.len() <= MAX_SOURCE_BYTES)
            {
                push_expression(
                    &mut blocks,
                    ParsedBlock {
                        range: line_start..line_end,
                        formula: inner.to_string(),
                        style: MathStyle::Display,
                    },
                );
                code_span = None;
                continue;
            }
        }

        if is_indented_code(line) {
            code_span = None;
            continue;
        }
        parse_inline_math(line, line_start, &mut code_span, &mut blocks);
    }
    blocks
}

fn push_expression(blocks: &mut Vec<ParsedBlock>, expression: ParsedBlock) {
    if blocks.len() < MAX_EXPRESSIONS {
        blocks.push(expression);
    }
}

fn parse_inline_math(
    line: &str,
    line_start: usize,
    code_span: &mut Option<usize>,
    blocks: &mut Vec<ParsedBlock>,
) {
    let bytes = line.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'`' {
            let run = bytes[cursor..]
                .iter()
                .take_while(|byte| **byte == b'`')
                .count();
            match *code_span {
                Some(delimiter) if delimiter == run => *code_span = None,
                None => *code_span = Some(run),
                Some(_) => {}
            }
            cursor += run;
            continue;
        }
        if code_span.is_some() {
            cursor += char_len_at(line, cursor);
            continue;
        }
        if bytes[cursor] == b'\\' {
            cursor += 1;
            if cursor < bytes.len() {
                cursor += char_len_at(line, cursor);
            }
            continue;
        }
        if bytes[cursor] != b'$'
            || bytes.get(cursor.wrapping_sub(1)) == Some(&b'$')
            || bytes.get(cursor + 1) == Some(&b'$')
        {
            cursor += char_len_at(line, cursor);
            continue;
        }
        let formula_start = cursor + 1;
        let Some(first) = line[formula_start..].chars().next() else {
            break;
        };
        if first.is_whitespace() {
            cursor = formula_start;
            continue;
        }
        let Some(close) = find_inline_math_close(line, formula_start) else {
            cursor = formula_start;
            continue;
        };
        let formula = &line[formula_start..close];
        if !formula.is_empty() && formula.len() <= MAX_SOURCE_BYTES {
            push_expression(
                blocks,
                ParsedBlock {
                    range: line_start + cursor..line_start + close + 1,
                    formula: formula.to_string(),
                    style: MathStyle::Inline,
                },
            );
        }
        cursor = close + 1;
    }
}

fn find_inline_math_close(line: &str, mut cursor: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    while cursor < bytes.len() {
        if bytes[cursor] == b'\\' {
            cursor += 1;
            if cursor < bytes.len() {
                cursor += char_len_at(line, cursor);
            }
            continue;
        }
        if bytes[cursor] == b'$'
            && bytes.get(cursor.wrapping_sub(1)) != Some(&b'$')
            && bytes.get(cursor + 1) != Some(&b'$')
        {
            let before = line[..cursor].chars().next_back()?;
            let after = line[cursor + 1..].chars().next();
            if !before.is_whitespace() && !after.is_some_and(|ch| ch.is_ascii_digit()) {
                return Some(cursor);
            }
        }
        cursor += char_len_at(line, cursor);
    }
    None
}

fn char_len_at(text: &str, byte: usize) -> usize {
    text[byte..].chars().next().map(char::len_utf8).unwrap_or(1)
}

fn is_indented_code(line: &str) -> bool {
    line.starts_with('\t') || line.bytes().take_while(|byte| *byte == b' ').count() >= 4
}

fn top_level_line(line: &str) -> Option<&str> {
    let indent = line.bytes().take_while(|byte| *byte == b' ').count();
    (indent <= 3).then(|| &line[indent..])
}

fn fence_opens(line: &str) -> Option<(char, usize)> {
    let line = top_level_line(line)?;
    let fence_char = line.chars().next()?;
    if !matches!(fence_char, '`' | '~') {
        return None;
    }
    let fence_len = line.chars().take_while(|ch| *ch == fence_char).count();
    (fence_len >= 3).then_some((fence_char, fence_len))
}

fn fence_closes(line: &str, fence_char: char, fence_len: usize) -> bool {
    let Some(line) = top_level_line(line) else {
        return false;
    };
    let run = line.chars().take_while(|ch| *ch == fence_char).count();
    run >= fence_len && line[run..].trim().is_empty()
}
