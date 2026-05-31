use crate::reader_store::ReaderStore;
use display::fb::Framebuffer;
use display::font::{draw_text, literata, measure_text, BitmapFont, FontStyle};
use display::WIDTH;
use proto::cache::{BlockRecord, PageRecord};
use proto::text::{TextAlign, TextRole};

pub(crate) const READER_PAGE_TOP: i16 = 22;
pub(crate) const READER_PAGE_BOTTOM: i16 = 472;
pub(crate) const READER_LEFT_X: i16 = 8;
pub(crate) const READER_RIGHT_X: i16 = 792;
pub(crate) const READER_WRAP_SAFETY: i16 = 4;
pub(crate) const STYLE_MARKER: char = '\u{1b}';

pub(crate) struct ReaderPagePlan {
    page_count: u32,
    page: PageRecord,
}

pub(crate) struct ReaderDrawableBlock<'a> {
    pub(crate) record: BlockRecord,
    pub(crate) text: &'a str,
    pub(crate) y: i16,
    pub(crate) advance: i16,
    pub(crate) style: FontStyle,
    pub(crate) font: &'static BitmapFont,
}

impl ReaderPagePlan {
    pub(crate) fn new(sd_library: &ReaderStore, requested_page: u32) -> Self {
        let page_count = reader_page_count(sd_library, READER_PAGE_TOP, READER_PAGE_BOTTOM);
        let requested_page = sd_library.local_page_for_global(requested_page.min(page_count - 1));
        let page = reader_page_at(
            sd_library,
            requested_page,
            READER_PAGE_TOP,
            READER_PAGE_BOTTOM,
        );
        Self { page_count, page }
    }

    pub(crate) fn page_count(&self) -> u32 {
        self.page_count
    }

    pub(crate) fn for_each_block(
        &self,
        sd_library: &ReaderStore,
        mut visit: impl FnMut(ReaderDrawableBlock<'_>) -> bool,
    ) {
        let mut y = READER_PAGE_BOTTOM - 8;
        for offset in 0..self.page.block_count as usize {
            let index = self.page.first_block as usize + offset;
            let Some(record) = sd_library.block_record(index) else {
                break;
            };
            let text = sd_library.block_text(index);
            let advance = line_advance_for(record.role);
            let style = sd_library.block_style(index);
            if y < READER_PAGE_TOP {
                break;
            }
            if !visit(ReaderDrawableBlock {
                record,
                text,
                y,
                advance,
                style,
                font: literata(style),
            }) {
                break;
            }
            y -= advance + paragraph_gap_after(sd_library, index);
        }
    }
}

pub(crate) fn reader_page_count(sd_library: &ReaderStore, page_top: i16, page_bottom: i16) -> u32 {
    if sd_library.book_total_pages > 0 {
        return sd_library.book_total_pages;
    }
    if sd_library.page_count > 0 {
        return sd_library.page_count as u32;
    }
    paginate_sd_reader(sd_library, page_top, page_bottom).max(1) as u32
}

pub(crate) fn reader_page_at(
    sd_library: &ReaderStore,
    page_index: usize,
    page_top: i16,
    page_bottom: i16,
) -> PageRecord {
    if page_index < sd_library.page_count {
        return sd_library.pages[page_index];
    }
    let mut current = 0usize;
    let mut first_block = 0usize;
    let mut block_count = 0usize;
    let mut y = page_top;

    for index in 0..sd_library.block_count {
        let block_height = sd_block_height(sd_library, index);
        let new_page = (y + block_height > page_bottom
            || sd_library.block_page_break_before[index])
            && y > page_top;
        if new_page {
            if current == page_index {
                return PageRecord {
                    first_block: first_block as u16,
                    block_count: block_count as u16,
                };
            }
            current += 1;
            first_block = index;
            block_count = 0;
            y = page_top;
        }
        block_count += 1;
        y += block_height;
    }

    PageRecord {
        first_block: first_block as u16,
        block_count: block_count as u16,
    }
}

pub(crate) fn rebuild_page_index(library: &mut ReaderStore, page_top: i16, page_bottom: i16) {
    library.page_count = 0;
    if library.block_count == 0 {
        return;
    }

    let mut first_block = 0usize;
    let mut block_count = 0usize;
    let mut y = page_top;

    for index in 0..library.block_count {
        let block_height = sd_block_height(library, index);
        let new_page = (y + block_height > page_bottom || library.block_page_break_before[index])
            && y > page_top;
        if new_page {
            push_sd_page_record(library, first_block, block_count);
            first_block = index;
            block_count = 0;
            y = page_top;
        }
        block_count += 1;
        y += block_height;
    }

    push_sd_page_record(library, first_block, block_count);
}

pub(crate) fn rebuild_toc_page_targets(library: &mut ReaderStore) {
    for toc_index in 0..library.toc_count {
        let spine_index = library.toc[toc_index].spine_index;
        if spine_index < 0 {
            library.toc_page[toc_index] = 0;
            continue;
        }
        let spine = spine_index as u16;
        let page = library
            .book_sections
            .iter()
            .take(library.book_section_count)
            .find(|section| section.spine == spine)
            .map(|section| section.start_page as usize)
            .or_else(|| {
                library
                    .page_spine
                    .iter()
                    .take(library.page_count)
                    .position(|page_spine| *page_spine == spine)
            })
            .unwrap_or(0);
        library.toc_page[toc_index] = page.min(u16::MAX as usize) as u16;
    }
}

fn push_sd_page_record(library: &mut ReaderStore, first_block: usize, block_count: usize) {
    if block_count == 0 || library.page_count >= library.pages.len() {
        return;
    }
    let page_index = library.page_count;
    library.pages[library.page_count] = PageRecord {
        first_block: first_block as u16,
        block_count: block_count as u16,
    };
    library.page_spine[page_index] = library.block_spine.get(first_block).copied().unwrap_or(0);
    library.page_count += 1;
}

fn paginate_sd_reader(sd_library: &ReaderStore, page_top: i16, page_bottom: i16) -> usize {
    let mut pages = 1u32;
    let mut y = page_top;

    for index in 0..sd_library.block_count {
        if sd_library.block_page_break_before[index] && y > page_top {
            pages = pages.saturating_add(1);
            y = page_top;
        }
        let block_height = sd_block_height(sd_library, index);

        if y + block_height > page_bottom && y > page_top {
            pages = pages.saturating_add(1);
            y = page_top;
        }
        y += block_height;
    }

    pages.max(1) as usize
}

pub(crate) fn line_advance_for(role: TextRole) -> i16 {
    if matches!(role, TextRole::Heading1 | TextRole::Heading2) {
        32
    } else {
        27
    }
}

fn sd_block_height(sd_library: &ReaderStore, index: usize) -> i16 {
    let Some(record) = sd_library.blocks.get(index) else {
        return 0;
    };
    let advance = line_advance_for(record.role);
    let height = if record.line_count == 1 {
        advance
    } else {
        wrapped_block_height(
            literata(sd_library.block_styles[index]),
            sd_library.block_text(index),
            record.role,
            record.align,
            advance,
        )
    };
    height + paragraph_gap_after(sd_library, index)
}

pub(crate) fn wrapped_block_height(
    font: &'static BitmapFont,
    text: &str,
    role: TextRole,
    align: TextAlign,
    line_advance: i16,
) -> i16 {
    let max_width = match align {
        TextAlign::Center => READER_RIGHT_X - READER_LEFT_X,
        TextAlign::Justify => reader_max_x_for(role, align) - reader_x_for(role),
        TextAlign::Left if matches!(role, TextRole::BlockQuote) => {
            reader_max_x_for(role, align) - reader_x_for(role)
        }
        TextAlign::Left => reader_max_x_for(role, align) - reader_x_for(role),
    };
    wrapped_line_count(font, text, max_width).max(1) as i16 * line_advance
}

fn wrapped_line_count(font: &'static BitmapFont, text: &str, max_width: i16) -> u16 {
    let mut cursor = 0usize;
    let bytes = text.as_bytes();
    let mut lines = 0u16;
    while let Some((_, _, next_cursor)) = next_wrapped_line(text, cursor, font, 0, max_width) {
        lines = lines.saturating_add(1);
        cursor = next_cursor;
        if cursor >= bytes.len() {
            break;
        }
    }
    lines
}

pub(crate) fn next_wrapped_line(
    text: &str,
    mut cursor: usize,
    font: &'static BitmapFont,
    x: i16,
    max_x: i16,
) -> Option<(usize, usize, usize)> {
    let bytes = text.as_bytes();
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if cursor >= bytes.len() {
        return None;
    }

    let mut scan = cursor;
    let mut best_end = cursor;
    let mut best_next = cursor;
    while scan < bytes.len() {
        let word_start = scan;
        while scan < bytes.len() && !bytes[scan].is_ascii_whitespace() {
            scan += 1;
        }
        let word_end = scan;
        let candidate = &text[cursor..word_end];
        if x + text_ink_width(font, candidate) + READER_WRAP_SAFETY > max_x {
            if best_end == cursor {
                return Some((word_start, word_end, word_end));
            }
            return Some((cursor, best_end, best_next));
        }
        best_end = word_end;
        while scan < bytes.len() && bytes[scan].is_ascii_whitespace() {
            scan += 1;
        }
        best_next = scan;
    }

    Some((cursor, best_end, best_next.max(best_end)))
}

pub(crate) fn text_ink_width(font: &'static BitmapFont, text: &str) -> i16 {
    let mut advance = 0i16;
    let mut right = 0i16;
    for ch in text.chars() {
        let codepoint = if ch as u32 > u16::MAX as u32 {
            b'?' as u16
        } else {
            ch as u16
        };
        let Some((metric, _)) = font.glyph(codepoint).or_else(|| font.glyph(b'?' as u16)) else {
            advance += 8;
            right = right.max(advance);
            continue;
        };
        let glyph_right = advance + metric.x_offset as i16 + metric.width as i16;
        right = right.max(glyph_right);
        advance += metric.advance as i16;
    }
    right.max(advance)
}

pub(crate) fn styled_text_ink_width(text: &str, default_font: &'static BitmapFont) -> i16 {
    let mut font = default_font;
    let mut chars = text.chars();
    let mut advance = 0i16;
    let mut right = 0i16;
    while let Some(ch) = chars.next() {
        if ch == STYLE_MARKER {
            if let Some(code) = chars.next() {
                font = literata(style_from_marker_code(code).unwrap_or(FontStyle::Regular));
            }
            continue;
        }
        let codepoint = if ch as u32 > u16::MAX as u32 {
            b'?' as u16
        } else {
            ch as u16
        };
        let Some((metric, _)) = font.glyph(codepoint).or_else(|| font.glyph(b'?' as u16)) else {
            advance += 8;
            right = right.max(advance);
            continue;
        };
        let glyph_right = advance + metric.x_offset as i16 + metric.width as i16;
        right = right.max(glyph_right);
        advance += metric.advance as i16;
    }
    right.max(advance)
}

pub(crate) fn style_marker_code(style: FontStyle) -> char {
    match style {
        FontStyle::Regular => '0',
        FontStyle::Italic => '1',
        FontStyle::Bold => '2',
        FontStyle::BoldItalic => '3',
    }
}

fn style_from_marker_code(code: char) -> Option<FontStyle> {
    match code {
        '0' => Some(FontStyle::Regular),
        '1' => Some(FontStyle::Italic),
        '2' => Some(FontStyle::Bold),
        '3' => Some(FontStyle::BoldItalic),
        _ => None,
    }
}

pub(crate) fn first_styled_line_style(text: &str) -> Option<FontStyle> {
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == STYLE_MARKER {
            return chars.next().and_then(style_from_marker_code);
        }
    }
    None
}

pub(crate) fn draw_styled_line(
    fb: &mut Framebuffer,
    text: &str,
    x: i16,
    baseline_y: i16,
    default_style: FontStyle,
) -> i16 {
    let mut cursor_x = x;
    let mut run_start = 0usize;
    let mut style = default_style;
    let mut iter = text.char_indices().peekable();
    while let Some((index, ch)) = iter.next() {
        if ch != STYLE_MARKER {
            continue;
        }
        if run_start < index {
            cursor_x = draw_text(
                fb,
                literata(style),
                &text[run_start..index],
                cursor_x,
                baseline_y,
                false,
            );
        }
        if let Some((code_index, code)) = iter.next() {
            style = style_from_marker_code(code).unwrap_or(style);
            run_start = code_index + code.len_utf8();
        } else {
            run_start = index + ch.len_utf8();
        }
    }
    if run_start < text.len() {
        cursor_x = draw_text(
            fb,
            literata(style),
            &text[run_start..],
            cursor_x,
            baseline_y,
            false,
        );
    }
    cursor_x
}

fn paragraph_gap(role: TextRole) -> i16 {
    match role {
        TextRole::Heading1 | TextRole::Heading2 => 10,
        TextRole::Heading3 => 6,
        TextRole::BlockQuote => 6,
        TextRole::Body => 3,
    }
}

pub(crate) fn paragraph_gap_after(sd_library: &ReaderStore, index: usize) -> i16 {
    if sd_library
        .block_paragraph_end
        .get(index)
        .copied()
        .unwrap_or(true)
    {
        paragraph_gap(sd_library.blocks[index].role)
    } else {
        0
    }
}

pub(crate) fn reader_x_for(role: TextRole) -> i16 {
    if matches!(role, TextRole::BlockQuote) {
        32
    } else {
        READER_LEFT_X
    }
}

pub(crate) fn reader_max_x_for(role: TextRole, align: TextAlign) -> i16 {
    match align {
        TextAlign::Center => READER_RIGHT_X,
        TextAlign::Justify | TextAlign::Left if matches!(role, TextRole::BlockQuote) => {
            READER_RIGHT_X
        }
        TextAlign::Justify | TextAlign::Left => READER_RIGHT_X,
    }
}

pub(crate) fn draw_centered_wrapped_literata(
    fb: &mut Framebuffer,
    font: &'static BitmapFont,
    text: &str,
    mut baseline_y: i16,
    max_width: i16,
    line_advance: i16,
) -> i16 {
    let mut cursor = 0usize;
    let bytes = text.as_bytes();
    while let Some((line_start, line_end, next_cursor)) =
        next_wrapped_line(text, cursor, font, 0, max_width)
    {
        let line = &text[line_start..line_end];
        let width = text_ink_width(font, line).min(max_width);
        let x = ((WIDTH as i16 - width) / 2).max(20);
        draw_text(fb, font, line, x, baseline_y, false);
        baseline_y += line_advance;
        cursor = next_cursor;
        if cursor >= bytes.len() {
            break;
        }
    }

    baseline_y
}

pub(crate) fn draw_wrapped_literata(
    fb: &mut Framebuffer,
    font: &'static BitmapFont,
    text: &str,
    x: i16,
    mut baseline_y: i16,
    max_x: i16,
    line_advance: i16,
) -> i16 {
    let mut cursor = 0usize;
    let bytes = text.as_bytes();
    while let Some((line_start, line_end, next_cursor)) =
        next_wrapped_line(text, cursor, font, x, max_x)
    {
        draw_text(fb, font, &text[line_start..line_end], x, baseline_y, false);
        baseline_y += line_advance;
        cursor = next_cursor;
        if cursor >= bytes.len() {
            break;
        }
    }

    baseline_y
}

pub(crate) fn draw_justified_wrapped_literata(
    fb: &mut Framebuffer,
    font: &'static BitmapFont,
    text: &str,
    x: i16,
    mut baseline_y: i16,
    max_x: i16,
    line_advance: i16,
) -> i16 {
    let mut cursor = 0usize;
    let bytes = text.as_bytes();
    while let Some((line_start, line_end, next_cursor)) =
        next_wrapped_line(text, cursor, font, x, max_x)
    {
        let is_last_line = next_cursor >= bytes.len();
        draw_justified_line(
            fb,
            font,
            &text[line_start..line_end],
            x,
            baseline_y,
            max_x,
            is_last_line,
        );
        baseline_y += line_advance;
        cursor = next_cursor;
        if cursor >= bytes.len() {
            break;
        }
    }

    baseline_y
}

fn draw_justified_line(
    fb: &mut Framebuffer,
    font: &'static BitmapFont,
    line: &str,
    x: i16,
    baseline_y: i16,
    max_x: i16,
    is_last_line: bool,
) {
    let gap_count = line
        .as_bytes()
        .windows(2)
        .filter(|pair| pair[0] == b' ' && pair[1] != b' ')
        .count();
    if is_last_line || gap_count == 0 {
        draw_text(fb, font, line, x, baseline_y, false);
        return;
    }

    let text_width = text_ink_width(font, line);
    let extra = (max_x - x - READER_WRAP_SAFETY - text_width).max(0);
    let extra_per_gap = extra / gap_count as i16;
    let mut remainder = extra % gap_count as i16;
    let mut cursor_x = x;
    let mut word_start = None;

    for (index, byte) in line.bytes().enumerate() {
        if byte == b' ' {
            if let Some(start) = word_start.take() {
                let word = &line[start..index];
                cursor_x = draw_text(fb, font, word, cursor_x, baseline_y, false);
            }
            let mut gap = measure_text(font, " ") as i16 + extra_per_gap;
            if remainder > 0 {
                gap += 1;
                remainder -= 1;
            }
            cursor_x += gap;
        } else if word_start.is_none() {
            word_start = Some(index);
        }
    }
    if let Some(start) = word_start {
        draw_text(fb, font, &line[start..], cursor_x, baseline_y, false);
    }
}
