use display::fb::Framebuffer;
use display::font::{draw_text, literata, measure_text, BitmapFont, FontStyle};
use display::render::{draw_ascii, fill_rect};
use display::{Rect, HEIGHT, WIDTH};
use proto::book::BookId;
use proto::epub::{
    load_epub_package, parse_css_text_align, xhtml_blocks_to_sink, CssRules, EpubPackage,
    SpineItem, XhtmlBlockSink, XhtmlError, ZipArchive, ZipEntry,
};
use proto::text::{TextAlign, TextRole};
use std::env;
use std::fs::{create_dir_all, read, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use ui::{
    render::render_shell, UiBook, UiLibraryStatus, UiOrientation, UiRefreshPolicy, UiShell,
    UiTocItem, UiView,
};

const PAGE_TOP: i16 = 22;
const PAGE_BOTTOM: i16 = 472;
const READER_LEFT_X: i16 = 8;
const READER_RIGHT_X: i16 = 792;
const READER_WRAP_SAFETY: i16 = 4;
const STYLE_MARKER: char = '\u{1b}';
const MAX_PREVIEW_LINES: usize = 4096;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse()?;
    create_dir_all(&args.out_dir)?;
    if let Some(epub) = args.epub {
        preview_epub(&epub, &args.out_dir, args.pages)?;
    } else {
        write_static_previews(&args.out_dir)?;
    }
    Ok(())
}

struct Args {
    epub: Option<PathBuf>,
    out_dir: PathBuf,
    pages: usize,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut epub = None;
        let mut out_dir = PathBuf::from("target/previews");
        let mut pages = 5usize;
        let mut iter = env::args().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--out" => out_dir = PathBuf::from(iter.next().ok_or("--out needs a path")?),
                "--pages" => {
                    let value = iter.next().ok_or("--pages needs a count")?;
                    pages = value.parse().map_err(|_| "--pages needs a number")?;
                }
                "--help" | "-h" => {
                    return Err(
                        "usage: cargo run -- <book.epub> [--out target/previews] [--pages 5]"
                            .into(),
                    );
                }
                value if value.starts_with('-') => return Err(format!("unknown option: {value}")),
                value => epub = Some(PathBuf::from(value)),
            }
        }
        Ok(Self {
            epub,
            out_dir,
            pages,
        })
    }
}

fn preview_epub(
    epub_path: &Path,
    out_dir: &Path,
    page_limit: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = read(epub_path)?;
    let zip = ZipArchive::new(&bytes).map_err(|err| format!("zip: {err:?}"))?;
    let mut container = vec![0u8; 8192];
    let mut opf = vec![0u8; 128 * 1024];
    let package = load_epub_package(
        &bytes,
        &mut container,
        &mut opf,
        BookId(1),
        epub_path.to_str().unwrap_or("book.epub"),
    )
    .map_err(|err| format!("epub package: {err:?}"))?;

    let mut css_rules = CssRules::new();
    load_css_rules(&zip, &package, &mut css_rules);

    let start_spine = package
        .text_reference_href
        .and_then(|href| {
            package
                .spine
                .iter()
                .position(|item| strip_fragment(item.href) == href)
        })
        .unwrap_or(0);

    let mut lines = PreviewLines::new();
    for spine in package
        .spine
        .iter()
        .skip(start_spine)
        .filter(|item| !item.href.is_empty() && !spine_item_is_navigation(item, &package))
    {
        if !lines.is_empty() {
            lines.force_next_line_to_new_page();
        }
        let path = resolve_epub_href(package.opf_path, spine.href);
        let Some(xhtml_entry) = zip.entries().flatten().find(|entry| entry.name == path) else {
            continue;
        };
        let xhtml_bytes = read_zip_entry(&zip, xhtml_entry)?;
        let Ok(xhtml) = std::str::from_utf8(&xhtml_bytes) else {
            continue;
        };
        let mut sink = PreviewSink::new(&mut lines);
        xhtml_blocks_to_sink(xhtml, Some(&css_rules), &mut sink)
            .map_err(|err| format!("xhtml: {err:?}"))?;
        if paginate(&lines).len() >= page_limit {
            break;
        }
    }

    let pages = paginate(&lines);
    write_report(out_dir, epub_path, &package, &lines, &pages)?;
    for (page_index, page) in pages.iter().take(page_limit).enumerate() {
        let mut fb = Framebuffer::new();
        draw_preview_page(&mut fb, &lines, *page);
        write_pbm(&out_dir.join(format!("epub-page-{page_index:02}.pbm")), &fb)?;
        write_png(&out_dir.join(format!("epub-page-{page_index:02}.png")), &fb)?;
        write_page_text(
            &out_dir.join(format!("epub-page-{page_index:02}.txt")),
            &lines,
            *page,
        )?;
    }

    println!(
        "Wrote {} page previews for '{}' to {}",
        pages.len().min(page_limit),
        package.meta.title,
        out_dir.display()
    );
    Ok(())
}

fn read_zip_entry(zip: &ZipArchive<'_>, entry: ZipEntry<'_>) -> Result<Vec<u8>, String> {
    let mut output = vec![0u8; entry.uncompressed_size as usize];
    let len = zip
        .read_entry(entry, &mut output)
        .map_err(|err| format!("zip entry {}: {err:?}", entry.name))?;
    output.truncate(len);
    Ok(output)
}

fn load_css_rules(zip: &ZipArchive<'_>, package: &EpubPackage<'_>, rules: &mut CssRules) {
    rules.clear();
    for item in package
        .manifest
        .iter()
        .filter(|item| item.media_type.contains("css") || item.href.ends_with(".css"))
    {
        let path = resolve_epub_href(package.opf_path, item.href);
        let Some(entry) = zip.entries().flatten().find(|entry| entry.name == path) else {
            continue;
        };
        let Ok(bytes) = read_zip_entry(zip, entry) else {
            continue;
        };
        let Ok(css) = std::str::from_utf8(&bytes) else {
            continue;
        };
        parse_css_text_align(css, rules);
    }
}

#[derive(Clone, Copy)]
struct PageSlice {
    first_line: usize,
    line_count: usize,
}

#[derive(Clone)]
struct PreviewLine {
    text: String,
    role: TextRole,
    align: TextAlign,
    style: FontStyle,
    paragraph_end: bool,
    page_break_before: bool,
}

struct PreviewLines {
    lines: Vec<PreviewLine>,
}

impl PreviewLines {
    fn new() -> Self {
        Self { lines: Vec::new() }
    }

    fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    fn force_next_line_to_new_page(&mut self) {
        self.lines.push(PreviewLine {
            text: String::new(),
            role: TextRole::Body,
            align: TextAlign::Justify,
            style: FontStyle::Regular,
            paragraph_end: false,
            page_break_before: true,
        });
    }
}

struct PreviewSink<'a> {
    lines: &'a mut PreviewLines,
    line: String,
    line_role: TextRole,
    line_align: TextAlign,
    line_style: FontStyle,
    pending_space: bool,
    dropping_paragraph: bool,
}

impl<'a> PreviewSink<'a> {
    fn new(lines: &'a mut PreviewLines) -> Self {
        Self {
            lines,
            line: String::new(),
            line_role: TextRole::Body,
            line_align: TextAlign::Justify,
            line_style: FontStyle::Regular,
            pending_space: false,
            dropping_paragraph: false,
        }
    }
}

impl XhtmlBlockSink for PreviewSink<'_> {
    fn push_block(
        &mut self,
        text: &str,
        role: TextRole,
        style: proto::text::FontStyle,
        align: TextAlign,
        paragraph_end: bool,
    ) -> Result<(), XhtmlError> {
        push_styled_fragment(
            self,
            text,
            preview_style_for_proto_style(style, role),
            role,
            align,
            paragraph_end,
        );
        Ok(())
    }
}

fn push_styled_fragment(
    sink: &mut PreviewSink<'_>,
    text: &str,
    style: FontStyle,
    role: TextRole,
    align: TextAlign,
    paragraph_end: bool,
) {
    if sink.dropping_paragraph {
        if paragraph_end {
            sink.dropping_paragraph = false;
            sink.pending_space = false;
        }
        return;
    }
    let starts_with_space = text
        .chars()
        .next()
        .map(char::is_whitespace)
        .unwrap_or(false);
    let ends_with_space = text
        .chars()
        .next_back()
        .map(char::is_whitespace)
        .unwrap_or(false);
    let mut normalized = normalize_text(text);
    if !sanitize_preview_block(&mut normalized) {
        sink.dropping_paragraph = !paragraph_end;
        sink.pending_space = false;
        return;
    }
    if normalized.is_empty() {
        sink.pending_space |= starts_with_space || ends_with_space;
        if paragraph_end {
            flush_preview_line(sink, true);
        }
        return;
    }

    normalize_decorative_separator(&mut normalized);
    let align = block_align_for(align, &normalized, role);
    let font = literata(style);
    let x = reader_x_for(role);
    let max_x = reader_max_x_for(role, align);

    if !sink.line.is_empty() && (sink.line_role != role || sink.line_align != align) {
        flush_preview_line(sink, false);
    }
    if sink.line.is_empty() {
        sink.line_role = role;
        sink.line_align = align;
        sink.line_style = FontStyle::Regular;
    }

    let mut first_word = true;
    for word in normalized.split_whitespace() {
        let attach = is_leading_punctuation_word(word) && !sink.line.is_empty();
        let leading_space = !sink.line.is_empty()
            && !attach
            && (sink.pending_space || !first_word || starts_with_space);
        let mut candidate = sink.line.clone();
        append_styled_word(&mut candidate, word, style, leading_space);
        if !sink.line.is_empty()
            && styled_text_ink_width(&candidate, font) + x + READER_WRAP_SAFETY > max_x
        {
            flush_preview_line(sink, false);
            append_styled_word(&mut sink.line, word, style, false);
        } else {
            sink.line = candidate;
        }
        sink.line_role = role;
        sink.line_align = align;
        sink.line_style = style;
        sink.pending_space = false;
        first_word = false;
    }

    sink.pending_space |= ends_with_space;
    if paragraph_end {
        flush_preview_line(sink, true);
    }
}

fn append_styled_word(line: &mut String, word: &str, style: FontStyle, leading_space: bool) {
    if leading_space {
        line.push(' ');
    }
    line.push(STYLE_MARKER);
    line.push(style_marker_code(style));
    line.push_str(word);
}

fn flush_preview_line(sink: &mut PreviewSink<'_>, paragraph_end: bool) {
    if sink.line.is_empty() {
        if paragraph_end {
            if let Some(previous) = sink.lines.lines.last_mut() {
                previous.paragraph_end = true;
            }
        }
        return;
    }
    if sink.lines.lines.len() < MAX_PREVIEW_LINES {
        sink.lines.lines.push(PreviewLine {
            text: sink.line.clone(),
            role: sink.line_role,
            align: sink.line_align,
            style: first_styled_line_style(&sink.line).unwrap_or(FontStyle::Regular),
            paragraph_end,
            page_break_before: false,
        });
    }
    sink.line.clear();
    sink.line_style = FontStyle::Regular;
    sink.pending_space = false;
}

fn paginate(lines: &PreviewLines) -> Vec<PageSlice> {
    let mut pages = Vec::new();
    let mut first_line = 0usize;
    let mut line_count = 0usize;
    let mut y = PAGE_TOP;

    for (index, line) in lines.lines.iter().enumerate() {
        if line.text.is_empty() && line.page_break_before {
            if line_count > 0 {
                pages.push(PageSlice {
                    first_line,
                    line_count,
                });
            }
            first_line = index + 1;
            line_count = 0;
            y = PAGE_TOP;
            continue;
        }

        let height = line_advance_for(line.role) + paragraph_gap_after(line);
        if (line.page_break_before || y + height > PAGE_BOTTOM) && line_count > 0 {
            pages.push(PageSlice {
                first_line,
                line_count,
            });
            first_line = index;
            line_count = 0;
            y = PAGE_TOP;
        }
        line_count += 1;
        y += height;
    }

    if line_count > 0 {
        pages.push(PageSlice {
            first_line,
            line_count,
        });
    }
    pages
}

fn draw_preview_page(fb: &mut Framebuffer, lines: &PreviewLines, page: PageSlice) {
    let mut y = PAGE_TOP + 16;
    for index in page.first_line..page.first_line + page.line_count {
        let Some(line) = lines.lines.get(index) else {
            break;
        };
        if line.text.is_empty() {
            continue;
        }
        let font = literata(line.style);
        let x = match line.align {
            TextAlign::Center => {
                let width =
                    styled_text_ink_width(&line.text, font).min(READER_RIGHT_X - READER_LEFT_X);
                ((WIDTH as i16 - width) / 2).max(READER_LEFT_X)
            }
            TextAlign::Left | TextAlign::Justify => reader_x_for(line.role),
        };
        draw_styled_line(fb, &line.text, x, y, line.style);
        y += line_advance_for(line.role) + paragraph_gap_after(line);
    }
}

fn write_report(
    out_dir: &Path,
    epub_path: &Path,
    package: &EpubPackage<'_>,
    lines: &PreviewLines,
    pages: &[PageSlice],
) -> std::io::Result<()> {
    let mut file = BufWriter::new(File::create(out_dir.join("epub-report.txt"))?);
    writeln!(file, "source: {}", epub_path.display())?;
    writeln!(file, "title: {}", package.meta.title)?;
    writeln!(file, "author: {}", package.meta.author)?;
    writeln!(file, "spine items: {}", package.spine.len())?;
    writeln!(file, "rendered lines: {}", lines.lines.len())?;
    writeln!(file, "pages: {}", pages.len())?;
    writeln!(file)?;
    for (index, page) in pages.iter().take(12).enumerate() {
        writeln!(
            file,
            "page {index}: lines {}..{}",
            page.first_line,
            page.first_line + page.line_count
        )?;
    }
    Ok(())
}

fn write_page_text(path: &Path, lines: &PreviewLines, page: PageSlice) -> std::io::Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    for index in page.first_line..page.first_line + page.line_count {
        let Some(line) = lines.lines.get(index) else {
            break;
        };
        if line.text.is_empty() {
            continue;
        }
        writeln!(
            file,
            "[{:?} {:?} {:?}] {}",
            line.role,
            line.align,
            line.style,
            styled_text_snapshot(&line.text)
        )?;
    }
    Ok(())
}

fn write_static_previews(out: &Path) -> std::io::Result<()> {
    write_shell_preview(out, "home", UiView::Home, 0)?;
    write_landscape_home_mockups(out)?;
    write_shell_preview(out, "files", UiView::Library, 1)?;
    write_reading(&out.join("reading.pbm"))?;
    write_chapters(&out.join("chapters.pbm"))?;
    write_shell_preview(out, "chapters-ui", UiView::Chapters, 3)?;
    write_shell_preview(out, "settings", UiView::Settings, 1)?;
    Ok(())
}

fn write_landscape_home_mockups(out: &Path) -> std::io::Result<()> {
    let variants = [
        ("home-landscape-rail", LandscapeHomeVariant::Rail),
        ("home-landscape-tabs", LandscapeHomeVariant::Tabs),
        ("home-landscape-book", LandscapeHomeVariant::BookFirst),
    ];
    for (name, variant) in variants {
        let mut fb = Framebuffer::new();
        draw_landscape_home(&mut fb, variant);
        write_pbm(&out.join(format!("{name}.pbm")), &fb)?;
        write_png(&out.join(format!("{name}.png")), &fb)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum LandscapeHomeVariant {
    Rail,
    Tabs,
    BookFirst,
}

fn draw_landscape_home(fb: &mut Framebuffer, variant: LandscapeHomeVariant) {
    fb.clear(true);
    match variant {
        LandscapeHomeVariant::Rail => draw_landscape_home_rail(fb),
        LandscapeHomeVariant::Tabs => draw_landscape_home_tabs(fb),
        LandscapeHomeVariant::BookFirst => draw_landscape_home_book_first(fb),
    }
}

fn draw_landscape_home_rail(fb: &mut Framebuffer) {
    let title_font = literata(FontStyle::Bold);
    let body_font = literata(FontStyle::Regular);
    let small_font = literata(FontStyle::Regular);
    draw_battery_landscape(fb, 724, 24, 82);
    fill_rect(fb, Rect::new(316, 28, 1, 424), false);
    draw_landscape_action_stack(fb, 28, 56, 248, 344, 0);
    draw_cover_art(fb, 430, 48, 205, 306);
    draw_text_centered(fb, title_font, "Flowers for Algernon", 532, 388);
    draw_text_centered(fb, body_font, "Daniel Keyes", 532, 418);
    draw_thin_progress(fb, 462, 442, 140, 420);
    draw_text(fb, small_font, "42%", 612, 450, false);
}

fn draw_landscape_home_tabs(fb: &mut Framebuffer) {
    let title_font = literata(FontStyle::Bold);
    let body_font = literata(FontStyle::Regular);
    draw_battery_landscape(fb, 724, 24, 82);
    draw_cover_art(fb, 468, 50, 190, 284);
    draw_text(fb, title_font, "Flowers for Algernon", 58, 82, false);
    draw_text(fb, body_font, "Daniel Keyes", 58, 116, false);
    draw_thin_progress(fb, 60, 146, 232, 420);
    draw_text(fb, body_font, "42%", 306, 153, false);
    fill_rect(fb, Rect::new(58, 198, 318, 1), false);
    draw_text(fb, body_font, "Current book", 58, 238, false);
    draw_text(fb, body_font, "Chapter 7", 58, 272, false);
    draw_bottom_tabs(fb, 16, 416, 768, 52, 0);
}

fn draw_landscape_home_book_first(fb: &mut Framebuffer) {
    let title_font = literata(FontStyle::Bold);
    let body_font = literata(FontStyle::Regular);
    draw_battery_landscape(fb, 724, 24, 82);
    draw_landscape_action_stack(fb, 36, 92, 206, 296, 0);
    draw_cover_art(fb, 336, 34, 245, 366);
    draw_text_centered(fb, title_font, "Flowers for Algernon", 458, 432);
    draw_text_centered(fb, body_font, "Daniel Keyes", 458, 460);
    draw_thin_progress(fb, 622, 156, 120, 420);
    draw_text(fb, body_font, "42%", 660, 188, false);
}

fn draw_landscape_action_stack(
    fb: &mut Framebuffer,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    selected: usize,
) {
    let labels = ["Read", "Files", "Sync", "Settings"];
    let font = literata(FontStyle::Regular);
    let row_h = h / labels.len() as u16;
    for (index, label) in labels.iter().enumerate() {
        let row_y = y + index as u16 * row_h;
        if index == selected {
            fill_rect(fb, Rect::new(x, row_y, w, row_h.saturating_sub(2)), false);
        } else {
            fill_rect(fb, Rect::new(x, row_y, w, 1), false);
            fill_rect(
                fb,
                Rect::new(x, row_y + row_h.saturating_sub(2), w, 1),
                false,
            );
        }
        let text_w = measure_text(font, label) as i16;
        let text_x = x as i16 + 24;
        let text_y = row_y as i16 + (row_h as i16 / 2) + 8;
        draw_text(fb, font, label, text_x, text_y, index == selected);
        fill_rect(
            fb,
            Rect::new(x + w - 30, row_y + row_h / 2 - 1, 20, 2),
            index == selected,
        );
        if index == selected {
            fill_rect(fb, Rect::new(x + 10, row_y + 10, 4, row_h - 22), true);
        } else {
            fill_rect(fb, Rect::new((text_x + text_w + 18) as u16, row_y + row_h / 2, 32, 1), false);
        }
    }
}

fn draw_bottom_tabs(fb: &mut Framebuffer, x: u16, y: u16, w: u16, h: u16, selected: usize) {
    let labels = ["Read", "Files", "Sync", "Settings"];
    let font = literata(FontStyle::Regular);
    let tab_w = w / labels.len() as u16;
    for (index, label) in labels.iter().enumerate() {
        let tab_x = x + index as u16 * tab_w;
        if index == selected {
            fill_rect(fb, Rect::new(tab_x, y, tab_w, h), false);
        } else {
            stroke_rect_direct(fb, tab_x, y, tab_w, h);
        }
        let text_x = tab_x as i16 + (tab_w as i16 - measure_text(font, label) as i16) / 2;
        draw_text(fb, font, label, text_x, y as i16 + 34, index == selected);
    }
}

fn draw_cover_art(fb: &mut Framebuffer, x: u16, y: u16, w: u16, h: u16) {
    stroke_rect_direct(fb, x, y, w, h);
    stroke_rect_direct(fb, x + 8, y + 8, w - 16, h - 16);
    fill_rect(fb, Rect::new(x + 18, y + 28, w - 36, 2), false);
    fill_rect(fb, Rect::new(x + 18, y + 58, w - 36, 1), false);
    fill_rect(fb, Rect::new(x + 28, y + h - 58, w - 56, 2), false);
    for row in 0..8 {
        let yy = y + 86 + row * 20;
        let inset = 24 + (row % 3) * 10;
        fill_rect(fb, Rect::new(x + inset, yy, w - inset * 2, 3), false);
    }
    draw_ascii(fb, "FLOWERS", x as usize + 48, y as usize + 36, false);
    draw_ascii(
        fb,
        "ALGERNON",
        x as usize + 42,
        y as usize + h as usize - 40,
        false,
    );
}

fn draw_battery_landscape(fb: &mut Framebuffer, x: u16, y: u16, percent: u8) {
    let font = literata(FontStyle::Regular);
    stroke_rect_direct(fb, x, y, 42, 18);
    fill_rect(fb, Rect::new(x + 42, y + 6, 4, 6), false);
    let fill_w = ((percent.min(100) as u16 * 34) / 100).max(1);
    fill_rect(fb, Rect::new(x + 4, y + 4, fill_w, 10), false);
    draw_text(fb, font, "82%", x as i16 - 48, y as i16 + 16, false);
}

fn draw_thin_progress(fb: &mut Framebuffer, x: u16, y: u16, w: u16, permille: u16) {
    fill_rect(fb, Rect::new(x, y, w, 1), false);
    let fill_w = ((w as u32 * permille.min(1000) as u32) / 1000) as u16;
    fill_rect(fb, Rect::new(x, y.saturating_sub(2), fill_w.max(1), 5), false);
}

fn draw_text_centered(fb: &mut Framebuffer, font: &BitmapFont, text: &str, center_x: i16, y: i16) {
    let x = center_x - measure_text(font, text) as i16 / 2;
    draw_text(fb, font, text, x, y, false);
}

fn stroke_rect_direct(fb: &mut Framebuffer, x: u16, y: u16, w: u16, h: u16) {
    fill_rect(fb, Rect::new(x, y, w, 1), false);
    fill_rect(fb, Rect::new(x, y + h - 1, w, 1), false);
    fill_rect(fb, Rect::new(x, y, 1, h), false);
    fill_rect(fb, Rect::new(x + w - 1, y, 1, h), false);
}

fn write_shell_preview(
    out: &Path,
    name: &str,
    view: UiView,
    selection: u8,
) -> std::io::Result<()> {
    let mut fb = Framebuffer::new();
    let entries = [
        "/books/Flowers for Algernon.epub",
        "/books/The Time Machine.epub",
        "/books/Unsong.epub",
    ];
    let chapters = [
        UiTocItem {
            title: "The Time Machine",
            level: 1,
        },
        UiTocItem {
            title: "I. Introduction",
            level: 1,
        },
        UiTocItem {
            title: "II. The Machine",
            level: 1,
        },
        UiTocItem {
            title: "III. The Time Traveller Returns",
            level: 1,
        },
        UiTocItem {
            title: "IV. Time Travelling",
            level: 1,
        },
        UiTocItem {
            title: "V. In the Golden Age",
            level: 1,
        },
    ];
    let shell = UiShell {
        view,
        orientation: UiOrientation::LandscapeButtonsBottom,
        refresh_policy: UiRefreshPolicy::FullOnWake,
        selection,
        battery_percent: 82,
        active_book: UiBook {
            title: "Flowers for Algernon",
            author: "Daniel Keyes",
            progress_permille: 420,
        },
        library_status: UiLibraryStatus::Ready,
        library_entries: &entries,
        chapters: &chapters,
    };
    render_shell(&mut fb, &shell);
    write_pbm(&out.join(format!("{name}.pbm")), &fb)?;
    write_png(&out.join(format!("{name}.png")), &fb)?;
    if matches!(view, UiView::Home | UiView::Library | UiView::Settings | UiView::Sync) {
        write_portrait_left_png(&out.join(format!("{name}-upright.png")), &fb)?;
    } else {
        write_panel_png(&out.join(format!("{name}-panel.png")), &fb)?;
    }
    Ok(())
}

fn write_reading(path: &Path) -> std::io::Result<()> {
    let mut fb = Framebuffer::new();
    let heading = literata(FontStyle::Bold);
    let body = literata(FontStyle::Regular);
    draw_text(&mut fb, heading, "Chapter 1", 320, 54, false);
    fill_rect(&mut fb, Rect::new(32, 76, 736, 2), false);
    let mut y = 120;
    for line in [
        "This is the first text-only EPUB reader surface.",
        "It uses generated Literata bitmap glyphs and",
        "keeps pagination as bounded data instead of a DOM.",
    ] {
        draw_text(&mut fb, body, line, 72, y, false);
        y += body.line_height as i16;
    }
    fill_rect(&mut fb, Rect::new(32, 424, 736, 2), false);
    draw_ascii(&mut fb, "Flowers for Algernon", 32, 444, false);
    write_pbm(path, &fb)
}

fn write_chapters(path: &Path) -> std::io::Result<()> {
    let mut fb = Framebuffer::new();
    draw_ascii(&mut fb, "CHAPTERS", 96, 112, false);
    for (index, item) in ["Chapter 1", "Chapter 2", "Chapter 3"].iter().enumerate() {
        draw_ascii(&mut fb, item, 136, 168 + index * 44, false);
    }
    write_pbm(path, &fb)
}

fn write_pbm(path: &Path, fb: &Framebuffer) -> std::io::Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    writeln!(file, "P1")?;
    writeln!(file, "{} {}", WIDTH, HEIGHT)?;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let black = !fb.pixel(x, y);
            write!(file, "{} ", black as u8)?;
        }
        writeln!(file)?;
    }
    Ok(())
}

fn write_png(path: &Path, fb: &Framebuffer) -> std::io::Result<()> {
    let file = BufWriter::new(File::create(path)?);
    let mut encoder = png::Encoder::new(file, WIDTH as u32, HEIGHT as u32);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    let mut pixels = vec![0u8; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            pixels[y * WIDTH + x] = if fb.pixel(x, y) { 255 } else { 0 };
        }
    }
    writer.write_image_data(&pixels)?;
    Ok(())
}

fn write_panel_png(path: &Path, fb: &Framebuffer) -> std::io::Result<()> {
    let file = BufWriter::new(File::create(path)?);
    let mut encoder = png::Encoder::new(file, WIDTH as u32, HEIGHT as u32);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    let mut pixels = vec![0u8; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let fb_x = WIDTH - 1 - x;
            pixels[y * WIDTH + x] = if fb.pixel(fb_x, y) { 255 } else { 0 };
        }
    }
    writer.write_image_data(&pixels)?;
    Ok(())
}

fn write_portrait_left_png(path: &Path, fb: &Framebuffer) -> std::io::Result<()> {
    let file = BufWriter::new(File::create(path)?);
    let mut encoder = png::Encoder::new(file, HEIGHT as u32, WIDTH as u32);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    let mut pixels = vec![0u8; WIDTH * HEIGHT];
    for y in 0..WIDTH {
        for x in 0..HEIGHT {
            let fb_x = WIDTH - 1 - y;
            let fb_y = HEIGHT - 1 - x;
            pixels[y * HEIGHT + x] = if fb.pixel(fb_x, fb_y) { 255 } else { 0 };
        }
    }
    writer.write_image_data(&pixels)?;
    Ok(())
}

fn preview_style_for_proto_style(style: proto::text::FontStyle, role: TextRole) -> FontStyle {
    match style {
        proto::text::FontStyle::BoldItalic => FontStyle::BoldItalic,
        proto::text::FontStyle::Bold => FontStyle::Bold,
        proto::text::FontStyle::Italic => FontStyle::Italic,
        proto::text::FontStyle::Regular => {
            if matches!(
                role,
                TextRole::Heading1 | TextRole::Heading2 | TextRole::Heading3
            ) {
                FontStyle::Bold
            } else {
                FontStyle::Regular
            }
        }
    }
}

fn line_advance_for(role: TextRole) -> i16 {
    if matches!(role, TextRole::Heading1 | TextRole::Heading2) {
        32
    } else {
        27
    }
}

fn paragraph_gap(role: TextRole) -> i16 {
    match role {
        TextRole::Heading1 | TextRole::Heading2 => 10,
        TextRole::Heading3 => 6,
        TextRole::BlockQuote => 6,
        TextRole::Body => 3,
    }
}

fn paragraph_gap_after(line: &PreviewLine) -> i16 {
    if line.paragraph_end {
        paragraph_gap(line.role)
    } else {
        0
    }
}

fn reader_x_for(role: TextRole) -> i16 {
    if matches!(role, TextRole::BlockQuote) {
        32
    } else {
        READER_LEFT_X
    }
}

fn reader_max_x_for(_role: TextRole, _align: TextAlign) -> i16 {
    READER_RIGHT_X
}

fn block_align_for(run_align: TextAlign, block: &str, role: TextRole) -> TextAlign {
    if run_align == TextAlign::Center
        || matches!(
            role,
            TextRole::Heading1 | TextRole::Heading2 | TextRole::Heading3
        )
        || is_decorative_separator(block)
    {
        TextAlign::Center
    } else {
        run_align
    }
}

fn normalize_text(text: &str) -> String {
    let mut output = String::new();
    let mut previous_space = true;
    let mut cursor = 0usize;
    while cursor < text.len() {
        let rest = &text[cursor..];
        let (ch, advance) = if let Some(decoded) = decode_entity(rest) {
            (decoded, rest.find(';').map(|index| index + 1).unwrap_or(1))
        } else {
            let Some(ch) = rest.chars().next() else {
                break;
            };
            (ch, ch.len_utf8())
        };
        if ch.is_whitespace() {
            if !previous_space {
                output.push(' ');
            }
            previous_space = true;
        } else {
            output.push(normalize_char(ch));
            previous_space = false;
        }
        cursor += advance;
    }
    output.trim_end().to_string()
}

fn normalize_char(ch: char) -> char {
    match ch {
        '\u{00A0}' => ' ',
        ch if ch as u32 <= u16::MAX as u32 => ch,
        _ => '?',
    }
}

fn decode_entity(input: &str) -> Option<char> {
    if let Some(decoded) = decode_numeric_entity(input) {
        Some(decoded)
    } else if input.starts_with("&amp;") {
        Some('&')
    } else if input.starts_with("&lt;") {
        Some('<')
    } else if input.starts_with("&gt;") {
        Some('>')
    } else if input.starts_with("&quot;") {
        Some('"')
    } else if input.starts_with("&apos;") {
        Some('\'')
    } else if input.starts_with("&nbsp;") {
        Some(' ')
    } else if input.starts_with("&mdash;") {
        Some('—')
    } else if input.starts_with("&ndash;") {
        Some('–')
    } else if input.starts_with("&lsquo;") {
        Some('‘')
    } else if input.starts_with("&rsquo;") {
        Some('’')
    } else if input.starts_with("&ldquo;") {
        Some('“')
    } else if input.starts_with("&rdquo;") {
        Some('”')
    } else if input.starts_with("&hellip;") {
        Some('…')
    } else {
        None
    }
}

fn decode_numeric_entity(input: &str) -> Option<char> {
    let rest = input.strip_prefix("&#")?;
    let end = rest.find(';')?;
    let entity = &rest[..end];
    let value = if let Some(hex) = entity
        .strip_prefix('x')
        .or_else(|| entity.strip_prefix('X'))
    {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        entity.parse::<u32>().ok()?
    };
    char::from_u32(value)
}

fn sanitize_preview_block(block: &mut String) -> bool {
    *block = block.trim().to_string();
    if block.is_empty() {
        return false;
    }
    if is_epub_titlepage_label(block) || contains_gutenberg_metadata(block) {
        return false;
    }
    if is_decorative_separator(block) {
        normalize_decorative_separator(block);
        return true;
    }
    if let Some(rest) = decorative_prefix_rest(block) {
        if rest.is_empty() {
            normalize_decorative_separator(block);
            return true;
        }
        if is_epub_titlepage_label(rest) || contains_gutenberg_metadata(rest) {
            return false;
        }
    }
    true
}

fn is_epub_titlepage_label(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.starts_with(": ")
        || lower == "title"
        || lower == "author"
        || lower == "creator"
        || lower == "language"
        || lower == "english"
        || lower == "english:"
        || lower == "release date"
        || lower == "original publication"
        || lower.starts_with("most recently updated")
        || lower.starts_with("other information")
        || lower.starts_with("other formats")
        || lower.starts_with("credits")
        || lower.starts_with("produced by")
        || lower.starts_with("transcribed from")
        || lower.starts_with("project gutenberg")
        || lower.starts_with("the project gutenberg")
}

fn contains_gutenberg_metadata(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("most recently updated")
        || lower.contains("project gutenberg ebook")
        || lower.contains("start of the project gutenberg")
        || lower.contains("end of the project gutenberg")
        || lower.contains("other information and formats")
        || lower.contains("this ebook is for the use of anyone")
        || lower.contains("project gutenberg license")
        || lower.contains("www.gutenberg.org")
        || lower.contains("laws of the country where you are located")
}

fn decorative_prefix_rest(text: &str) -> Option<&str> {
    let mut mark_count = 0u8;
    let mut end = 0usize;
    for (index, ch) in text.char_indices() {
        if ch == '*' {
            mark_count = mark_count.saturating_add(1);
            end = index + ch.len_utf8();
            continue;
        }
        if ch.is_whitespace() {
            end = index + ch.len_utf8();
            continue;
        }
        break;
    }
    if mark_count >= 3 {
        Some(text[end..].trim())
    } else {
        None
    }
}

fn is_decorative_separator(text: &str) -> bool {
    let mut saw_mark = false;
    let mut mark_count = 0u8;
    for ch in text.chars() {
        if ch == '*' {
            saw_mark = true;
            mark_count = mark_count.saturating_add(1);
            continue;
        }
        if ch.is_whitespace() {
            continue;
        }
        return false;
    }
    saw_mark && mark_count >= 3
}

fn normalize_decorative_separator(block: &mut String) {
    if is_decorative_separator(block) {
        block.clear();
        block.push_str("* * *");
    }
}

fn is_leading_punctuation_word(word: &str) -> bool {
    word.chars()
        .next()
        .map(|ch| {
            matches!(
                ch,
                ',' | '.' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\u{2019}' | '\u{201D}'
            )
        })
        .unwrap_or(false)
}

fn style_marker_code(style: FontStyle) -> char {
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

fn first_styled_line_style(text: &str) -> Option<FontStyle> {
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == STYLE_MARKER {
            return chars.next().and_then(style_from_marker_code);
        }
    }
    None
}

fn styled_text_ink_width(text: &str, default_font: &BitmapFont) -> i16 {
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

fn draw_styled_line(
    fb: &mut Framebuffer,
    text: &str,
    x: i16,
    baseline_y: i16,
    default_style: FontStyle,
) -> i16 {
    let mut cursor_x = x;
    let mut run_start = 0usize;
    let mut style = default_style;
    let mut iter = text.char_indices();
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

fn styled_text_snapshot(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    let mut current = FontStyle::Regular;
    while let Some(ch) = chars.next() {
        if ch == STYLE_MARKER {
            if let Some(code) = chars.next().and_then(style_from_marker_code) {
                if code != current {
                    current = code;
                    match current {
                        FontStyle::Regular => out.push_str("[regular]"),
                        FontStyle::Italic => out.push_str("[italic]"),
                        FontStyle::Bold => out.push_str("[bold]"),
                        FontStyle::BoldItalic => out.push_str("[bold-italic]"),
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn strip_fragment(value: &str) -> &str {
    value.split('#').next().unwrap_or(value)
}

fn resolve_epub_href(opf_path: &str, href: &str) -> String {
    let href_no_fragment = href.split('#').next().unwrap_or(href);
    if href_no_fragment.starts_with('/') {
        return href_no_fragment.trim_start_matches('/').to_string();
    }
    let mut out = String::new();
    if let Some((dir, _)) = opf_path.rsplit_once('/') {
        out.push_str(dir);
        out.push('/');
    }
    out.push_str(href_no_fragment);
    out
}

fn spine_item_is_navigation(item: &SpineItem<'_>, package: &EpubPackage<'_>) -> bool {
    let lower_href = item.href.to_ascii_lowercase();
    let lower_props = item.properties.to_ascii_lowercase();
    item.media_type == "application/x-dtbncx+xml"
        || package
            .nav_href
            .map(|href| href == item.href)
            .unwrap_or(false)
        || package
            .ncx_href
            .map(|href| href == item.href)
            .unwrap_or(false)
        || lower_props
            .split_ascii_whitespace()
            .any(|prop| prop == "nav")
        || lower_href.ends_with("toc.xhtml")
        || lower_href.ends_with("toc.html")
        || lower_href.ends_with("nav.xhtml")
        || lower_href.ends_with("nav.html")
}
