use crate::{
    catalog, Button, DisplayCommand, DisplayEvent, InputEvent, PowerEvent, ReaderSource,
    RenderKind, StorageCommand, DISPLAY_COMMANDS, DISPLAY_EVENTS, INPUT_EVENTS, LIBRARY_EVENTS,
    POWER_EVENTS, STORAGE_COMMANDS,
};
use app_core::{AppView, ReaderState, ReducerContext};
use display::Rect;
use embassy_futures::select::{select3, Either3};

#[embassy_executor::task]
pub async fn run() {
    esp_println::println!("app: started");
    let ctx = reducer_context();
    let mut state = ReaderState::boot();
    let mut rendering = true;
    let mut render_pending = false;
    let mut sleeping = false;
    let mut catalog_refresh_requested = false;
    let mut pending_storage: Option<StorageCommand> = None;
    let mut opening_book: Option<u32> = None;
    let mut suppress_input_until_open_settled = false;
    send_render(RenderKind::Boot, state).await;

    loop {
        match select3(
            INPUT_EVENTS.receive(),
            DISPLAY_EVENTS.receive(),
            LIBRARY_EVENTS.receive(),
        )
        .await
        {
            Either3::First(event) => {
                if matches!(
                    event,
                    InputEvent::Sample {
                        button: Some(Button::Power),
                        ..
                    }
                ) {
                    if sleeping {
                        esp_println::println!("app: wake");
                        sleeping = false;
                        state.view = AppView::Home;
                        state.dirty = Rect::FULL;
                        send_render(RenderKind::Page, state).await;
                        rendering = true;
                        render_pending = false;
                    } else {
                        esp_println::println!("app: sleep");
                        sleeping = true;
                        let _ = DISPLAY_COMMANDS.send(DisplayCommand::Sleep).await;
                    }
                    continue;
                }

                if sleeping {
                    continue;
                }

                if opening_book.is_some() || suppress_input_until_open_settled {
                    esp_println::println!("app: input ignored while book open pending");
                    drain_input_events();
                    continue;
                }

                let _ = POWER_EVENTS.try_send(PowerEvent::Activity);
                let previous = state;
                let previous_persisted = state.persisted();
                state = state.apply_input(ctx, event);
                let next_persisted = state.persisted();
                let storage_command = storage_command_for_transition(previous, state);
                if let Some(command) = storage_command {
                    if should_send_storage_immediately(command) {
                        log_storage_command("send", command);
                        if let Some(book_id) = open_book_id(command) {
                            opening_book = Some(book_id);
                            suppress_input_until_open_settled = true;
                        }
                        if STORAGE_COMMANDS.try_send(command).is_err() {
                            log_storage_command("queue", command);
                            pending_storage = Some(command);
                        }
                    } else {
                        log_storage_command("queue", command);
                        if let Some(book_id) = open_book_id(command) {
                            opening_book = Some(book_id);
                            suppress_input_until_open_settled = true;
                        }
                        pending_storage = Some(command);
                    }
                }
                if previous_persisted != next_persisted {
                    let command = StorageCommand::StoreProgress(next_persisted);
                    if STORAGE_COMMANDS.try_send(command).is_err() && pending_storage.is_none() {
                        pending_storage = Some(command);
                    }
                }
                if storage_command
                    .map(should_wait_for_loaded_before_render)
                    .unwrap_or(false)
                {
                    render_pending = false;
                    continue;
                }
                if rendering {
                    render_pending = true;
                } else {
                    send_render(RenderKind::Page, state).await;
                    rendering = true;
                    render_pending = false;
                }
            }
            Either3::Second(event) => match event {
                DisplayEvent::Settled => {
                    rendering = false;
                    if !catalog_refresh_requested {
                        catalog_refresh_requested = true;
                        if STORAGE_COMMANDS
                            .try_send(StorageCommand::LoadCatalogCache)
                            .is_err()
                        {
                            esp_println::println!("app: storage queue full for catalog cache");
                        }
                    }
                    if let Some(command) = pending_storage.take() {
                        log_storage_command("send", command);
                        if let Some(book_id) = open_book_id(command) {
                            opening_book = Some(book_id);
                            suppress_input_until_open_settled = true;
                        }
                        STORAGE_COMMANDS.send(command).await;
                    }
                    if render_pending {
                        send_render(RenderKind::Page, state).await;
                        rendering = true;
                        render_pending = false;
                    } else if suppress_input_until_open_settled && opening_book.is_none() {
                        drain_input_events();
                        suppress_input_until_open_settled = false;
                    }
                }
                DisplayEvent::Asleep => {
                    esp_println::println!("app: display asleep");
                    rendering = false;
                    render_pending = false;
                    opening_book = None;
                    suppress_input_until_open_settled = false;
                }
                DisplayEvent::Library(event) => {
                    if let Some(book_id) = loaded_book_id(event) {
                        if opening_book == Some(book_id) {
                            opening_book = None;
                            drain_input_events();
                        }
                    }
                    let should_render = library_event_affects_view(state, event);
                    state = state.apply_library_event(ctx, event);
                    if !should_render {
                        continue;
                    }
                    if rendering {
                        render_pending = true;
                    } else {
                        send_render(RenderKind::Page, state).await;
                        rendering = true;
                        render_pending = false;
                    }
                }
            },
            Either3::Third(event) => {
                if let Some(book_id) = loaded_book_id(event) {
                    if opening_book == Some(book_id) {
                        opening_book = None;
                        drain_input_events();
                    }
                }
                let should_render = library_event_affects_view(state, event);
                state = state.apply_library_event(ctx, event);
                if !should_render {
                    continue;
                }
                if rendering {
                    render_pending = true;
                } else {
                    send_render(RenderKind::Page, state).await;
                    rendering = true;
                    render_pending = false;
                }
            }
        }
    }
}

fn library_event_affects_view(state: ReaderState, event: crate::LibraryEvent) -> bool {
    match event {
        crate::LibraryEvent::Scanned { count } => {
            state.view == AppView::Library && state.library_count != count
        }
        crate::LibraryEvent::Loaded {
            book_id,
            pages: _,
            chapters: _,
        } => state.book_id == book_id,
        crate::LibraryEvent::ChapterPage {
            book_id,
            chapter,
            page,
        } => {
            state.book_id == book_id
                && state
                    .sd_chapter_pages
                    .get(chapter as usize)
                    .map(|stored| *stored != page.min(u16::MAX as u32) as u16)
                    .unwrap_or(false)
        }
        crate::LibraryEvent::Restored { .. } => true,
    }
}

fn should_send_storage_immediately(command: StorageCommand) -> bool {
    matches!(
        command,
        StorageCommand::OpenBook { .. } | StorageCommand::ExtendSection { .. }
    )
}

fn should_wait_for_loaded_before_render(command: StorageCommand) -> bool {
    matches!(
        command,
        StorageCommand::OpenBook { .. } | StorageCommand::ExtendSection { .. }
    )
}

fn open_book_id(command: StorageCommand) -> Option<u32> {
    match command {
        StorageCommand::OpenBook { book_id, .. }
        | StorageCommand::ExtendSection { book_id, .. } => Some(book_id),
        _ => None,
    }
}

fn loaded_book_id(event: crate::LibraryEvent) -> Option<u32> {
    match event {
        crate::LibraryEvent::Loaded { book_id, .. } => Some(book_id),
        _ => None,
    }
}

fn drain_input_events() {
    while INPUT_EVENTS.try_receive().is_ok() {}
}

async fn send_render(kind: RenderKind, state: ReaderState) {
    DISPLAY_COMMANDS
        .send(DisplayCommand::Render(state.render_request(kind)))
        .await;
}

fn log_storage_command(label: &str, command: StorageCommand) {
    match command {
        StorageCommand::OpenBook {
            book_id,
            index,
            chapter,
            target_pages,
        } => esp_println::println!(
            "app: storage {label} open book_id={book_id} index={index} chapter={chapter} target={target_pages}"
        ),
        StorageCommand::ExtendSection {
            book_id,
            index,
            chapter,
            target_pages,
        } => esp_println::println!(
            "app: storage {label} extend book_id={book_id} index={index} chapter={chapter} target={target_pages}"
        ),
        StorageCommand::StoreProgress(_) => {
            esp_println::println!("app: storage {label} progress")
        }
        StorageCommand::LoadCatalogCache => {
            esp_println::println!("app: storage {label} load catalog cache")
        }
        StorageCommand::RefreshCatalog => {
            esp_println::println!("app: storage {label} refresh catalog")
        }
    }
}

fn reducer_context() -> ReducerContext {
    ReducerContext::new(catalog::book_count(), catalog::chapter_count())
}

fn storage_command_for_transition(
    previous: ReaderState,
    next: ReaderState,
) -> Option<StorageCommand> {
    let Some(index) = ReaderSource::from_book_id(next.book_id).sd_index() else {
        return None;
    };
    if next.view != AppView::Reading {
        return None;
    }

    if previous.book_id != next.book_id {
        return Some(open_book_command(next, index));
    }

    if previous.view != AppView::Reading {
        if previous.view == AppView::Chapters {
            return if previous.page != next.page || previous.chapter != next.chapter {
                Some(extend_section_command(next, index))
            } else {
                None
            };
        }
        return Some(open_book_command(next, index));
    }

    if previous.page != next.page || previous.chapter != next.chapter {
        return Some(extend_section_command(next, index));
    }

    None
}

fn open_book_command(state: ReaderState, index: u8) -> StorageCommand {
    StorageCommand::OpenBook {
        book_id: state.book_id,
        index,
        chapter: state.chapter,
        target_pages: state.page.min(u16::MAX as u32) as u16,
    }
}

fn extend_section_command(state: ReaderState, index: u8) -> StorageCommand {
    StorageCommand::ExtendSection {
        book_id: state.book_id,
        index,
        chapter: state.chapter,
        target_pages: state.page.min(u16::MAX as u32) as u16,
    }
}
