use proto::book::{BookId, BookMeta, BookSource, ChapterMeta, CoverStatus};

pub const ACTIVE_BOOK_ID: BookId = BookId(1);

pub const BOOKS: [BookMeta<'static>; 1] = [BookMeta {
    id: ACTIVE_BOOK_ID,
    title: "About This Reader",
    author: "",
    source_path: "/built-in/guide",
    byte_size: 0,
    source: BookSource::BuiltIn,
    cover_status: CoverStatus::Missing,
}];

pub const CHAPTERS: [ChapterMeta<'static>; 4] = [
    ChapterMeta {
        title: "Reading",
        spine_index: 0,
        source_href: "guide/reading.xhtml",
    },
    ChapterMeta {
        title: "Adding Books",
        spine_index: 1,
        source_href: "guide/adding-books.xhtml",
    },
    ChapterMeta {
        title: "Wi-Fi Setup",
        spine_index: 2,
        source_href: "guide/wifi-setup.xhtml",
    },
    ChapterMeta {
        title: "Progress Sync",
        spine_index: 3,
        source_href: "guide/progress-sync.xhtml",
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderLineStyle {
    Heading,
    Body,
    Italic,
    Bold,
    Quote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReaderLine {
    pub text: &'static str,
    pub style: ReaderLineStyle,
    pub gap_after: u8,
}

impl ReaderLine {
    pub const fn new(text: &'static str, style: ReaderLineStyle, gap_after: u8) -> Self {
        Self {
            text,
            style,
            gap_after,
        }
    }
}

pub const READER_PAGES: [&[ReaderLine]; 4] = [
    &[
        ReaderLine::new("Reading", ReaderLineStyle::Heading, 18),
        ReaderLine::new(
            "The two keys under your grip turn pages. On the front column: the top key backs out of any screen, the second confirms, and the lower pair browses.",
            ReaderLineStyle::Body,
            14,
        ),
        ReaderLine::new(
            "While reading, confirm opens the chapter list and back returns home.",
            ReaderLineStyle::Body,
            14,
        ),
        ReaderLine::new(
            "Your place is saved as you read and restored when the reader wakes.",
            ReaderLineStyle::Italic,
            0,
        ),
    ],
    &[
        ReaderLine::new("Adding Books", ReaderLineStyle::Heading, 18),
        ReaderLine::new(
            "Copy EPUB files into the books folder on the card, or send them from a browser: start sync from home, and once connected the screen shows an address.",
            ReaderLineStyle::Body,
            14,
        ),
        ReaderLine::new(
            "Open that address on any computer or phone on the same network to see what is on the card, add books, or remove them.",
            ReaderLineStyle::Body,
            14,
        ),
        ReaderLine::new(
            "New books appear after the reader restarts.",
            ReaderLineStyle::Italic,
            0,
        ),
    ],
    &[
        ReaderLine::new("Wi-Fi Setup", ReaderLineStyle::Heading, 18),
        ReaderLine::new(
            "The first time you start sync, the reader opens its own hotspot instead of joining a network.",
            ReaderLineStyle::Body,
            14,
        ),
        ReaderLine::new(
            "Scan the code on its screen with your phone, and enter your home network's name and password in the page that appears.",
            ReaderLineStyle::Body,
            14,
        ),
        ReaderLine::new(
            "The reader remembers the network on its card; run sync again to connect.",
            ReaderLineStyle::Body,
            0,
        ),
    ],
    &[
        ReaderLine::new("Progress Sync", ReaderLineStyle::Heading, 18),
        ReaderLine::new(
            "Sync exchanges your reading position with a KOReader-compatible server: whichever device read furthest wins.",
            ReaderLineStyle::Body,
            14,
        ),
        ReaderLine::new(
            "Read a few pages here, sync, and pick up the same book on your phone or tablet exactly where you stopped.",
            ReaderLineStyle::Body,
            14,
        ),
        ReaderLine::new(
            "Sync pauses reading and ends with a quick restart.",
            ReaderLineStyle::Italic,
            0,
        ),
    ],
];

pub fn active_book(book_id: u32) -> BookMeta<'static> {
    BOOKS
        .iter()
        .copied()
        .find(|book| book.id.0 == book_id)
        .unwrap_or(BOOKS[0])
}

pub fn book_at(index: usize) -> Option<BookMeta<'static>> {
    BOOKS.get(index).copied()
}

pub const fn book_count() -> u8 {
    BOOKS.len() as u8
}

pub fn chapter_at(index: usize) -> Option<ChapterMeta<'static>> {
    CHAPTERS.get(index).copied()
}

pub const fn chapter_count() -> u8 {
    CHAPTERS.len() as u8
}
