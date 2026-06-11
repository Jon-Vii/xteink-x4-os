//! Browser-to-shelf book upload plumbing.
//!
//! The wifi task receives raw EPUB bytes over HTTP and streams them to
//! the display task (the single SD owner) through a two-buffer
//! ping-pong: chunks carry loaned 4 KB buffers one way, the buffers
//! come back on the return channel once written. The display task holds
//! one SD session for the whole upload phase and writes /BOOKS/<8.3>.

use heapless::String;

/// 8.3 names cap at twelve characters.
pub type UploadName = String<12>;

pub struct UploadBegin {
    pub name: UploadName,
}

pub struct UploadChunk {
    /// `None` only on aborts that have no buffer left to hand over.
    pub buffer: Option<&'static mut [u8]>,
    pub len: usize,
    pub last: bool,
    pub abort: bool,
}

/// Derives an 8.3 upload name from a browser filename: keep the first
/// eight ASCII alphanumerics uppercased, default to BOOK, extension
/// `.EPU` (which the catalog scan accepts alongside `.epub`).
pub fn sanitized_name(client_name: &str) -> UploadName {
    let stem_source = client_name
        .rsplit_once('.')
        .map(|(stem, _ext)| stem)
        .unwrap_or(client_name);
    let mut name = UploadName::new();
    for byte in stem_source.bytes() {
        if name.len() == 8 {
            break;
        }
        if byte.is_ascii_alphanumeric() {
            let _ = name.push(byte.to_ascii_uppercase() as char);
        }
    }
    if name.is_empty() {
        let _ = name.push_str("BOOK");
    }
    let _ = name.push_str(".EPU");
    name
}
