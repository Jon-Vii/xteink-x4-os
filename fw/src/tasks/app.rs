use crate::{
    catalog, AppView, Button, DisplayCommand, DisplayEvent, DisplayOrientation, InputEvent,
    LibraryEvent, PowerEvent, RefreshPolicy, RenderKind, RenderRequest, DISPLAY_COMMANDS,
    DISPLAY_EVENTS, INPUT_EVENTS, LIBRARY_EVENTS, POWER_EVENTS,
};
use display::Rect;
use embassy_futures::select::{select3, Either3};
use hal_ext::nvm::AppStateRecord;

const SETTINGS_ITEMS: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReaderState {
    view: AppView,
    page: u32,
    selection: u8,
    chapter: u8,
    book_id: u32,
    orientation: DisplayOrientation,
    refresh_policy: RefreshPolicy,
    last_button: Option<Button>,
    aux_raw: u16,
    nav_raw: u16,
    page_raw: u16,
    battery_mv: u16,
    battery_percent: u8,
    library_count: u8,
    dirty: Rect,
}

impl ReaderState {
    const fn boot() -> Self {
        Self {
            view: AppView::Home,
            page: 0,
            selection: 0,
            chapter: 0,
            book_id: 1,
            orientation: DisplayOrientation::LandscapeButtonsTop,
            refresh_policy: RefreshPolicy::FullOnWake,
            last_button: None,
            aux_raw: 0,
            nav_raw: 0,
            page_raw: 0,
            battery_mv: 0,
            battery_percent: 100,
            library_count: 0,
            dirty: Rect::FULL,
        }
    }

    fn apply(self, event: InputEvent) -> Self {
        let InputEvent::Sample {
            button,
            aux_raw,
            nav_raw,
            page_raw,
            battery_mv,
            battery_percent,
        } = event;
        let mut next = self;
        next.last_button = button;
        next.aux_raw = aux_raw;
        next.nav_raw = nav_raw;
        next.page_raw = page_raw;
        next.battery_mv = battery_mv;
        next.battery_percent = battery_percent;
        next.dirty = Rect::FULL;

        match (self.view, button) {
            (_, None) => {}
            (_, Some(Button::Power)) => {}
            (AppView::Home, Some(Button::Back)) => {
                next.view = AppView::Reading;
                next.selection = self.chapter;
            }
            (AppView::Home, Some(Button::Confirm)) => {
                next.view = AppView::Library;
                next.selection = 0;
            }
            (AppView::Home, Some(Button::Previous)) => {
                next.view = AppView::Sync;
                next.selection = 0;
            }
            (AppView::Home, Some(Button::Next)) => {
                next.view = AppView::Settings;
                next.selection = 0;
            }

            (AppView::Library, Some(Button::Next)) => {
                next.selection = wrap_next(self.selection, self.library_item_count());
            }
            (AppView::Library, Some(Button::Previous)) => {
                next.selection = wrap_prev(self.selection, self.library_item_count());
            }
            (AppView::Library, Some(Button::Confirm)) => {
                if self.selection < self.library_count {
                    next.book_id = self.selection as u32 + 2;
                    next.view = AppView::Reading;
                    next.chapter = 0;
                    next.selection = 0;
                    next.page = 0;
                } else if let Some(book) = catalog::book_at(self.selection as usize) {
                    next.book_id = book.id.0;
                    next.view = AppView::Reading;
                    next.selection = 0;
                }
            }
            (AppView::Library, Some(Button::Back)) => {
                next.view = AppView::Home;
                next.selection = 1;
            }

            (AppView::Reading, Some(Button::Next)) => {
                next.chapter = wrap_next(self.chapter, catalog::chapter_count());
                next.selection = next.chapter;
                next.page = 0;
            }
            (AppView::Reading, Some(Button::Previous)) => {
                next.chapter = wrap_prev(self.chapter, catalog::chapter_count());
                next.selection = next.chapter;
                next.page = 0;
            }
            (AppView::Reading, Some(Button::Confirm)) => {
                next.view = AppView::Chapters;
                next.selection = self.chapter;
            }
            (AppView::Reading, Some(Button::Back)) => {
                next.view = AppView::Home;
                next.selection = 0;
            }

            (AppView::Chapters, Some(Button::Next)) => {
                next.selection = wrap_next(self.selection, catalog::chapter_count());
            }
            (AppView::Chapters, Some(Button::Previous)) => {
                next.selection = wrap_prev(self.selection, catalog::chapter_count());
            }
            (AppView::Chapters, Some(Button::Confirm)) => {
                next.chapter = self.selection;
                next.page = 0;
                next.view = AppView::Reading;
            }
            (AppView::Chapters, Some(Button::Back)) => {
                next.view = AppView::Reading;
            }

            (AppView::Sync, Some(Button::Back | Button::Confirm)) => {
                next.view = AppView::Home;
                next.selection = 0;
            }
            (AppView::Sync, Some(Button::Previous | Button::Next)) => {}

            (AppView::Settings, Some(Button::Next)) => {
                next.selection = wrap_next(self.selection, SETTINGS_ITEMS);
            }
            (AppView::Settings, Some(Button::Previous)) => {
                next.selection = wrap_prev(self.selection, SETTINGS_ITEMS);
            }
            (AppView::Settings, Some(Button::Confirm)) => {
                next = apply_setting(next);
            }
            (AppView::Settings, Some(Button::Back)) => {
                next.view = AppView::Home;
                next.selection = 2;
            }
        }

        next
    }

    fn apply_library_event(mut self, event: LibraryEvent) -> Self {
        let LibraryEvent::Scanned { count } = event;
        self.library_count = count;
        if self.view == AppView::Library {
            if count == 0 {
                self.selection = 0;
            } else if self.selection >= count {
                self.selection = count - 1;
            }
            self.dirty = Rect::FULL;
        }
        self
    }

    fn library_item_count(self) -> u8 {
        self.library_count.max(catalog::book_count()).max(1)
    }

    fn persisted(self) -> AppStateRecord {
        AppStateRecord {
            book_id: self.book_id,
            chapter: self.chapter as u16,
            screen: self.page,
            shell_orientation: DisplayOrientation::PortraitButtonsLeft as u8,
            reading_orientation: self.orientation as u8,
            refresh_policy: self.refresh_policy as u8,
        }
    }
}

fn wrap_next(value: u8, len: u8) -> u8 {
    if value + 1 >= len {
        0
    } else {
        value + 1
    }
}

fn wrap_prev(value: u8, len: u8) -> u8 {
    if value == 0 {
        len - 1
    } else {
        value - 1
    }
}

fn apply_setting(mut state: ReaderState) -> ReaderState {
    match state.selection {
        0 => {
            state.orientation = match state.orientation {
                DisplayOrientation::LandscapeButtonsBottom => {
                    DisplayOrientation::LandscapeButtonsTop
                }
                DisplayOrientation::LandscapeButtonsTop => DisplayOrientation::PortraitButtonsLeft,
                DisplayOrientation::PortraitButtonsLeft => DisplayOrientation::PortraitButtonsRight,
                DisplayOrientation::PortraitButtonsRight => {
                    DisplayOrientation::LandscapeButtonsBottom
                }
            };
        }
        1 => {
            state.refresh_policy = match state.refresh_policy {
                RefreshPolicy::FastOnly => RefreshPolicy::FullOnWake,
                RefreshPolicy::FullOnWake => RefreshPolicy::FullEveryTen,
                RefreshPolicy::FullEveryTen => RefreshPolicy::FastOnly,
            };
        }
        _ => {
            state.view = AppView::Home;
            state.selection = 2;
        }
    }
    state
}

#[embassy_executor::task]
pub async fn run() {
    esp_println::println!("app: started");
    let mut state = ReaderState::boot();
    let mut rendering = true;
    let mut render_pending = false;
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
                    let _ = POWER_EVENTS.try_send(PowerEvent::Activity);
                    continue;
                }

                let _ = POWER_EVENTS.try_send(PowerEvent::Activity);
                state = state.apply(event);
                let _pending_persist = state.persisted();
                if rendering {
                    render_pending = true;
                } else {
                    send_render(RenderKind::Page, state).await;
                    rendering = true;
                    render_pending = false;
                }
            }
            Either3::Second(DisplayEvent::Settled) => {
                rendering = false;
                if render_pending {
                    send_render(RenderKind::Page, state).await;
                    rendering = true;
                    render_pending = false;
                }
            }
            Either3::Third(event) => {
                state = state.apply_library_event(event);
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

async fn send_render(kind: RenderKind, state: ReaderState) {
    DISPLAY_COMMANDS
        .send(DisplayCommand::Render(RenderRequest {
            kind,
            view: state.view,
            page: state.page,
            chapter: state.chapter,
            selection: state.selection,
            book_id: state.book_id,
            orientation: state.orientation,
            refresh_policy: state.refresh_policy,
            last_button: state.last_button,
            aux_raw: state.aux_raw,
            nav_raw: state.nav_raw,
            page_raw: state.page_raw,
            battery_mv: state.battery_mv,
            battery_percent: state.battery_percent,
            dirty: state.dirty,
        }))
        .await;
}
