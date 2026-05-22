use crate::reader_layout::{self, READER_LEFT_X, READER_RIGHT_X};
use crate::reader_store::{BookLoadStatus, LibraryScanStatus, ReaderStore, MAX_LIBRARY_BOOKS};
use crate::{catalog, AppView, RenderRequest};
use display::fb::Framebuffer;
use display::font::literata;
use display::render::{draw_ascii, fill_rect, stroke_rect};
use display::{Rect, WIDTH};
use proto::text::TextAlign;
use ui::{
    app_render::{self, UiRenderModel},
    UiBook, UiCover, UiLibraryStatus, UiTocItem,
};

const SHOW_INPUT_DEBUG: bool = false;
const MAX_UI_CHAPTERS: usize = 64;

pub(crate) fn render(fb: &mut Framebuffer, request: RenderRequest, sd_library: &ReaderStore) {
    if request.view == AppView::Reading && request.book_id >= 2 {
        fb.clear(true);
        draw_sd_reader_page(fb, request, sd_library);
        mirror_framebuffer_long_axis(fb);
    } else {
        let mut library_entries = [""; MAX_LIBRARY_BOOKS];
        let mut chapters = [UiTocItem {
            title: "",
            level: 1,
        }; MAX_UI_CHAPTERS];
        let model = ui_model(request, sd_library, &mut library_entries, &mut chapters);
        app_render::render_request(fb, request, &model);
    }

    if SHOW_INPUT_DEBUG {
        draw_input_sample(fb, request);
    }
}

pub(crate) fn render_sleep(fb: &mut Framebuffer, request: RenderRequest, sd_library: &ReaderStore) {
    let mut library_entries = [""; MAX_LIBRARY_BOOKS];
    let mut chapters = [UiTocItem {
        title: "",
        level: 1,
    }; MAX_UI_CHAPTERS];
    let model = ui_model(request, sd_library, &mut library_entries, &mut chapters);
    app_render::render_sleep(fb, request, &model);
}

fn ui_model<'a>(
    request: RenderRequest,
    sd_library: &'a ReaderStore,
    library_entries: &'a mut [&'a str; MAX_LIBRARY_BOOKS],
    chapters: &'a mut [UiTocItem<'a>; MAX_UI_CHAPTERS],
) -> UiRenderModel<'a> {
    let library_count = sd_library.catalog_count().min(library_entries.len());
    for (index, entry) in sd_library
        .catalog_entries()
        .iter()
        .take(library_count)
        .enumerate()
    {
        library_entries[index] = entry.display_name.as_str();
    }
    let chapter_count = fill_chapters(chapters, request, sd_library);

    let fallback_book = catalog::active_book(request.book_id);
    let (title, author) =
        sd_library.active_book_labels(request.book_id, fallback_book.title, fallback_book.author);

    UiRenderModel {
        active_book: UiBook {
            title,
            author,
            progress_permille: book_progress_permille(request),
            cover: if let Some(cover) = sd_library.selected_cover(request.book_id) {
                Some(UiCover {
                    width: cover.width,
                    height: cover.height,
                    stride: cover.stride,
                    bits: cover.bits,
                })
            } else {
                None
            },
        },
        library_status: ui_library_status(sd_library.status),
        library_entries: &library_entries[..library_count],
        chapters: &chapters[..chapter_count],
    }
}

fn fill_chapters<'a>(
    chapters: &mut [UiTocItem<'a>; MAX_UI_CHAPTERS],
    request: RenderRequest,
    sd_library: &'a ReaderStore,
) -> usize {
    if request.book_id >= 2 && sd_library.toc_count() > 0 {
        let count = sd_library.toc_count().min(chapters.len());
        for (index, item) in chapters.iter_mut().take(count).enumerate() {
            if let Some(toc_item) = sd_library.toc_item(index) {
                *item = UiTocItem {
                    title: toc_item.title,
                    level: toc_item.level,
                };
            }
        }
        return count;
    }

    let count = (catalog::chapter_count() as usize).min(chapters.len());
    for (index, item) in chapters.iter_mut().take(count).enumerate() {
        if let Some(chapter) = catalog::chapter_at(index) {
            *item = UiTocItem {
                title: chapter.title,
                level: 1,
            };
        }
    }
    count
}

fn ui_library_status(status: LibraryScanStatus) -> UiLibraryStatus {
    match status {
        LibraryScanStatus::NotScanned => UiLibraryStatus::NotScanned,
        LibraryScanStatus::Scanning => UiLibraryStatus::Scanning,
        LibraryScanStatus::Ready => UiLibraryStatus::Ready,
        LibraryScanStatus::Empty => UiLibraryStatus::Empty,
        LibraryScanStatus::Error => UiLibraryStatus::Error,
    }
}

fn draw_sd_reader_page(fb: &mut Framebuffer, request: RenderRequest, sd_library: &ReaderStore) {
    match sd_library.reader_status() {
        BookLoadStatus::Empty | BookLoadStatus::Loading => {
            draw_ascii(fb, "OPENING EPUB", 20, 72, false);
        }
        BookLoadStatus::Error => {
            draw_ascii(fb, "COULD NOT OPEN EPUB", 20, 72, false);
            draw_ascii(fb, sd_library.reader_error(), 20, 104, false);
        }
        BookLoadStatus::Ready => {
            let page_top = 22i16;
            let page_bottom = 472i16;
            let page_count = reader_layout::reader_page_count(sd_library, page_top, page_bottom);
            let requested_page = request.page.min(page_count - 1) as usize;
            let page =
                reader_layout::reader_page_at(sd_library, requested_page, page_top, page_bottom);
            let mut y = page_bottom - 8;

            for offset in 0..page.block_count as usize {
                let index = page.first_block as usize + offset;
                let Some(record) = sd_library.block_record(index) else {
                    break;
                };
                let role = record.role;
                let align = record.align;
                let text = sd_library.block_text(index);
                let advance = reader_layout::line_advance_for(role);
                let style = sd_library.block_style(index);
                let font = literata(style);
                if y < page_top {
                    break;
                }

                match align {
                    TextAlign::Left => {
                        let x = reader_layout::reader_x_for(role);
                        if record.line_count == 1 {
                            reader_layout::draw_styled_line(fb, text, x, y, style);
                        } else {
                            reader_layout::draw_wrapped_literata(
                                fb,
                                font,
                                text,
                                x,
                                y,
                                reader_layout::reader_max_x_for(role, align),
                                advance,
                            );
                        }
                    }
                    TextAlign::Justify => {
                        let x = reader_layout::reader_x_for(role);
                        if record.line_count == 1 {
                            reader_layout::draw_styled_line(fb, text, x, y, style);
                        } else {
                            reader_layout::draw_justified_wrapped_literata(
                                fb,
                                font,
                                text,
                                x,
                                y,
                                reader_layout::reader_max_x_for(role, align),
                                advance,
                            );
                        }
                    }
                    TextAlign::Center => {
                        if record.line_count == 1 {
                            let width = reader_layout::styled_text_ink_width(text, font)
                                .min(READER_RIGHT_X - READER_LEFT_X);
                            let x = ((WIDTH as i16 - width) / 2).max(READER_LEFT_X);
                            reader_layout::draw_styled_line(fb, text, x, y, style);
                        } else {
                            reader_layout::draw_centered_wrapped_literata(
                                fb,
                                font,
                                text,
                                y,
                                READER_RIGHT_X - READER_LEFT_X,
                                advance,
                            );
                        }
                    }
                };
                y -= advance + reader_layout::paragraph_gap_after(sd_library, index);
            }
        }
    }
}

fn draw_input_sample(fb: &mut Framebuffer, request: RenderRequest) {
    fill_rect(fb, Rect::new(488, 104, 220, 64), true);
    stroke_rect(fb, Rect::new(488, 104, 220, 64), false);
    draw_ascii(fb, "LAST", 504, 120, false);
    draw_ascii(fb, button_label(request.last_button), 552, 120, false);
}

fn mirror_framebuffer_long_axis(fb: &mut Framebuffer) {
    for y in 0..display::HEIGHT / 2 {
        let other_y = display::HEIGHT - 1 - y;
        for x in 0..display::WIDTH {
            let top = fb.pixel(x, y);
            let bottom = fb.pixel(x, other_y);
            fb.set_pixel(x, y, bottom);
            fb.set_pixel(x, other_y, top);
        }
    }
}

fn button_label(button: Option<crate::Button>) -> &'static str {
    match button {
        Some(crate::Button::Power) => "POWER",
        Some(crate::Button::Back) => "BACK",
        Some(crate::Button::Confirm) => "OK",
        Some(crate::Button::Previous) => "PREV",
        Some(crate::Button::Next) => "NEXT",
        None => "NONE",
    }
}

fn book_progress_permille(request: RenderRequest) -> u16 {
    let chapters = catalog::chapter_count().max(1) as u32;
    ((request.chapter as u32 * 1000) / chapters.saturating_sub(1).max(1)) as u16
}
