//! Browser-to-shelf book upload plumbing.
//!
//! The wifi task receives raw EPUB bytes over HTTP and streams them to
//! the display task (the single SD owner) through a two-buffer
//! ping-pong: chunks carry loaned 4 KB buffers one way, the buffers
//! come back on the return channel once written. The display task holds
//! one SD session for the whole upload phase and writes /BOOKS/<8.3>.

use heapless::String;
// riscv32imc has no CAS; portable-atomic provides it on single-core.
use portable_atomic::AtomicBool;

/// True while a book body is streaming; the session-ending reset waits
/// for it so a done press cannot truncate a file mid-write.
pub static UPLOAD_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

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

/// Derives an 8.3 upload name from raw (still percent-encoded) filename
/// bytes: keep the first eight ASCII alphanumerics uppercased, default
/// to BOOK, extension `.EPU` (which the catalog scan accepts alongside
/// `.epub`). Working on raw bytes sidesteps any decode-buffer limit;
/// percent escapes simply contribute their hex letters.
pub fn sanitized_name(client_name: &[u8]) -> UploadName {
    let stem_end = client_name
        .iter()
        .rposition(|byte| *byte == b'.')
        .unwrap_or(client_name.len());
    let stem_source = client_name[..stem_end].iter().copied();
    let mut name = UploadName::new();
    for byte in stem_source {
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
