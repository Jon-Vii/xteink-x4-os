use crate::book::{BookId, BookMeta, BookSource, CoverStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanRoot {
    BooksDir,
    CardRoot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileCandidate<'a> {
    pub root: ScanRoot,
    pub path: &'a str,
    pub byte_size: u32,
}

impl<'a> FileCandidate<'a> {
    pub fn as_book(self, id: BookId) -> Option<BookMeta<'a>> {
        if !is_epub_path(self.path) {
            return None;
        }
        let file_name = self.path.rsplit('/').next().unwrap_or(self.path);
        let title = file_name.strip_suffix(".epub").unwrap_or(file_name);
        Some(BookMeta {
            id,
            title,
            author: "Unknown Author",
            source_path: self.path,
            byte_size: self.byte_size,
            source: BookSource::MicroSd,
            cover_status: CoverStatus::Unknown,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageError {
    NoCard,
    UnsupportedFilesystem,
    Io,
    TooManyBooks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReaderProgressRecord<'a> {
    pub book_path: &'a str,
    pub book_id: BookId,
    pub spine_index: u16,
    pub screen_index: u32,
    pub text_run_index: u16,
    pub text_byte_offset: u16,
    pub reading_orientation: u8,
    pub refresh_policy: u8,
}

pub trait BookStorage {
    fn scan_epubs(
        &mut self,
        on_candidate: impl FnMut(FileCandidate<'_>) -> Result<(), StorageError>,
    ) -> Result<(), StorageError>;

    fn read_at(&mut self, path: &str, offset: u32, out: &mut [u8]) -> Result<usize, StorageError>;
}

pub trait ProgressStorage {
    fn load_progress<'a>(
        &mut self,
        scratch: &'a mut [u8],
    ) -> Result<Option<ReaderProgressRecord<'a>>, StorageError>;

    fn store_progress(&mut self, record: ReaderProgressRecord<'_>) -> Result<(), StorageError>;
}

pub fn is_epub_path(path: &str) -> bool {
    // Uploads are written with 8.3 names, where the extension truncates
    // to ".epu"; accept both spellings everywhere EPUBs are discovered.
    if path.len() >= 4 {
        let tail = &path.as_bytes()[path.len() - 4..];
        if tail[0] == b'.'
            && tail[1].eq_ignore_ascii_case(&b'e')
            && tail[2].eq_ignore_ascii_case(&b'p')
            && tail[3].eq_ignore_ascii_case(&b'u')
        {
            return true;
        }
    }
    path.as_bytes()
        .windows(5)
        .last()
        .map(|suffix| suffix.eq_ignore_ascii_case(b".epub"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_epub_suffix_case_insensitively() {
        assert!(is_epub_path("/books/Alice.EPUB"));
        assert!(is_epub_path("book.epub"));
        assert!(!is_epub_path("book.epub.tmp"));
    }

    #[test]
    fn file_candidate_becomes_minimal_book_meta() {
        let candidate = FileCandidate {
            root: ScanRoot::BooksDir,
            path: "/books/algernon.epub",
            byte_size: 42,
        };

        let book = candidate.as_book(BookId(3)).expect("epub candidate");

        assert_eq!(book.title, "algernon");
        assert_eq!(book.source, BookSource::MicroSd);
        assert_eq!(book.byte_size, 42);
    }
}
