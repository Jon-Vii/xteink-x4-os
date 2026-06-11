#![no_std]
#![forbid(unsafe_code)]

use display::font::{FontSize, LineSpacing, TypeSettings};
use display::{epd::RefreshMode, Rect};

pub const SETTINGS_ITEMS: u8 = 4;
pub const MAX_SD_CHAPTERS: usize = 128;
pub const FIRST_SD_BOOK_ID: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderSource {
    BuiltIn { book_id: u32 },
    Sd { index: u8 },
}

impl ReaderSource {
    pub fn from_book_id(book_id: u32) -> Self {
        if book_id >= FIRST_SD_BOOK_ID {
            Self::Sd {
                index: book_id.saturating_sub(FIRST_SD_BOOK_ID).min(u8::MAX as u32) as u8,
            }
        } else {
            Self::BuiltIn { book_id }
        }
    }

    pub const fn sd(index: u8) -> Self {
        Self::Sd { index }
    }

    pub const fn book_id(self) -> u32 {
        match self {
            Self::BuiltIn { book_id } => book_id,
            Self::Sd { index } => FIRST_SD_BOOK_ID + index as u32,
        }
    }

    pub const fn sd_index(self) -> Option<u8> {
        match self {
            Self::BuiltIn { .. } => None,
            Self::Sd { index } => Some(index),
        }
    }

    pub const fn is_sd(self) -> bool {
        matches!(self, Self::Sd { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Power,
    Back,
    Confirm,
    Previous,
    Next,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Sample {
        button: Option<Button>,
        aux_raw: u16,
        nav_raw: u16,
        page_raw: u16,
        battery_mv: u16,
        battery_percent: u8,
    },
}

impl InputEvent {
    pub const fn button(button: Button) -> Self {
        Self::Sample {
            button: Some(button),
            aux_raw: 2000,
            nav_raw: 0,
            page_raw: 0,
            battery_mv: 4000,
            battery_percent: 77,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderKind {
    Boot,
    Page,
    Battery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayOrientation {
    LandscapeButtonsBottom,
    LandscapeButtonsTop,
    PortraitButtonsLeft,
    PortraitButtonsRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppView {
    Home,
    Library,
    Reading,
    Chapters,
    Sync,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HomeAction {
    Read,
    Files,
    Sync,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshPolicy {
    FastOnly,
    FullOnWake,
    FullEveryTen,
}

pub const DEFAULT_FULL_REFRESH_INTERVAL: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshPlanner {
    screen_on: bool,
    fast_refreshes: u8,
    last_request: Option<RenderRequest>,
    fast_refresh_enabled: bool,
    full_refresh_interval: u8,
    panel_shows_sleep_screen: bool,
}

impl RefreshPlanner {
    pub const fn new() -> Self {
        Self {
            screen_on: false,
            fast_refreshes: 0,
            last_request: None,
            fast_refresh_enabled: true,
            full_refresh_interval: DEFAULT_FULL_REFRESH_INTERVAL,
            panel_shows_sleep_screen: false,
        }
    }

    pub const fn with_fast_refresh_enabled(mut self, enabled: bool) -> Self {
        self.fast_refresh_enabled = enabled;
        self
    }

    pub const fn screen_on(&self) -> bool {
        self.screen_on
    }

    pub const fn last_request(&self) -> Option<RenderRequest> {
        self.last_request
    }

    pub fn mode_for(&self, request: RenderRequest) -> RefreshMode {
        let Some(last) = self.last_request else {
            // Cold boot leaves unknown pixels on the panel; only the deep
            // full waveform reliably clears them. After a display sleep the
            // panel still shows the sleep screen this firmware drew, so the
            // one-flicker clean is enough to wake.
            return if self.fast_refresh_enabled && self.panel_shows_sleep_screen {
                RefreshMode::FastClean
            } else {
                RefreshMode::Full
            };
        };
        if !self.fast_refresh_enabled || !self.screen_on {
            return RefreshMode::Full;
        }
        // Context changes need ghost cleanup, but the panel state is known
        // (the frame just shown), so the one-flicker clean suffices and the
        // multi-flash full waveform stays reserved for boot and sleep.
        if last.kind == RenderKind::Boot
            || request.view != last.view
            || request.book_id != last.book_id
            // A type-settings change redraws whole text columns; the clean
            // pass avoids fast-diff ghosting across the page.
            || request.font_size != last.font_size
            || request.line_spacing != last.line_spacing
            || Self::needs_clean_library_refresh(request, last)
        {
            return RefreshMode::FastClean;
        }
        match request.refresh_policy {
            RefreshPolicy::FastOnly | RefreshPolicy::FullOnWake => RefreshMode::Fast,
            RefreshPolicy::FullEveryTen if self.fast_refreshes >= self.full_refresh_interval => {
                RefreshMode::FastClean
            }
            RefreshPolicy::FullEveryTen => RefreshMode::Fast,
        }
    }

    pub fn record_render(&mut self, request: RenderRequest, mode: RefreshMode) {
        self.screen_on = true;
        self.last_request = Some(request);
        self.panel_shows_sleep_screen = false;
        if mode == RefreshMode::Fast {
            self.fast_refreshes = self.fast_refreshes.saturating_add(1);
        } else {
            self.fast_refreshes = 0;
        }
    }

    pub fn record_sleep(&mut self) {
        self.screen_on = false;
        self.fast_refreshes = 0;
        self.last_request = None;
        self.panel_shows_sleep_screen = true;
    }

    fn needs_clean_library_refresh(request: RenderRequest, last: RenderRequest) -> bool {
        request.library_count != last.library_count
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderRequest {
    pub kind: RenderKind,
    pub view: AppView,
    pub page: u32,
    pub page_count: u32,
    pub chapter: u8,
    pub selection: u8,
    pub book_id: u32,
    pub orientation: DisplayOrientation,
    pub refresh_policy: RefreshPolicy,
    pub font_size: FontSize,
    pub line_spacing: LineSpacing,
    pub last_button: Option<Button>,
    pub aux_raw: u16,
    pub nav_raw: u16,
    pub page_raw: u16,
    pub battery_mv: u16,
    pub battery_percent: u8,
    pub library_count: u8,
    pub dirty: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayCommand {
    Render(RenderRequest),
    Sleep,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageCommand {
    LoadCatalogCache,
    RefreshCatalog,
    OpenBook {
        request_id: u32,
        book_id: u32,
        index: u8,
        chapter: u8,
        target_pages: u16,
        type_settings: TypeSettings,
    },
    ExtendSection {
        request_id: u32,
        book_id: u32,
        index: u8,
        chapter: u8,
        target_pages: u16,
        type_settings: TypeSettings,
    },
    StoreProgress(PersistedAppState),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayEvent {
    Settled,
    Asleep,
    Library(LibraryEvent),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryEvent {
    Scanned {
        count: u8,
    },
    Loaded {
        book_id: u32,
        pages: u32,
        chapters: u8,
        chapter_pages: [u16; MAX_SD_CHAPTERS],
    },
    ChapterPage {
        book_id: u32,
        chapter: u8,
        page: u32,
    },
    Restored {
        book_id: u32,
        chapter: u8,
        page: u32,
        reading_orientation: u8,
        refresh_policy: u8,
        font_size: u8,
        line_spacing: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerEvent {
    Activity,
    DisplaySettled,
    DisplayAsleep,
    SleepNow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PersistedAppState {
    pub book_id: u32,
    pub chapter: u16,
    pub screen: u32,
    pub shell_orientation: u8,
    pub reading_orientation: u8,
    pub refresh_policy: u8,
    pub font_size: u8,
    pub line_spacing: u8,
    pub source_hash: u32,
    pub source_size: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReducerContext {
    pub builtin_book_count: u8,
    pub builtin_chapter_count: u8,
}

impl ReducerContext {
    pub const fn new(builtin_book_count: u8, builtin_chapter_count: u8) -> Self {
        Self {
            builtin_book_count,
            builtin_chapter_count,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReaderState {
    pub view: AppView,
    pub page: u32,
    pub selection: u8,
    pub chapter: u8,
    pub book_id: u32,
    pub orientation: DisplayOrientation,
    pub refresh_policy: RefreshPolicy,
    pub font_size: FontSize,
    pub line_spacing: LineSpacing,
    pub last_button: Option<Button>,
    pub aux_raw: u16,
    pub nav_raw: u16,
    pub page_raw: u16,
    pub battery_mv: u16,
    pub battery_percent: u8,
    pub library_count: u8,
    pub sd_page_count: u32,
    pub sd_chapter_count: u8,
    pub sd_chapter_pages: [u16; MAX_SD_CHAPTERS],
    pub read_request_pending: bool,
    pub dirty: Rect,
}

impl ReaderState {
    pub const fn boot() -> Self {
        Self {
            view: AppView::Home,
            page: 0,
            selection: 0,
            chapter: 0,
            book_id: 1,
            orientation: DisplayOrientation::LandscapeButtonsBottom,
            refresh_policy: RefreshPolicy::FullOnWake,
            font_size: FontSize::Medium,
            line_spacing: LineSpacing::Normal,
            last_button: None,
            aux_raw: 0,
            nav_raw: 0,
            page_raw: 0,
            battery_mv: 0,
            battery_percent: 100,
            library_count: 0,
            sd_page_count: 1,
            sd_chapter_count: 1,
            sd_chapter_pages: [0; MAX_SD_CHAPTERS],
            read_request_pending: false,
            dirty: Rect::FULL,
        }
    }

    pub fn apply_input(self, ctx: ReducerContext, event: InputEvent) -> Self {
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
            (AppView::Home, Some(button)) => {
                next = apply_home_action(next, home_action_for_button(button));
            }
            (AppView::Library, Some(Button::Next)) => {
                next.selection = wrap_next(self.selection, self.library_item_count(ctx));
            }
            (AppView::Library, Some(Button::Previous)) => {
                next.selection = wrap_prev(self.selection, self.library_item_count(ctx));
            }
            // Imprint key grammar: Back always zooms out one level,
            // Confirm always affirms the screen's primary action.
            (AppView::Library, Some(Button::Confirm)) => {
                if self.selection < self.library_count {
                    next.book_id = ReaderSource::sd(self.selection).book_id();
                    next.view = AppView::Reading;
                    next.chapter = 0;
                    next.selection = 0;
                    next.page = 0;
                    next.sd_page_count = 1;
                    next.sd_chapter_count = 1;
                    next.sd_chapter_pages = [0; MAX_SD_CHAPTERS];
                    next.read_request_pending = false;
                }
            }
            (AppView::Library, Some(Button::Back)) => {
                next.view = AppView::Home;
                next.selection = 0;
                next.read_request_pending = false;
            }
            (AppView::Reading, Some(Button::Next)) => {
                if ReaderSource::from_book_id(self.book_id).is_sd() {
                    if self.page + 1 < self.sd_page_count {
                        next.page = self.page + 1;
                    } else {
                        next.page = self.sd_page_count.saturating_sub(1);
                    }
                    next.chapter = next.sd_chapter_for_page(next.page);
                    next.selection = next.chapter;
                } else {
                    next.chapter = wrap_next(self.chapter, ctx.builtin_chapter_count.max(1));
                    next.selection = next.chapter;
                    next.page = 0;
                }
            }
            (AppView::Reading, Some(Button::Previous)) => {
                if ReaderSource::from_book_id(self.book_id).is_sd() {
                    if self.page > 0 {
                        next.page = self.page - 1;
                    }
                    next.chapter = next.sd_chapter_for_page(next.page);
                    next.selection = next.chapter;
                } else {
                    next.chapter = wrap_prev(self.chapter, ctx.builtin_chapter_count.max(1));
                    next.selection = next.chapter;
                    next.page = 0;
                }
            }
            (AppView::Reading, Some(Button::Confirm)) => {
                next.view = AppView::Chapters;
                next.selection = if ReaderSource::from_book_id(self.book_id).is_sd() {
                    self.sd_chapter_for_page(self.page)
                } else {
                    self.chapter
                };
            }
            (AppView::Reading, Some(Button::Back)) => {
                next.view = AppView::Home;
                next.selection = 0;
            }
            (AppView::Chapters, Some(Button::Next)) => {
                next.selection = wrap_next(self.selection, self.chapter_item_count(ctx));
            }
            (AppView::Chapters, Some(Button::Previous)) => {
                next.selection = wrap_prev(self.selection, self.chapter_item_count(ctx));
            }
            (AppView::Chapters, Some(Button::Confirm)) => {
                next.chapter = self.selection;
                next.page = if ReaderSource::from_book_id(self.book_id).is_sd() {
                    u32::from(
                        self.sd_chapter_pages
                            .get(self.selection as usize)
                            .copied()
                            .unwrap_or(0),
                    )
                } else {
                    0
                };
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
                next.selection = 0;
            }
        }

        next
    }

    pub fn apply_library_event(mut self, ctx: ReducerContext, event: LibraryEvent) -> Self {
        match event {
            LibraryEvent::Scanned { count } => {
                self.library_count = count;
                if self.view == AppView::Library {
                    if count == 0 {
                        self.selection = 0;
                    } else if self.selection >= count {
                        self.selection = count - 1;
                    }
                    self.dirty = Rect::FULL;
                    if self.read_request_pending {
                        self.read_request_pending = false;
                    }
                }
            }
            LibraryEvent::Loaded {
                book_id,
                pages,
                chapters,
                chapter_pages,
            } => {
                if self.book_id == book_id {
                    self.sd_page_count = pages.max(1);
                    self.sd_chapter_count = chapters.max(1);
                    self.sd_chapter_pages = chapter_pages;
                    self.page = self.page.min(self.sd_page_count.saturating_sub(1));
                    self.dirty = Rect::FULL;
                }
            }
            LibraryEvent::ChapterPage {
                book_id,
                chapter,
                page,
            } => {
                if self.book_id == book_id {
                    if let Some(slot) = self.sd_chapter_pages.get_mut(chapter as usize) {
                        *slot = page.min(u16::MAX as u32) as u16;
                    }
                    self.dirty = Rect::FULL;
                }
            }
            LibraryEvent::Restored {
                book_id,
                chapter,
                page,
                reading_orientation,
                refresh_policy,
                font_size,
                line_spacing,
            } => {
                self.book_id = book_id;
                self.chapter = chapter;
                self.page = page;
                if self.read_request_pending {
                    self.view = AppView::Reading;
                    self.selection = chapter;
                } else if self.view == AppView::Library {
                    let restored_index =
                        ReaderSource::from_book_id(book_id).sd_index().unwrap_or(0);
                    self.selection = restored_index.min(self.library_count.saturating_sub(1));
                } else {
                    self.selection = chapter;
                }
                self.read_request_pending = false;
                if let Some(orientation) = display_orientation_from_u8(reading_orientation) {
                    self.orientation = orientation;
                }
                if let Some(policy) = refresh_policy_from_u8(refresh_policy) {
                    self.refresh_policy = policy;
                }
                if let Some(size) = FontSize::from_u8(font_size) {
                    self.font_size = size;
                }
                if let Some(spacing) = LineSpacing::from_u8(line_spacing) {
                    self.line_spacing = spacing;
                }
                self.dirty = Rect::FULL;
            }
        }
        if self.view == AppView::Library {
            self.selection = self
                .selection
                .min(self.library_item_count(ctx).saturating_sub(1));
            self.dirty = Rect::FULL;
        }
        self
    }

    pub fn render_request(self, kind: RenderKind) -> RenderRequest {
        RenderRequest {
            kind,
            view: self.view,
            page: self.page,
            page_count: self.sd_page_count,
            chapter: self.chapter,
            selection: self.selection,
            book_id: self.book_id,
            orientation: self.orientation,
            refresh_policy: self.refresh_policy,
            font_size: self.font_size,
            line_spacing: self.line_spacing,
            last_button: self.last_button,
            aux_raw: self.aux_raw,
            nav_raw: self.nav_raw,
            page_raw: self.page_raw,
            battery_mv: self.battery_mv,
            battery_percent: self.battery_percent,
            library_count: self.library_count,
            dirty: self.dirty,
        }
    }

    pub fn persisted(self) -> PersistedAppState {
        PersistedAppState {
            book_id: self.book_id,
            chapter: self.chapter as u16,
            screen: self.page,
            shell_orientation: DisplayOrientation::PortraitButtonsLeft as u8,
            reading_orientation: self.orientation as u8,
            refresh_policy: self.refresh_policy as u8,
            font_size: self.font_size as u8,
            line_spacing: self.line_spacing as u8,
            source_hash: 0,
            source_size: 0,
        }
    }

    pub fn type_settings(self) -> TypeSettings {
        TypeSettings {
            size: self.font_size,
            spacing: self.line_spacing,
        }
    }

    pub fn library_item_count(self, ctx: ReducerContext) -> u8 {
        self.library_count.max(ctx.builtin_book_count).max(1)
    }

    pub fn chapter_item_count(self, ctx: ReducerContext) -> u8 {
        if ReaderSource::from_book_id(self.book_id).is_sd() {
            self.sd_chapter_count.max(1)
        } else {
            ctx.builtin_chapter_count.max(1)
        }
    }

    pub fn sd_chapter_for_page(self, page: u32) -> u8 {
        let mut selected = 0u8;
        for index in 0..self.sd_chapter_count.min(MAX_SD_CHAPTERS as u8) {
            if u32::from(self.sd_chapter_pages[index as usize]) <= page {
                selected = index;
            } else {
                break;
            }
        }
        selected
    }
}

pub fn display_orientation_from_u8(value: u8) -> Option<DisplayOrientation> {
    match value {
        0 => Some(DisplayOrientation::LandscapeButtonsBottom),
        1 => Some(DisplayOrientation::LandscapeButtonsTop),
        2 => Some(DisplayOrientation::PortraitButtonsLeft),
        3 => Some(DisplayOrientation::PortraitButtonsRight),
        _ => None,
    }
}

pub fn refresh_policy_from_u8(value: u8) -> Option<RefreshPolicy> {
    match value {
        0 => Some(RefreshPolicy::FastOnly),
        1 => Some(RefreshPolicy::FullOnWake),
        2 => Some(RefreshPolicy::FullEveryTen),
        _ => None,
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

fn home_action_for_button(button: Button) -> HomeAction {
    match button {
        // Home direct-maps the left-edge key column (top to bottom:
        // Back, Confirm, Previous, Next). Back zooms out of the book
        // onto the shelf; Confirm affirms continuing to read.
        Button::Back => HomeAction::Files,
        Button::Confirm => HomeAction::Read,
        Button::Previous => HomeAction::Sync,
        Button::Next | Button::Power => HomeAction::Settings,
    }
}

fn apply_home_action(mut state: ReaderState, action: HomeAction) -> ReaderState {
    state.selection = 0;
    state.read_request_pending = false;
    match action {
        HomeAction::Read => {
            if ReaderSource::from_book_id(state.book_id).is_sd() {
                state.view = AppView::Reading;
                state.selection = state.chapter;
            } else if state.library_count > 0 {
                state.view = AppView::Library;
            } else {
                state.view = AppView::Reading;
                state.book_id = 1;
            }
        }
        HomeAction::Files => {
            state.view = AppView::Library;
        }
        HomeAction::Sync => {
            state.view = AppView::Sync;
        }
        HomeAction::Settings => {
            state.view = AppView::Settings;
        }
    }
    state
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
        2 => {
            state.font_size = match state.font_size {
                FontSize::Small => FontSize::Medium,
                FontSize::Medium => FontSize::Large,
                FontSize::Large => FontSize::Small,
            };
        }
        _ => {
            state.line_spacing = match state.line_spacing {
                LineSpacing::Compact => LineSpacing::Normal,
                LineSpacing::Normal => LineSpacing::Relaxed,
                LineSpacing::Relaxed => LineSpacing::Compact,
            };
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTX: ReducerContext = ReducerContext::new(1, 3);

    fn press(state: ReaderState, button: Button) -> ReaderState {
        state.apply_input(CTX, InputEvent::button(button))
    }

    #[test]
    fn home_navigation_opens_primary_views() {
        assert_eq!(
            press(ReaderState::boot(), Button::Confirm).view,
            AppView::Reading
        );
        assert_eq!(
            press(ReaderState::boot(), Button::Back).view,
            AppView::Library
        );
        assert_eq!(
            press(ReaderState::boot(), Button::Previous).view,
            AppView::Sync
        );
        assert_eq!(
            press(ReaderState::boot(), Button::Next).view,
            AppView::Settings
        );
    }

    #[test]
    fn reader_source_maps_sd_catalog_indices_to_book_ids() {
        assert_eq!(
            ReaderSource::from_book_id(1),
            ReaderSource::BuiltIn { book_id: 1 }
        );
        assert_eq!(ReaderSource::sd(0).book_id(), 2);
        assert_eq!(ReaderSource::sd(7).book_id(), 9);
        assert_eq!(ReaderSource::from_book_id(9).sd_index(), Some(7));
    }

    #[test]
    fn library_open_key_opens_sd_book() {
        let state = press(ReaderState::boot(), Button::Back)
            .apply_library_event(CTX, LibraryEvent::Scanned { count: 2 });
        let state = press(press(state, Button::Next), Button::Confirm);
        assert_eq!(state.view, AppView::Reading);
        assert_eq!(state.book_id, 3);
    }

    #[test]
    fn library_back_key_returns_home_without_opening() {
        let state = press(ReaderState::boot(), Button::Back)
            .apply_library_event(CTX, LibraryEvent::Scanned { count: 2 });
        let state = press(press(state, Button::Next), Button::Back);
        assert_eq!(state.view, AppView::Home);
        assert_eq!(state.book_id, 1);
    }

    #[test]
    fn reading_next_previous_bounds_sd_pages() {
        let mut state = ReaderState::boot();
        state.view = AppView::Reading;
        state.book_id = 2;
        state.sd_page_count = 2;
        assert_eq!(press(state, Button::Next).page, 1);
        assert_eq!(press(press(state, Button::Next), Button::Next).page, 1);
        assert_eq!(press(press(state, Button::Next), Button::Previous).page, 0);
    }

    #[test]
    fn chapter_selection_changes_reading_chapter() {
        let mut state = ReaderState::boot();
        state.view = AppView::Reading;
        state.book_id = 1;
        let state = press(state, Button::Confirm);
        assert_eq!(state.view, AppView::Chapters);
        let state = press(press(state, Button::Next), Button::Confirm);
        assert_eq!(state.view, AppView::Reading);
        assert_eq!(state.chapter, 1);
    }

    #[test]
    fn sd_chapter_selection_uses_toc_page_target() {
        let mut state = ReaderState::boot();
        state.view = AppView::Reading;
        state.book_id = ReaderSource::sd(0).book_id();
        state.sd_page_count = 40;
        state.sd_chapter_count = 3;
        state.sd_chapter_pages[0] = 0;
        state.sd_chapter_pages[1] = 12;
        state.sd_chapter_pages[2] = 24;

        let state = press(state, Button::Confirm);
        let state = press(press(state, Button::Next), Button::Confirm);

        assert_eq!(state.view, AppView::Reading);
        assert_eq!(state.chapter, 1);
        assert_eq!(state.page, 12);
    }

    #[test]
    fn sd_page_navigation_tracks_chapter_without_wrapping_pages() {
        let mut state = ReaderState::boot();
        state.view = AppView::Reading;
        state.book_id = ReaderSource::sd(0).book_id();
        state.page = 11;
        state.sd_page_count = 40;
        state.sd_chapter_count = 3;
        state.sd_chapter_pages[0] = 0;
        state.sd_chapter_pages[1] = 12;
        state.sd_chapter_pages[2] = 24;

        let state = press(state, Button::Next);

        assert_eq!(state.page, 12);
        assert_eq!(state.chapter, 1);
    }

    #[test]
    fn sd_page_navigation_tracks_chapters_past_first_screen() {
        let mut state = ReaderState::boot();
        state.view = AppView::Reading;
        state.book_id = ReaderSource::sd(0).book_id();
        state.sd_page_count = 400;
        state.sd_chapter_count = 40;
        for index in 0..40 {
            state.sd_chapter_pages[index] = (index as u16) * 10;
        }
        state.page = 249;
        state.chapter = 23;

        let state = press(state, Button::Next);

        assert_eq!(state.page, 250);
        assert_eq!(state.chapter, 25);
        assert_eq!(state.selection, 25);
    }

    #[test]
    fn catalog_scan_does_not_auto_open_from_files() {
        let state = press(ReaderState::boot(), Button::Back);
        assert_eq!(state.view, AppView::Library);
        assert!(!state.read_request_pending);

        let state = state.apply_library_event(CTX, LibraryEvent::Scanned { count: 2 });
        assert_eq!(state.view, AppView::Library);
        assert_eq!(state.library_count, 2);
        assert!(!state.read_request_pending);
    }

    #[test]
    fn library_open_before_scan_stays_in_files() {
        let state = press(ReaderState::boot(), Button::Back);
        let state = press(state, Button::Confirm);
        assert_eq!(state.view, AppView::Library);
        assert_eq!(state.book_id, 1);

        let state = state.apply_library_event(CTX, LibraryEvent::Scanned { count: 2 });
        assert_eq!(state.view, AppView::Library);
        assert_eq!(state.library_count, 2);
    }

    #[test]
    fn settings_change_key_cycles_orientation_and_refresh_policy() {
        let state = press(ReaderState::boot(), Button::Next);
        let state = press(state, Button::Confirm);
        assert_eq!(state.orientation, DisplayOrientation::LandscapeButtonsTop);
        let state = press(state, Button::Next);
        let state = press(state, Button::Confirm);
        assert_eq!(state.refresh_policy, RefreshPolicy::FullEveryTen);
        let state = press(state, Button::Back);
        assert_eq!(state.view, AppView::Home);
    }

    #[test]
    fn settings_change_key_cycles_type_size_and_line_spacing() {
        let state = press(ReaderState::boot(), Button::Next);
        let state = press(press(state, Button::Next), Button::Next);
        assert_eq!(state.selection, 2);
        let state = press(state, Button::Confirm);
        assert_eq!(state.font_size, FontSize::Large);
        let state = press(press(state, Button::Confirm), Button::Confirm);
        assert_eq!(state.font_size, FontSize::Medium);

        let state = press(state, Button::Next);
        assert_eq!(state.selection, 3);
        let state = press(state, Button::Confirm);
        assert_eq!(state.line_spacing, LineSpacing::Relaxed);
        let state = press(state, Button::Next);
        assert_eq!(state.selection, 0, "selection wraps after the last row");
    }

    #[test]
    fn library_restore_updates_progress_and_preferences() {
        let state = ReaderState::boot().apply_library_event(
            CTX,
            LibraryEvent::Restored {
                book_id: 2,
                chapter: 4,
                page: 12,
                reading_orientation: DisplayOrientation::PortraitButtonsRight as u8,
                refresh_policy: RefreshPolicy::FastOnly as u8,
                font_size: FontSize::Large as u8,
                line_spacing: LineSpacing::Compact as u8,
            },
        );
        assert_eq!(state.book_id, 2);
        assert_eq!(state.chapter, 4);
        assert_eq!(state.page, 12);
        assert_eq!(state.orientation, DisplayOrientation::PortraitButtonsRight);
        assert_eq!(state.refresh_policy, RefreshPolicy::FastOnly);
        assert_eq!(state.font_size, FontSize::Large);
        assert_eq!(state.line_spacing, LineSpacing::Compact);
    }

    #[test]
    fn refresh_plan_cleans_after_type_settings_change() {
        let mut planner = RefreshPlanner::new();
        let mut state = ReaderState::boot();
        state.view = AppView::Settings;
        let request = state.render_request(RenderKind::Page);
        planner.record_render(request, RefreshMode::Full);

        state.font_size = FontSize::Large;
        assert_eq!(
            planner.mode_for(state.render_request(RenderKind::Page)),
            RefreshMode::FastClean
        );
    }

    #[test]
    fn refresh_plan_uses_fast_clean_for_context_changes_and_fast_for_selection() {
        let mut planner = RefreshPlanner::new();
        let mut request = ReaderState::boot().render_request(RenderKind::Boot);

        // Cold boot is the only render where panel contents are unknown,
        // so it keeps the deep multi-flash full waveform.
        assert_eq!(planner.mode_for(request), RefreshMode::Full);
        planner.record_render(request, RefreshMode::Full);

        request.kind = RenderKind::Page;
        assert_eq!(planner.mode_for(request), RefreshMode::FastClean);

        request.view = AppView::Settings;
        assert_eq!(planner.mode_for(request), RefreshMode::FastClean);
        planner.record_render(request, RefreshMode::FastClean);

        // Cursor moves inside Settings ride the fast differential refresh
        // against the prestaged previous frame; leaving the view is a view
        // change, which gets the one-flicker cleaning refresh.
        request.selection = 1;
        assert_eq!(planner.mode_for(request), RefreshMode::Fast);
        planner.record_render(request, RefreshMode::Fast);

        request.view = AppView::Home;
        assert_eq!(planner.mode_for(request), RefreshMode::FastClean);
    }

    #[test]
    fn refresh_plan_keeps_library_selection_fast() {
        let mut planner = RefreshPlanner::new();
        let mut state = ReaderState::boot();
        state.view = AppView::Library;
        state.library_count = 3;
        let mut request = state.render_request(RenderKind::Page);

        planner.record_render(request, RefreshMode::Full);
        request.selection = 1;

        assert_eq!(planner.mode_for(request), RefreshMode::Fast);
    }

    #[test]
    fn refresh_plan_keeps_chapter_selection_fast() {
        let mut planner = RefreshPlanner::new();
        let mut state = ReaderState::boot();
        state.view = AppView::Chapters;
        let mut request = state.render_request(RenderKind::Page);

        planner.record_render(request, RefreshMode::Full);
        request.selection = 1;

        assert_eq!(planner.mode_for(request), RefreshMode::Fast);
    }

    #[test]
    fn refresh_plan_counts_fast_refreshes_and_resets_on_sleep() {
        let mut planner = RefreshPlanner::new();
        let mut state = ReaderState::boot();
        state.refresh_policy = RefreshPolicy::FullEveryTen;
        let request = state.render_request(RenderKind::Page);
        planner.record_render(request, RefreshMode::Full);

        for _ in 0..DEFAULT_FULL_REFRESH_INTERVAL {
            assert_eq!(planner.mode_for(request), RefreshMode::Fast);
            planner.record_render(request, RefreshMode::Fast);
        }
        // Periodic mid-reading cleanup uses the one-flicker clean instead
        // of the jarring multi-flash full waveform.
        assert_eq!(planner.mode_for(request), RefreshMode::FastClean);

        // After a display sleep the panel shows the sleep screen the
        // firmware drew, so wake also needs only the one-flicker clean.
        planner.record_sleep();
        assert_eq!(planner.mode_for(request), RefreshMode::FastClean);
    }

    #[test]
    fn refresh_plan_keeps_deep_full_for_cold_boot_only() {
        let mut planner = RefreshPlanner::new();
        let request = ReaderState::boot().render_request(RenderKind::Boot);

        // Cold boot: unknown panel contents, deep full waveform.
        assert_eq!(planner.mode_for(request), RefreshMode::Full);
        planner.record_render(request, RefreshMode::Full);

        // Wake after sleep: known sleep-screen contents, one-flicker clean.
        planner.record_sleep();
        assert_eq!(planner.mode_for(request), RefreshMode::FastClean);
        planner.record_render(request, RefreshMode::FastClean);
        planner.record_sleep();

        // Disabling fast refresh falls back to the deep full everywhere.
        let conservative = RefreshPlanner::new().with_fast_refresh_enabled(false);
        assert_eq!(conservative.mode_for(request), RefreshMode::Full);
    }
}
