use crate::reader_store::ReaderStore;
use display::font::FontStyle;
use embedded_sdmmc::{Directory, File, Mode, TimeSource};
use heapless::String;
use proto::cache::{
    decode_block, decode_cover_header, decode_page, decode_section_header, encode_block,
    encode_book_header, encode_page, encode_section_header, encode_spine, encode_toc,
    section_file_name, BookCacheHeader, SectionHeader, SpineRecord, TocRecord as CacheTocRecord,
    BLOCK_RECORD_BYTES, BOOK_HEADER_BYTES, CACHE_BOOK_FILE, CACHE_COVER_FILE, CACHE_DIR,
    CACHE_ROOT_DIR, CACHE_SECTIONS_DIR, CACHE_SECTION_FILE_BYTES, CACHE_STATE_FILE, COVER_BYTES,
    PAGE_RECORD_BYTES, SECTION_HEADER_BYTES, SPINE_RECORD_BYTES, TOC_RECORD_BYTES,
};

pub(crate) fn ensure_cache_dirs<
    D,
    T,
    const MAX_DIRS: usize,
    const MAX_FILES: usize,
    const MAX_VOLUMES: usize,
>(
    root: &Directory<'_, D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
    key: &str,
) -> Result<(), ()>
where
    D: embedded_sdmmc::BlockDevice,
    T: TimeSource,
{
    let xteink = open_or_make_dir(root, CACHE_ROOT_DIR)?;
    let cache = open_or_make_dir(&xteink, CACHE_DIR)?;
    let book = open_or_make_dir(&cache, key)?;
    let _ = open_or_make_dir(&book, CACHE_SECTIONS_DIR)?;
    Ok(())
}

pub(crate) fn write_state_file<
    D,
    T,
    const MAX_DIRS: usize,
    const MAX_FILES: usize,
    const MAX_VOLUMES: usize,
>(
    root: &Directory<'_, D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
    record: hal_ext::nvm::AppStateRecord,
) -> Result<(), ()>
where
    D: embedded_sdmmc::BlockDevice,
    T: TimeSource,
{
    let xteink = open_or_make_dir(root, CACHE_ROOT_DIR)?;
    let file = xteink
        .open_file_in_dir(CACHE_STATE_FILE, Mode::ReadWriteCreateOrTruncate)
        .map_err(|_| ())?;
    file.write(&record.encode()).map_err(|_| ())
}

fn open_or_make_dir<
    'a,
    D,
    T,
    const MAX_DIRS: usize,
    const MAX_FILES: usize,
    const MAX_VOLUMES: usize,
>(
    parent: &'a Directory<'_, D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
    name: &str,
) -> Result<Directory<'a, D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>, ()>
where
    D: embedded_sdmmc::BlockDevice,
    T: TimeSource,
{
    match parent.open_dir(name) {
        Ok(dir) => Ok(dir),
        Err(_) => {
            let _ = parent.make_dir_in_dir(name);
            parent.open_dir(name).map_err(|_| ())
        }
    }
}

pub(crate) fn write_book_cache<
    D,
    T,
    const MAX_DIRS: usize,
    const MAX_FILES: usize,
    const MAX_VOLUMES: usize,
>(
    root: &Directory<'_, D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
    key: &str,
    package: &proto::epub::EpubPackage<'_>,
    library: &ReaderStore,
) where
    D: embedded_sdmmc::BlockDevice,
    T: TimeSource,
{
    let Ok(xteink) = root.open_dir(CACHE_ROOT_DIR) else {
        return;
    };
    let Ok(cache) = xteink.open_dir(CACHE_DIR) else {
        return;
    };
    let Ok(book_dir) = cache.open_dir(key) else {
        return;
    };
    let Ok(file) = book_dir.open_file_in_dir(CACHE_BOOK_FILE, Mode::ReadWriteCreateOrTruncate)
    else {
        return;
    };
    let string_bytes = book_string_bytes(package, library);
    let header = BookCacheHeader {
        spine_count: package.spine.len().min(u16::MAX as usize) as u16,
        toc_count: library.toc_count.min(u16::MAX as usize) as u16,
        string_bytes,
    };
    let mut record = [0u8; TOC_RECORD_BYTES];
    if encode_book_header(header, &mut record[..BOOK_HEADER_BYTES]).is_err()
        || file.write(&record[..BOOK_HEADER_BYTES]).is_err()
    {
        return;
    }

    let mut offset = book_meta_string_bytes(package);
    for (spine_index, spine) in package.spine.iter().enumerate() {
        let href_len = spine.href.len().min(u16::MAX as usize) as u16;
        let toc_index = library
            .toc
            .iter()
            .take(library.toc_count)
            .position(|toc| toc.spine_index == spine_index as i16)
            .map(|index| index as i16)
            .unwrap_or(-1);
        let spine_record = SpineRecord {
            href_offset: offset,
            href_len,
            toc_index,
            byte_size: 0,
        };
        if encode_spine(spine_record, &mut record[..SPINE_RECORD_BYTES]).is_err()
            || file.write(&record[..SPINE_RECORD_BYTES]).is_err()
        {
            return;
        }
        offset = offset.saturating_add(href_len as u32);
    }

    for toc in library.toc.iter().take(library.toc_count).copied() {
        let title_offset = offset;
        offset = offset.saturating_add(toc.title_len as u32);
        let href_offset = offset;
        offset = offset.saturating_add(toc.href_len as u32);
        let cache_toc = CacheTocRecord {
            title_offset,
            title_len: toc.title_len,
            href_offset,
            href_len: toc.href_len,
            anchor_offset: 0,
            anchor_len: 0,
            level: toc.level,
            spine_index: toc.spine_index,
        };
        if encode_toc(cache_toc, &mut record[..TOC_RECORD_BYTES]).is_err()
            || file.write(&record[..TOC_RECORD_BYTES]).is_err()
        {
            return;
        }
    }

    let _ = file.write(package.meta.title.as_bytes());
    let _ = file.write(&[0]);
    let _ = file.write(package.meta.author.as_bytes());
    let _ = file.write(&[0]);
    let _ = file.write(package.meta.source_path.as_bytes());
    let _ = file.write(&[0]);
    for spine in package.spine.iter() {
        let _ = file.write(spine.href.as_bytes());
    }
    for index in 0..library.toc_count {
        let _ = file.write(library.toc_title(index).as_bytes());
        let href = library.toc_href(index);
        let _ = file.write(href.as_bytes());
    }
}

pub(crate) fn load_section_cache<
    D,
    T,
    const MAX_DIRS: usize,
    const MAX_FILES: usize,
    const MAX_VOLUMES: usize,
>(
    root: &Directory<'_, D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
    key: &str,
    spine: u16,
    library: &mut ReaderStore,
) -> Option<usize>
where
    D: embedded_sdmmc::BlockDevice,
    T: TimeSource,
{
    let xteink = root.open_dir(CACHE_ROOT_DIR).ok()?;
    let cache = xteink.open_dir(CACHE_DIR).ok()?;
    let book_dir = cache.open_dir(key).ok()?;
    let sections = book_dir.open_dir(CACHE_SECTIONS_DIR).ok()?;
    let mut name = String::<CACHE_SECTION_FILE_BYTES>::new();
    section_file_name(spine, &mut name);
    let file = sections
        .open_file_in_dir(name.as_str(), Mode::ReadOnly)
        .ok()?;
    let mut header_bytes = [0u8; SECTION_HEADER_BYTES];
    read_exact_file(&file, &mut header_bytes).ok()?;
    let header = decode_section_header(&header_bytes).ok()?;
    let page_count = header.page_count as usize;
    let block_count = header.block_count as usize;
    let text_bytes = header.text_bytes as usize;
    if page_count > library.pages.len()
        || block_count > library.blocks.len()
        || text_bytes > library.text.len()
    {
        return None;
    }

    let mut record_bytes = [0u8; 16];
    for index in 0..page_count {
        read_exact_file(&file, &mut record_bytes[..PAGE_RECORD_BYTES]).ok()?;
        library.pages[index] = decode_page(&record_bytes[..PAGE_RECORD_BYTES]).ok()?;
        library.page_spine[index] = spine;
    }
    for index in 0..block_count {
        read_exact_file(&file, &mut record_bytes[..BLOCK_RECORD_BYTES]).ok()?;
        let block = decode_block(&record_bytes[..BLOCK_RECORD_BYTES]).ok()?;
        library.blocks[index] = block;
        library.block_styles[index] = display_style_for_proto_style(block.style);
        library.block_spine[index] = spine;
    }
    for index in 0..block_count {
        let mut flag = [0u8; 1];
        read_exact_file(&file, &mut flag).ok()?;
        library.block_paragraph_end[index] = flag[0] != 0;
    }
    read_exact_file(&file, &mut library.text[..text_bytes]).ok()?;
    library.page_count = page_count;
    library.block_count = block_count;
    library.text_len = text_bytes;
    library.cached_spine = spine;
    library.section_partial = header.partial;
    Some(page_count)
}

pub(crate) fn load_cover_cache<
    D,
    T,
    const MAX_DIRS: usize,
    const MAX_FILES: usize,
    const MAX_VOLUMES: usize,
>(
    root: &Directory<'_, D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
    key: &str,
    library: &mut ReaderStore,
) where
    D: embedded_sdmmc::BlockDevice,
    T: TimeSource,
{
    library.clear_cover();
    let Some((width, height, bits)) = read_cover_cache(root, key, &mut library.cover_bits) else {
        return;
    };
    library.cover_width = width;
    library.cover_height = height;
    library.cover_ready = bits == COVER_BYTES;
}

fn read_cover_cache<D, T, const MAX_DIRS: usize, const MAX_FILES: usize, const MAX_VOLUMES: usize>(
    root: &Directory<'_, D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
    key: &str,
    out: &mut [u8; COVER_BYTES],
) -> Option<(u16, u16, usize)>
where
    D: embedded_sdmmc::BlockDevice,
    T: TimeSource,
{
    let xteink = root.open_dir(CACHE_ROOT_DIR).ok()?;
    let cache = xteink.open_dir(CACHE_DIR).ok()?;
    let book_dir = cache.open_dir(key).ok()?;
    let file = book_dir
        .open_file_in_dir(CACHE_COVER_FILE, Mode::ReadOnly)
        .ok()?;
    let mut header = [0u8; proto::cache::COVER_HEADER_BYTES];
    read_exact_file(&file, &mut header).ok()?;
    let header = decode_cover_header(&header).ok()?;
    read_exact_file(&file, out).ok()?;
    Some((header.width, header.height, COVER_BYTES))
}

fn book_string_bytes(package: &proto::epub::EpubPackage<'_>, library: &ReaderStore) -> u32 {
    let mut total = book_meta_string_bytes(package);
    for spine in package.spine.iter() {
        total = total.saturating_add(spine.href.len().min(u16::MAX as usize) as u32);
    }
    for index in 0..library.toc_count {
        total = total.saturating_add(library.toc_title(index).len().min(u16::MAX as usize) as u32);
        total = total.saturating_add(library.toc_href(index).len().min(u16::MAX as usize) as u32);
    }
    total
}

fn book_meta_string_bytes(package: &proto::epub::EpubPackage<'_>) -> u32 {
    package.meta.title.len().saturating_add(1) as u32
        + package.meta.author.len().saturating_add(1) as u32
        + package.meta.source_path.len().saturating_add(1) as u32
}

pub(crate) fn write_section_cache<
    D,
    T,
    const MAX_DIRS: usize,
    const MAX_FILES: usize,
    const MAX_VOLUMES: usize,
>(
    root: &Directory<'_, D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
    key: &str,
    spine: u16,
    library: &ReaderStore,
) where
    D: embedded_sdmmc::BlockDevice,
    T: TimeSource,
{
    let Ok(xteink) = root.open_dir(CACHE_ROOT_DIR) else {
        return;
    };
    let Ok(cache) = xteink.open_dir(CACHE_DIR) else {
        return;
    };
    let Ok(book_dir) = cache.open_dir(key) else {
        return;
    };
    let Ok(sections) = book_dir.open_dir(CACHE_SECTIONS_DIR) else {
        return;
    };
    let mut name = String::<CACHE_SECTION_FILE_BYTES>::new();
    section_file_name(spine, &mut name);
    let Ok(file) = sections.open_file_in_dir(name.as_str(), Mode::ReadWriteCreateOrTruncate) else {
        return;
    };
    let header = SectionHeader {
        page_count: library.page_count.min(u16::MAX as usize) as u16,
        block_count: library.block_count.min(u16::MAX as usize) as u16,
        line_count: 0,
        word_count: 0,
        text_bytes: library.text_len.min(u32::MAX as usize) as u32,
        viewport_width: 800,
        viewport_height: 480,
        font_config: 1,
        bytes_consumed: 0,
        total_bytes: 0,
        partial: library.section_partial,
    };
    let mut bytes = [0u8; SECTION_HEADER_BYTES];
    if encode_section_header(header, &mut bytes).is_err() || file.write(&bytes).is_err() {
        return;
    }
    let mut record = [0u8; 16];
    for page in library.pages.iter().take(library.page_count) {
        if encode_page(*page, &mut record[..PAGE_RECORD_BYTES]).is_err()
            || file.write(&record[..PAGE_RECORD_BYTES]).is_err()
        {
            return;
        }
    }
    for block in library.blocks.iter().take(library.block_count) {
        if encode_block(*block, &mut record[..BLOCK_RECORD_BYTES]).is_err()
            || file.write(&record[..BLOCK_RECORD_BYTES]).is_err()
        {
            return;
        }
    }
    for flag in library
        .block_paragraph_end
        .iter()
        .take(library.block_count)
        .copied()
    {
        if file.write(&[flag as u8]).is_err() {
            return;
        }
    }
    let _ = file.write(&library.text[..library.text_len]);
}

fn read_exact_file<D, T, const MAX_DIRS: usize, const MAX_FILES: usize, const MAX_VOLUMES: usize>(
    file: &File<'_, D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
    mut out: &mut [u8],
) -> Result<(), ()>
where
    D: embedded_sdmmc::BlockDevice,
    T: TimeSource,
{
    while !out.is_empty() {
        let read = file.read(out).map_err(|_| ())?;
        if read == 0 {
            return Err(());
        }
        let tmp = out;
        out = &mut tmp[read..];
    }
    Ok(())
}

fn display_style_for_proto_style(style: proto::text::FontStyle) -> FontStyle {
    match style {
        proto::text::FontStyle::BoldItalic => FontStyle::BoldItalic,
        proto::text::FontStyle::Bold => FontStyle::Bold,
        proto::text::FontStyle::Italic => FontStyle::Italic,
        proto::text::FontStyle::Regular => FontStyle::Regular,
    }
}
