use crate::{
    catalog, AppView, DisplayCommand, DisplayEvent, DisplayOrientation, LibraryEvent, PowerEvent,
    RefreshPolicy, RenderRequest, DISPLAY_COMMANDS, DISPLAY_EVENTS, LIBRARY_EVENTS, POWER_EVENTS,
};
use display::epd::{
    ram_x_counter, ram_x_range, ram_y_counter, ram_y_range, update_control_1, update_control_2,
    RefreshMode, SpiOp, CMD_DEEP_SLEEP, CMD_DISPLAY_UPDATE_CTRL1, CMD_DISPLAY_UPDATE_CTRL2,
    CMD_MASTER_ACTIVATION, CMD_SET_RAM_X_COUNTER, CMD_SET_RAM_X_RANGE, CMD_SET_RAM_Y_COUNTER,
    CMD_SET_RAM_Y_RANGE, CMD_WRITE_RAM_BW, CMD_WRITE_RAM_RED, INIT_SEQUENCE,
};
use display::fb::Framebuffer;
use display::font::{draw_text_mirrored_y_glyphs, literata, measure_text, BitmapFont, FontStyle};
use display::render::{draw_ascii, fill_rect, glyph_5x7, stroke_rect};
use display::{Rect, BAND_BYTES, BAND_ROWS, HEIGHT, ROW_BYTES, WIDTH};
use embassy_time::Instant;
use embedded_hal::delay::DelayNs;
use embedded_hal::digital::OutputPin;
use embedded_hal::spi::{Operation, SpiBus as BlockingSpiBus, SpiDevice};
use embedded_sdmmc::{LfnBuffer, SdCard, TimeSource, Timestamp, VolumeIdx, VolumeManager};
use esp_hal::gpio::{Input, Output};
use esp_hal::peripherals::SPI2;
use esp_hal::prelude::*;
use esp_hal::spi::master::SpiDmaBus;
use esp_hal::spi::FullDuplexMode;
use esp_hal::Async;
use heapless::String;

type Epd = hal_ext::spi_dma::EpdBus<
    SpiDmaBus<'static, SPI2, FullDuplexMode, Async>,
    Output<'static>,
    Output<'static>,
    Input<'static>,
    Output<'static>,
>;

const MIRROR_X: bool = true;
const MIRROR_Y: bool = false;
const REVERSE_BITS: bool = true;
const SHOW_INPUT_DEBUG: bool = false;
const FAST_REFRESH_ENABLED: bool = true;
const PERIODIC_FULL_REFRESH_ENABLED: bool = false;
const FULL_REFRESH_INTERVAL: u8 = 8;
const SHELL_ORIENTATION: DisplayOrientation = DisplayOrientation::PortraitButtonsLeft;
const MAX_SD_BOOKS: usize = 8;

const HOME_ITEMS: [&str; 4] = ["READ", "FILES", "SYNC", "SETTINGS"];
const SETTINGS_ITEMS: [&str; 3] = ["ORIENTATION", "REFRESH", "BACK TO HOME"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SdStatus {
    NotScanned,
    Scanning,
    Ready,
    Empty,
    Error,
}

struct SdLibrary {
    status: SdStatus,
    names: [String<64>; MAX_SD_BOOKS],
    count: usize,
}

impl SdLibrary {
    fn new() -> Self {
        Self {
            status: SdStatus::NotScanned,
            names: core::array::from_fn(|_| String::new()),
            count: 0,
        }
    }

    fn clear(&mut self) {
        self.count = 0;
        for name in self.names.iter_mut() {
            name.clear();
        }
    }

    fn push(&mut self, name: &str) {
        if self.count >= self.names.len() {
            return;
        }
        let slot = &mut self.names[self.count];
        slot.clear();
        let _ = slot.push_str(name);
        self.count += 1;
    }
}

struct StaticTime;

impl TimeSource for StaticTime {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp {
            year_since_1970: 56,
            zero_indexed_month: 4,
            zero_indexed_day: 19,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

struct SdSpiDevice<'a, SPI, CS> {
    spi: &'a mut SPI,
    cs: &'a mut CS,
    delay: esp_hal::delay::Delay,
}

impl<SPI, CS> embedded_hal::spi::ErrorType for SdSpiDevice<'_, SPI, CS>
where
    SPI: embedded_hal::spi::ErrorType,
{
    type Error = SPI::Error;
}

impl<SPI, CS> SpiDevice for SdSpiDevice<'_, SPI, CS>
where
    SPI: BlockingSpiBus<u8>,
    CS: OutputPin,
{
    fn transaction(&mut self, operations: &mut [Operation<'_, u8>]) -> Result<(), Self::Error> {
        let _ = self.cs.set_low();
        let mut result = Ok(());

        for operation in operations {
            result = match operation {
                Operation::Read(buffer) => self.spi.read(buffer),
                Operation::Write(buffer) => self.spi.write(buffer),
                Operation::Transfer(read, write) => self.spi.transfer(read, write),
                Operation::TransferInPlace(buffer) => self.spi.transfer_in_place(buffer),
                Operation::DelayNs(ns) => {
                    self.delay.delay_ns(*ns);
                    Ok(())
                }
            };

            if result.is_err() {
                break;
            }
        }

        let _ = self.spi.flush();
        let _ = self.cs.set_high();
        result
    }
}

#[embassy_executor::task]
pub async fn run(mut epd: Epd, mut sd_cs: Output<'static>) {
    esp_println::println!("display: started");

    static FB: static_cell::StaticCell<Framebuffer> = static_cell::StaticCell::new();
    let fb = FB.init(Framebuffer::new());
    static PREV_FB: static_cell::StaticCell<Framebuffer> = static_cell::StaticCell::new();
    let prev_fb = PREV_FB.init(Framebuffer::new());
    static TX_BAND: static_cell::StaticCell<[u8; BAND_BYTES]> = static_cell::StaticCell::new();
    let tx_band = TX_BAND.init([0; BAND_BYTES]);

    esp_println::println!("display: init start");
    init_panel(&mut epd).await;
    esp_println::println!("display: init complete");

    let mut screen_on = false;
    let mut fast_refreshes = 0u8;
    let mut sd_library = SdLibrary::new();
    loop {
        match DISPLAY_COMMANDS.receive().await {
            DisplayCommand::Render(request) => {
                if request.view == AppView::Library && sd_library.status == SdStatus::NotScanned {
                    sd_library.status = SdStatus::Scanning;
                    scan_sd_books(&mut epd, &mut sd_cs, &mut sd_library);
                    let _ = LIBRARY_EVENTS.try_send(LibraryEvent::Scanned {
                        count: sd_library.count.min(u8::MAX as usize) as u8,
                    });
                }
                render(fb, request, &sd_library);

                let mode = refresh_mode(screen_on, fast_refreshes);
                if flush(&mut epd, fb, prev_fb, tx_band, screen_on, mode)
                    .await
                    .is_ok()
                {
                    screen_on = true;
                    if mode == RefreshMode::Fast {
                        fast_refreshes = fast_refreshes.saturating_add(1);
                    } else {
                        fast_refreshes = 0;
                    }
                    prev_fb.copy_from(fb);
                    let _ = DISPLAY_EVENTS.try_send(DisplayEvent::Settled);
                    let _ = POWER_EVENTS.send(PowerEvent::DisplaySettled).await;
                } else {
                    esp_println::println!("display: SPI transfer failed");
                }
            }
            DisplayCommand::Sleep => {
                if sleep_panel(&mut epd).await.is_ok() {
                    screen_on = false;
                    fast_refreshes = 0;
                    let _ = POWER_EVENTS.send(PowerEvent::DisplayAsleep).await;
                } else {
                    esp_println::println!("display: sleep command failed");
                    let _ = POWER_EVENTS.send(PowerEvent::DisplayAsleep).await;
                }
            }
        }
    }
}

fn refresh_mode(screen_on: bool, fast_refreshes: u8) -> RefreshMode {
    let cleanup_due = PERIODIC_FULL_REFRESH_ENABLED && fast_refreshes >= FULL_REFRESH_INTERVAL;
    if FAST_REFRESH_ENABLED && screen_on && !cleanup_due {
        RefreshMode::Fast
    } else {
        RefreshMode::Full
    }
}

fn scan_sd_books(epd: &mut Epd, sd_cs: &mut Output<'static>, library: &mut SdLibrary) {
    esp_println::println!("sd: scan start");
    library.clear();
    epd.deselect_display();
    sd_cs.set_high();
    epd.spi_mut().change_bus_frequency(400_u32.kHz());

    let startup_clocks = [0xFF; 10];
    if BlockingSpiBus::write(epd.spi_mut(), &startup_clocks).is_err() {
        esp_println::println!("sd: startup clocks failed");
        epd.spi_mut().change_bus_frequency(40_u32.MHz());
        library.status = SdStatus::Error;
        return;
    }

    let status = 'scan: {
        let spi = SdSpiDevice {
            spi: epd.spi_mut(),
            cs: sd_cs,
            delay: esp_hal::delay::Delay::new(),
        };
        let card = SdCard::new(spi, esp_hal::delay::Delay::new());
        match card.num_bytes() {
            Ok(bytes) => esp_println::println!("sd: card size {} bytes", bytes),
            Err(err) => {
                esp_println::println!("sd: card init failed: {:?}", err);
                break 'scan SdStatus::Error;
            }
        }

        card.spi(|device| device.spi.change_bus_frequency(8_u32.MHz()));
        let volume_mgr: VolumeManager<_, _, 4, 4, 1> = VolumeManager::new(card, StaticTime);
        let volume = match volume_mgr.open_volume(VolumeIdx(0)) {
            Ok(volume) => volume,
            Err(err) => {
                esp_println::println!("sd: open volume failed: {:?}", err);
                break 'scan SdStatus::Error;
            }
        };
        let root = match volume.open_root_dir() {
            Ok(root) => root,
            Err(err) => {
                esp_println::println!("sd: open root failed: {:?}", err);
                break 'scan SdStatus::Error;
            }
        };

        if let Ok(books) = root.open_dir("BOOKS") {
            collect_epubs(&books, "/books/", library);
        }
        if library.count == 0 {
            collect_epubs(&root, "/", library);
        }

        if library.count == 0 {
            SdStatus::Empty
        } else {
            SdStatus::Ready
        }
    };
    epd.spi_mut().change_bus_frequency(40_u32.MHz());
    library.status = status;
    esp_println::println!("sd: scan complete, {} epub(s)", library.count);
}

fn collect_epubs<D, T, const MAX_DIRS: usize, const MAX_FILES: usize, const MAX_VOLUMES: usize>(
    dir: &embedded_sdmmc::Directory<'_, D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
    prefix: &str,
    library: &mut SdLibrary,
) where
    D: embedded_sdmmc::BlockDevice,
    T: TimeSource,
{
    let mut lfn_storage = [0u8; 192];
    let mut lfn_buffer = LfnBuffer::new(&mut lfn_storage);
    let _ = dir.iterate_dir_lfn(&mut lfn_buffer, |entry, long_name| {
        if entry.attributes.is_directory() || entry.attributes.is_volume() {
            return;
        }

        let mut name = String::<64>::new();
        let Some(file_name) = long_name else {
            use core::fmt::Write;
            let _ = write!(name, "{}", entry.name);
            if !is_epub_name(&name) {
                return;
            }
            push_prefixed(prefix, &name, library);
            return;
        };

        if is_epub_name(file_name) {
            push_prefixed(prefix, file_name, library);
        }
    });
}

fn push_prefixed(prefix: &str, name: &str, library: &mut SdLibrary) {
    let mut path = String::<64>::new();
    let _ = path.push_str(prefix);
    let _ = path.push_str(name);
    library.push(&path);
}

fn is_epub_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() < 5 {
        return false;
    }
    let ext = &bytes[bytes.len() - 5..];
    ext[0] == b'.'
        && ext[1].eq_ignore_ascii_case(&b'e')
        && ext[2].eq_ignore_ascii_case(&b'p')
        && ext[3].eq_ignore_ascii_case(&b'u')
        && ext[4].eq_ignore_ascii_case(&b'b')
}

async fn init_panel(epd: &mut Epd) {
    for op in INIT_SEQUENCE {
        match *op {
            SpiOp::Reset => epd.reset().await,
            SpiOp::WaitBusy => epd.wait_ready().await,
            SpiOp::Command { cmd, data } => {
                epd.command(cmd, data).await.unwrap();
            }
        }
    }
}

fn render(fb: &mut Framebuffer, request: RenderRequest, sd_library: &SdLibrary) {
    fb.clear(true);
    render_reader_shell(fb, request, sd_library);
    if SHOW_INPUT_DEBUG {
        draw_input_sample(fb, request);
    }
}

fn draw_input_sample(fb: &mut Framebuffer, request: RenderRequest) {
    fill_rect(fb, Rect::new(488, 104, 220, 64), true);
    stroke_rect(fb, Rect::new(488, 104, 220, 64), false);
    draw_ascii(fb, "LAST", 504, 120, false);
    draw_ascii(fb, button_label(request.last_button), 552, 120, false);

    if SHOW_INPUT_DEBUG {
        let mut aux_buf = [0u8; 10];
        let mut nav_buf = [0u8; 10];
        let mut page_buf = [0u8; 10];
        draw_ascii(fb, "GPIO0", 504, 144, false);
        draw_ascii(
            fb,
            fmt_u32(request.aux_raw as u32, &mut aux_buf),
            568,
            144,
            false,
        );
        draw_ascii(fb, "GPIO1", 504, 168, false);
        draw_ascii(
            fb,
            fmt_u32(request.nav_raw as u32, &mut nav_buf),
            568,
            168,
            false,
        );
        draw_ascii(fb, "GPIO2", 504, 192, false);
        draw_ascii(
            fb,
            fmt_u32(request.page_raw as u32, &mut page_buf),
            568,
            192,
            false,
        );
    }
}

fn render_reader_shell(fb: &mut Framebuffer, request: RenderRequest, sd_library: &SdLibrary) {
    if is_shell_view(request.view) {
        render_shell_portrait(fb, request, sd_library);
        return;
    }

    if request.view == AppView::Reading {
        render_reading_landscape(fb, request);
        return;
    }

    stroke_rect(fb, Rect::new(0, 0, WIDTH as u16, HEIGHT as u16), false);
    draw_header(fb, request);
    draw_body(fb, request);
    draw_footer(fb, request);
}

fn is_shell_view(view: AppView) -> bool {
    matches!(
        view,
        AppView::Home | AppView::Library | AppView::Sync | AppView::Settings
    )
}

fn render_shell_portrait(fb: &mut Framebuffer, request: RenderRequest, sd_library: &SdLibrary) {
    let mut ui = Ui::new(fb, SHELL_ORIENTATION);

    match request.view {
        AppView::Home => draw_home_portrait(&mut ui, request),
        AppView::Library => draw_library_portrait(&mut ui, request, sd_library),
        AppView::Sync => draw_sync_portrait(&mut ui),
        AppView::Settings => draw_settings_portrait(&mut ui, request),
        AppView::Reading | AppView::Chapters => {}
    }
}

fn render_reading_landscape(fb: &mut Framebuffer, request: RenderRequest) {
    draw_reader_page(fb, request);
    draw_reading_footer(fb, request);
}

fn draw_home_portrait(ui: &mut Ui<'_>, request: RenderRequest) {
    let book = catalog::active_book(request.book_id);
    draw_home_status(ui, request);
    draw_cover_placeholder_ui(ui, 68, 264, 344, 474);
    draw_progress_bar_ui(ui, 142, 238, 196, 6, 420);
    ui.draw_ascii(book.title, centered_x_for(480, book.title), 202, false);
    ui.draw_ascii(book.author, centered_x_for(480, book.author), 176, false);
    draw_home_soft_keys(ui);
}

fn draw_home_status(ui: &mut Ui<'_>, request: RenderRequest) {
    ui.draw_ascii("XTEINK", 36, 744, false);
    let mut buf = [0u8; 10];
    ui.draw_ascii(
        fmt_percent(request.battery_percent, &mut buf),
        366,
        746,
        false,
    );
    draw_battery_icon_ui(ui, 404, 744, battery_bars(request.battery_percent));
}

fn draw_home_soft_keys(ui: &mut Ui<'_>) {
    let tab_y = 0;
    let tab_h = 58;
    let tab_w = 120;
    let mut x = 0;
    for item in HOME_ITEMS {
        ui.stroke_rect(x, tab_y, tab_w, tab_h, false);
        ui.draw_ascii(
            item,
            x as usize + centered_x_for(tab_w as usize, item),
            tab_y as usize + 24,
            false,
        );
        x += tab_w;
    }
}

fn draw_menu_portrait(ui: &mut Ui<'_>, title: &str, items: &[&str], selection: u8) {
    ui.draw_ascii(title, 64, 72, false);
    ui.fill_rect(64, 110, 352, 2, false);
    let mut y = 172;
    for (index, item) in items.iter().enumerate() {
        let selected = index == selection as usize;
        if selected {
            ui.fill_rect(56, y - 12, 368, 32, false);
        }
        ui.draw_ascii(if selected { ">" } else { " " }, 76, y as usize, selected);
        ui.draw_ascii(item, 112, y as usize, selected);
        y += 48;
    }
}

fn draw_library_portrait(ui: &mut Ui<'_>, request: RenderRequest, sd_library: &SdLibrary) {
    ui.draw_ascii("FILES", 64, 72, false);
    ui.fill_rect(64, 110, 352, 2, false);
    ui.draw_ascii("/books then /", 64, 132, false);

    match sd_library.status {
        SdStatus::NotScanned | SdStatus::Scanning => {
            ui.draw_ascii("SCANNING MICROSD", 64, 216, false);
            return;
        }
        SdStatus::Error => {
            ui.draw_ascii("MICROSD NOT READY", 64, 216, false);
            ui.draw_ascii("USE FAT16/FAT32", 64, 248, false);
            return;
        }
        SdStatus::Empty => {
            ui.draw_ascii("NO EPUB FILES FOUND", 64, 216, false);
            ui.draw_ascii("PUT BOOKS IN /books", 64, 248, false);
            return;
        }
        SdStatus::Ready => {}
    }

    if sd_library.count == 0 {
        ui.draw_ascii("NO EPUB FILES FOUND", 64, 216, false);
        return;
    }

    let mut y = 198;
    for index in 0..sd_library.count {
        let selected = index == request.selection as usize;
        if selected {
            ui.fill_rect(56, y - 12, 368, 32, false);
        }
        ui.draw_ascii(if selected { ">" } else { " " }, 76, y as usize, selected);
        ui.draw_ascii(&sd_library.names[index], 112, y as usize, selected);
        y += 48;
    }
}

fn draw_settings_portrait(ui: &mut Ui<'_>, request: RenderRequest) {
    draw_menu_portrait(ui, "SETTINGS", &SETTINGS_ITEMS, request.selection);
    ui.draw_ascii("READING ORIENTATION", 64, 380, false);
    ui.draw_ascii(orientation_label(request.orientation), 64, 408, false);
    ui.draw_ascii("REFRESH", 64, 464, false);
    ui.draw_ascii(refresh_policy_label(request.refresh_policy), 64, 492, false);
}

fn draw_sync_portrait(ui: &mut Ui<'_>) {
    ui.draw_ascii("SYNC", centered_x_for(480, "SYNC"), 300, false);
    ui.draw_ascii(
        "NOT CONFIGURED",
        centered_x_for(480, "NOT CONFIGURED"),
        344,
        false,
    );
    ui.draw_ascii("BACK", centered_x_for(480, "BACK"), 620, false);
}

fn draw_cover_placeholder_ui(ui: &mut Ui<'_>, x: u16, y: u16, w: u16, h: u16) {
    ui.stroke_rect(x, y, w, h, false);
    ui.stroke_rect(x + 8, y + 8, w - 16, h - 16, false);
    ui.fill_rect(x + 24, y + 38, 2, h - 76, false);
    ui.draw_ascii("BRING", x as usize + 90, y as usize + 126, false);
    ui.draw_ascii("UP", x as usize + 106, y as usize + 158, false);
    ui.draw_ascii("NOTES", x as usize + 94, y as usize + 190, false);
}

fn draw_battery_icon_ui(ui: &mut Ui<'_>, x: u16, y: u16, bars: u8) {
    ui.stroke_rect(x, y, 36, 16, false);
    ui.fill_rect(x + 36, y + 5, 4, 6, false);
    for bar in 0..bars.min(4) {
        ui.fill_rect(x + 4 + bar as u16 * 8, y + 4, 5, 8, false);
    }
}

fn draw_header(fb: &mut Framebuffer, request: RenderRequest) {
    draw_ascii(fb, "XTEINK X4", 32, 28, false);
    draw_ascii(fb, view_label(request.view), 328, 28, false);
    draw_ascii(fb, orientation_label(request.orientation), 576, 28, false);
    draw_rule(fb, 64);
}

fn draw_body(fb: &mut Framebuffer, request: RenderRequest) {
    match request.view {
        AppView::Home => draw_home(fb, request),
        AppView::Library => draw_library_landscape(fb, request),
        AppView::Reading => draw_reader_page(fb, request),
        AppView::Chapters => draw_chapters(fb, request),
        AppView::Sync => {}
        AppView::Settings => draw_settings(fb, request),
    }
}

fn draw_footer(fb: &mut Framebuffer, request: RenderRequest) {
    if request.view == AppView::Reading {
        draw_reading_footer(fb, request);
    } else {
        draw_rule(fb, 408);
        draw_ascii(fb, "PREV NEXT", 32, 432, false);
        draw_ascii(fb, "OK SELECT", 344, 432, false);
        draw_ascii(fb, "BACK", 656, 432, false);
    }
}

fn draw_home(fb: &mut Framebuffer, request: RenderRequest) {
    let book = catalog::active_book(request.book_id);
    draw_cover_placeholder(fb, 92, 112, 184, 248);
    draw_ascii(fb, book.title, 344, 116, false);
    draw_ascii(fb, book.author, 344, 144, false);
    draw_progress_bar(fb, Rect::new(344, 188, 320, 10), 420);
    draw_ascii(
        fb,
        catalog::chapter_at(request.selection as usize)
            .map(|chapter| chapter.title)
            .unwrap_or("Chapter"),
        344,
        216,
        false,
    );

    let mut y = 268;
    for (index, item) in HOME_ITEMS.iter().enumerate() {
        let selected = index == request.selection as usize;
        if selected {
            fill_rect(fb, Rect::new(332, y as u16 - 10, 300, 28), false);
        }
        draw_ascii(fb, if selected { ">" } else { " " }, 348, y, selected);
        draw_ascii(fb, item, 380, y, selected);
        y += 40;
    }
}

fn draw_cover_placeholder(fb: &mut Framebuffer, x: u16, y: u16, w: u16, h: u16) {
    stroke_rect(fb, Rect::new(x, y, w, h), false);
    stroke_rect(fb, Rect::new(x + 8, y + 8, w - 16, h - 16), false);
    fill_rect(fb, Rect::new(x + 24, y + 34, 2, h - 68), false);
    draw_ascii(fb, "XTEINK", x as usize + 64, y as usize + 88, false);
    draw_ascii(fb, "BRING UP", x as usize + 48, y as usize + 120, false);
    draw_ascii(fb, "NOTES", x as usize + 72, y as usize + 152, false);
}

fn draw_reader_page(fb: &mut Framebuffer, request: RenderRequest) {
    let index = (request.chapter as usize) % catalog::READER_PAGES.len();
    let page = catalog::READER_PAGES[index];
    let mut baseline_y = 72i16;

    for line in page.iter() {
        let (style, x, advance) = match line.style {
            catalog::ReaderLineStyle::Heading => (FontStyle::Bold, 18, 34),
            catalog::ReaderLineStyle::Body => (FontStyle::Regular, 20, 28),
            catalog::ReaderLineStyle::Italic => (FontStyle::Italic, 20, 28),
            catalog::ReaderLineStyle::Bold => (FontStyle::Bold, 20, 28),
            catalog::ReaderLineStyle::Quote => (FontStyle::Italic, 46, 28),
        };
        let font = literata(style);
        baseline_y = draw_wrapped_literata(fb, font, line.text, x, baseline_y, 728, advance);
        baseline_y += line.gap_after as i16;
    }
}

fn draw_wrapped_literata(
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
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor] == b' ' {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            break;
        }

        let mut end = cursor;
        let mut best = cursor;
        while end < bytes.len() {
            while end < bytes.len() && bytes[end] != b' ' {
                end += 1;
            }
            let candidate = &text[cursor..end];
            if x + measure_text(font, candidate) as i16 > max_x {
                break;
            }
            best = end;
            while end < bytes.len() && bytes[end] == b' ' {
                end += 1;
            }
        }

        if best == cursor {
            best = next_word_end(bytes, cursor);
        }

        draw_text_mirrored_y_glyphs(fb, font, &text[cursor..best], x, baseline_y, false);
        baseline_y += line_advance;
        cursor = best;
    }

    baseline_y
}

fn next_word_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len() && bytes[end] != b' ' {
        end += 1;
    }
    end
}

fn draw_reading_footer(fb: &mut Framebuffer, request: RenderRequest) {
    let book = catalog::active_book(request.book_id);
    fill_rect(fb, Rect::new(16, 454, 768, 2), false);
    draw_ascii(fb, book.title, 16, 462, false);

    let mut screen_buf = [0u8; 10];
    let mut total_buf = [0u8; 10];
    draw_ascii(
        fb,
        fmt_u32(request.page + 1, &mut screen_buf),
        376,
        462,
        false,
    );
    draw_ascii(fb, "/", 400, 462, false);
    draw_ascii(fb, fmt_u32(1, &mut total_buf), 416, 462, false);

    draw_battery_icon(fb, 728, 460, battery_bars(request.battery_percent));
    draw_progress_bar(
        fb,
        Rect::new(16, 448, 768, 3),
        book_progress_permille(request),
    );
}

fn draw_library_landscape(fb: &mut Framebuffer, request: RenderRequest) {
    draw_ascii(fb, "FILES", 96, 112, false);
    draw_ascii(fb, "/books/*.epub", 96, 144, false);
    let mut item_y = 204;
    for index in 0..catalog::book_count() as usize {
        let Some(book) = catalog::book_at(index) else {
            continue;
        };
        let selected = index == request.selection as usize;
        if selected {
            fill_rect(fb, Rect::new(88, item_y as u16 - 10, 624, 28), false);
        }
        draw_ascii(fb, if selected { ">" } else { " " }, 104, item_y, selected);
        draw_ascii(fb, book.title, 136, item_y, selected);
        item_y += 44;
    }
}

fn draw_chapters(fb: &mut Framebuffer, request: RenderRequest) {
    draw_ascii(fb, "CHAPTERS", 96, 112, false);
    let mut item_y = 168;
    for index in 0..catalog::chapter_count() as usize {
        let Some(chapter) = catalog::chapter_at(index) else {
            continue;
        };
        let selected = index == request.selection as usize;
        if selected {
            fill_rect(fb, Rect::new(88, item_y as u16 - 10, 624, 28), false);
        }
        draw_ascii(fb, if selected { ">" } else { " " }, 104, item_y, selected);
        draw_ascii(fb, chapter.title, 136, item_y, selected);
        item_y += 44;
    }
}

fn draw_menu(fb: &mut Framebuffer, title: &str, items: &[&str], selection: u8, y: usize) {
    draw_ascii(fb, title, 96, y, false);
    let mut item_y = y + 56;
    for (index, item) in items.iter().enumerate() {
        let selected = index == selection as usize;
        if selected {
            fill_rect(fb, Rect::new(88, item_y as u16 - 10, 624, 28), false);
        }
        draw_ascii(fb, if selected { ">" } else { " " }, 104, item_y, selected);
        draw_ascii(fb, item, 136, item_y, selected);
        item_y += 44;
    }
}

fn draw_settings(fb: &mut Framebuffer, request: RenderRequest) {
    draw_menu(fb, "SETTINGS", &SETTINGS_ITEMS, request.selection, 96);
    draw_ascii(fb, "CURRENT", 96, 292, false);
    draw_ascii(fb, orientation_label(request.orientation), 200, 292, false);
    draw_ascii(fb, "REFRESH", 96, 324, false);
    draw_ascii(
        fb,
        refresh_policy_label(request.refresh_policy),
        200,
        324,
        false,
    );
}

fn draw_progress_bar(fb: &mut Framebuffer, rect: Rect, permille: u16) {
    stroke_rect(fb, rect, false);
    let inner_w = rect.w.saturating_sub(4);
    let fill_w = ((inner_w as u32 * permille.min(1000) as u32) / 1000) as u16;
    if fill_w > 0 {
        let fill_h = rect.h.saturating_sub(4).max(1);
        let fill_y = if rect.h > 4 { rect.y + 2 } else { rect.y + 1 };
        fill_rect(fb, Rect::new(rect.x + 2, fill_y, fill_w, fill_h), false);
    }
}

fn draw_battery_icon(fb: &mut Framebuffer, x: u16, y: u16, bars: u8) {
    stroke_rect(fb, Rect::new(x, y, 36, 16), false);
    fill_rect(fb, Rect::new(x + 36, y + 5, 4, 6), false);
    for bar in 0..bars.min(4) {
        fill_rect(fb, Rect::new(x + 4 + bar as u16 * 8, y + 4, 5, 8), false);
    }
}

fn battery_bars(percent: u8) -> u8 {
    match percent {
        0..=10 => 0,
        11..=35 => 1,
        36..=60 => 2,
        61..=85 => 3,
        _ => 4,
    }
}

fn draw_progress_bar_ui(ui: &mut Ui<'_>, x: u16, y: u16, w: u16, h: u16, permille: u16) {
    ui.stroke_rect(x, y, w, h, false);
    let inner_w = w.saturating_sub(4);
    let fill_w = ((inner_w as u32 * permille.min(1000) as u32) / 1000) as u16;
    if fill_w > 0 {
        let fill_h = h.saturating_sub(4).max(1);
        let fill_y = if h > 4 { y + 2 } else { y + 1 };
        ui.fill_rect(x + 2, fill_y, fill_w, fill_h, false);
    }
}

fn book_progress_permille(request: RenderRequest) -> u16 {
    let chapters = catalog::chapter_count().max(1) as u32;
    ((request.chapter as u32 * 1000) / chapters.saturating_sub(1).max(1)) as u16
}

fn draw_rule(fb: &mut Framebuffer, y: usize) {
    fill_rect(fb, Rect::new(32, y as u16, 736, 2), false);
}

fn centered_x_for(width: usize, text: &str) -> usize {
    width.saturating_sub(text.len() * 8) / 2
}

fn view_label(view: AppView) -> &'static str {
    match view {
        AppView::Home => "HOME",
        AppView::Library => "LIBRARY",
        AppView::Reading => "READING",
        AppView::Chapters => "CHAPTERS",
        AppView::Sync => "SYNC",
        AppView::Settings => "SETTINGS",
    }
}

struct Ui<'a> {
    fb: &'a mut Framebuffer,
    orientation: DisplayOrientation,
}

impl<'a> Ui<'a> {
    fn new(fb: &'a mut Framebuffer, orientation: DisplayOrientation) -> Self {
        Self { fb, orientation }
    }

    fn fill_rect(&mut self, x: u16, y: u16, w: u16, h: u16, white: bool) {
        for yy in y..y.saturating_add(h) {
            for xx in x..x.saturating_add(w) {
                self.set_pixel(xx as usize, yy as usize, white);
            }
        }
    }

    fn stroke_rect(&mut self, x: u16, y: u16, w: u16, h: u16, white: bool) {
        if w == 0 || h == 0 {
            return;
        }
        let x1 = x + w - 1;
        let y1 = y + h - 1;
        for xx in x..=x1 {
            self.set_pixel(xx as usize, y as usize, white);
            self.set_pixel(xx as usize, y1 as usize, white);
        }
        for yy in y..=y1 {
            self.set_pixel(x as usize, yy as usize, white);
            self.set_pixel(x1 as usize, yy as usize, white);
        }
    }

    fn draw_ascii(&mut self, text: &str, x: usize, y: usize, white: bool) {
        let mut cursor = x;
        for byte in text.bytes() {
            self.draw_glyph(byte, cursor, y, white);
            cursor += 8;
        }
    }

    fn draw_glyph(&mut self, byte: u8, x: usize, y: usize, white: bool) {
        let glyph = glyph_5x7(byte);
        for (col, bits) in glyph.iter().enumerate() {
            for row in 0..7 {
                if bits & (1 << row) != 0 {
                    self.set_pixel(x + col, y + row, white);
                }
            }
        }
    }

    fn set_pixel(&mut self, x: usize, y: usize, white: bool) {
        let Some((fx, fy)) = map_ui_pixel(self.orientation, x, y) else {
            return;
        };
        self.fb.set_pixel(fx, fy, white);
    }
}

fn map_ui_pixel(orientation: DisplayOrientation, x: usize, y: usize) -> Option<(usize, usize)> {
    match orientation {
        DisplayOrientation::LandscapeButtonsBottom => {
            if x < WIDTH && y < HEIGHT {
                Some((x, y))
            } else {
                None
            }
        }
        DisplayOrientation::LandscapeButtonsTop => {
            if x < WIDTH && y < HEIGHT {
                Some((WIDTH - 1 - x, HEIGHT - 1 - y))
            } else {
                None
            }
        }
        DisplayOrientation::PortraitButtonsRight => {
            if x < HEIGHT && y < WIDTH {
                Some((WIDTH - 1 - y, x))
            } else {
                None
            }
        }
        DisplayOrientation::PortraitButtonsLeft => {
            if x < HEIGHT && y < WIDTH {
                Some((y, HEIGHT - 1 - x))
            } else {
                None
            }
        }
    }
}

fn orientation_label(orientation: DisplayOrientation) -> &'static str {
    match orientation {
        DisplayOrientation::LandscapeButtonsBottom => "LANDSCAPE BOTTOM",
        DisplayOrientation::LandscapeButtonsTop => "LANDSCAPE TOP",
        DisplayOrientation::PortraitButtonsLeft => "PORTRAIT LEFT",
        DisplayOrientation::PortraitButtonsRight => "PORTRAIT RIGHT",
    }
}

fn refresh_policy_label(policy: RefreshPolicy) -> &'static str {
    match policy {
        RefreshPolicy::FastOnly => "FAST ONLY",
        RefreshPolicy::FullOnWake => "FULL ON WAKE",
        RefreshPolicy::FullEveryTen => "FULL EVERY 10",
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

async fn flush(
    epd: &mut Epd,
    fb: &Framebuffer,
    prev_fb: &Framebuffer,
    tx_band: &mut [u8; BAND_BYTES],
    screen_on: bool,
    mode: RefreshMode,
) -> Result<
    (),
    <SpiDmaBus<'static, SPI2, FullDuplexMode, Async> as embedded_hal_async::spi::ErrorType>::Error,
> {
    esp_println::println!("display: write BW RAM {:?}", mode);
    write_ram(epd, CMD_WRITE_RAM_BW, fb, tx_band).await?;
    esp_println::println!("display: write RED RAM");
    let red_source = if mode == RefreshMode::Fast {
        prev_fb
    } else {
        fb
    };
    write_ram(epd, CMD_WRITE_RAM_RED, red_source, tx_band).await?;

    esp_println::println!("display: refresh activate");
    epd.command(CMD_DISPLAY_UPDATE_CTRL1, &update_control_1(mode))
        .await?;
    epd.command(
        CMD_DISPLAY_UPDATE_CTRL2,
        &[update_control_2(mode, screen_on, false)],
    )
    .await?;
    epd.command(CMD_MASTER_ACTIVATION, &[]).await?;
    let start = Instant::now();
    epd.wait_ready().await;
    let elapsed = start.elapsed();
    esp_println::println!("display: refresh busy {} ms", elapsed.as_millis());
    Ok(())
}

async fn sleep_panel(
    epd: &mut Epd,
) -> Result<
    (),
    <SpiDmaBus<'static, SPI2, FullDuplexMode, Async> as embedded_hal_async::spi::ErrorType>::Error,
> {
    esp_println::println!("display: sleep start");
    epd.command(
        CMD_DISPLAY_UPDATE_CTRL2,
        &[update_control_2(RefreshMode::PowerDown, true, false)],
    )
    .await?;
    epd.command(CMD_MASTER_ACTIVATION, &[]).await?;
    epd.wait_ready().await;
    esp_println::println!("display: sleep deep");
    epd.command(CMD_DEEP_SLEEP, &[0x01]).await
}

async fn write_ram(
    epd: &mut Epd,
    ram_command: u8,
    fb: &Framebuffer,
    tx_band: &mut [u8; BAND_BYTES],
) -> Result<
    (),
    <SpiDmaBus<'static, SPI2, FullDuplexMode, Async> as embedded_hal_async::spi::ErrorType>::Error,
> {
    let rect = Rect::FULL;
    epd.command(CMD_SET_RAM_X_RANGE, &ram_x_range(rect)).await?;
    epd.command(CMD_SET_RAM_Y_RANGE, &ram_y_range(rect)).await?;
    epd.command(CMD_SET_RAM_X_COUNTER, &ram_x_counter(rect))
        .await?;
    epd.command(CMD_SET_RAM_Y_COUNTER, &ram_y_counter(rect))
        .await?;

    epd.begin_ram_write(ram_command).await?;
    let mut y = 0;
    let mut result = Ok(());
    while y < HEIGHT {
        let len = fill_transformed_band(fb, y, tx_band);
        if let Err(err) = epd.ram_chunk(&tx_band[..len]).await {
            result = Err(err);
            break;
        }
        y += BAND_ROWS;
    }
    epd.end_ram_write();
    result
}

fn fill_transformed_band(fb: &Framebuffer, band_y: usize, out: &mut [u8; BAND_BYTES]) -> usize {
    let rows = BAND_ROWS.min(HEIGHT - band_y);
    let len = rows * ROW_BYTES;

    if !MIRROR_X && !MIRROR_Y && !REVERSE_BITS {
        out[..len].copy_from_slice(fb.band(band_y, rows));
        return len;
    }

    for out_row in 0..rows {
        let panel_y = band_y + out_row;
        let src_y = if MIRROR_Y {
            HEIGHT - 1 - panel_y
        } else {
            panel_y
        };
        for out_byte in 0..ROW_BYTES {
            let src_byte = if MIRROR_X {
                ROW_BYTES - 1 - out_byte
            } else {
                out_byte
            };
            let mut value = fb.band(src_y, 1)[src_byte];
            if MIRROR_X || REVERSE_BITS {
                value = value.reverse_bits();
            }
            out[out_row * ROW_BYTES + out_byte] = value;
        }
    }

    len
}

fn fmt_u32(n: u32, buf: &mut [u8; 10]) -> &str {
    let mut i = buf.len();
    let mut v = n;
    if v == 0 {
        i -= 1;
        buf[i] = b'0';
    }
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    core::str::from_utf8(&buf[i..]).unwrap_or("?")
}

fn fmt_percent(n: u8, buf: &mut [u8; 10]) -> &str {
    let mut tmp = [0u8; 10];
    let number = fmt_u32(n as u32, &mut tmp).as_bytes();
    if number.len() + 1 > buf.len() {
        return "?";
    }
    buf[..number.len()].copy_from_slice(number);
    buf[number.len()] = b'%';
    core::str::from_utf8(&buf[..number.len() + 1]).unwrap_or("?")
}
