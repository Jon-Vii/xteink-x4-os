use crate::font::{draw_str, GLYPH_HEIGHT, GLYPH_WIDTH};
use display::fb::Framebuffer;
use display::render::draw_rect;

pub const MAX_WIDGETS: usize = 64;
pub type WidgetId = u8;
pub const NO_WIDGET: WidgetId = u8::MAX;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i16,
    pub y: i16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub const fn new(x: i16, y: i16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }
}

const _: () = assert!(
    MAX_WIDGETS * core::mem::size_of::<Rect>() < 1024,
    "widget rect array exceeds 1 KB — review MAX_WIDGETS"
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WidgetKind {
    Panel,
    Text,
    Button,
    ProgressBar { percent: u8 },
}

impl Default for WidgetKind {
    fn default() -> Self {
        Self::Panel
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextSlot {
    pub offset: u16,
    pub len: u16,
}

impl TextSlot {
    pub const fn new(offset: u16, len: u16) -> Self {
        Self { offset, len }
    }
}

pub struct Arena {
    pub kinds: [WidgetKind; MAX_WIDGETS],
    pub rects: [Rect; MAX_WIDGETS],
    pub parents: [WidgetId; MAX_WIDGETS],
    pub texts: [TextSlot; MAX_WIDGETS],
    pub visible: [bool; MAX_WIDGETS],
    pub count: usize,
}

impl Arena {
    pub const fn new() -> Self {
        Self {
            kinds: [WidgetKind::Panel; MAX_WIDGETS],
            rects: [Rect::new(0, 0, 0, 0); MAX_WIDGETS],
            parents: [NO_WIDGET; MAX_WIDGETS],
            texts: [TextSlot::new(0, 0); MAX_WIDGETS],
            visible: [false; MAX_WIDGETS],
            count: 0,
        }
    }

    /// Adds a widget to the flat arena layout.
    pub fn add_widget(
        &mut self,
        kind: WidgetKind,
        rect: Rect,
        parent: WidgetId,
        text: Option<TextSlot>,
    ) -> WidgetId {
        if self.count >= MAX_WIDGETS {
            return NO_WIDGET;
        }
        let id = self.count as WidgetId;
        self.kinds[self.count] = kind;
        self.rects[self.count] = rect;
        self.parents[self.count] = parent;
        self.texts[self.count] = text.unwrap_or_default();
        self.visible[self.count] = true;
        self.count += 1;
        id
    }

    /// Recursively computes absolute screen coordinates based on parent hierarchy.
    pub fn absolute_rect(&self, id: WidgetId) -> Rect {
        if id == NO_WIDGET || id as usize >= self.count {
            return Rect::default();
        }
        let mut r = self.rects[id as usize];
        let mut parent = self.parents[id as usize];
        while parent != NO_WIDGET && (parent as usize) < self.count {
            let pr = self.rects[parent as usize];
            r.x += pr.x;
            r.y += pr.y;
            parent = self.parents[parent as usize];
        }
        r
    }

    /// Performs linear hit-testing to find the front-most widget under the coordinate.
    pub fn hit_test(&self, x: i16, y: i16) -> WidgetId {
        let mut hit = NO_WIDGET;
        for i in 0..self.count {
            if !self.visible[i] {
                continue;
            }
            let abs_r = self.absolute_rect(i as WidgetId);
            if x >= abs_r.x
                && x < abs_r.x + abs_r.w as i16
                && y >= abs_r.y
                && y < abs_r.y + abs_r.h as i16
            {
                hit = i as WidgetId;
            }
        }
        hit
    }

    /// Renders the widget tree arena directly into the framebuffer.
    /// Uses string_pool to look up TextSlot offsets.
    pub fn draw_into(&self, fb: &mut Framebuffer, string_pool: &str) {
        for i in 0..self.count {
            if !self.visible[i] {
                continue;
            }
            let abs_r = self.absolute_rect(i as WidgetId);
            let x = abs_r.x as usize;
            let y = abs_r.y as usize;
            let w = abs_r.w as usize;
            let h = abs_r.h as usize;

            match self.kinds[i] {
                WidgetKind::Panel => {
                    // Draw outer panel box (wireframe, black)
                    draw_rect(fb, x, y, w, h, false, false);
                }
                WidgetKind::Button => {
                    // Button has a double frame or filled background depending on selection
                    draw_rect(fb, x, y, w, h, false, false);
                    draw_rect(fb, x + 2, y + 2, w - 4, h - 4, false, false);
                }
                WidgetKind::ProgressBar { percent } => {
                    // Render outer border, then fill portion proportional to percent
                    draw_rect(fb, x, y, w, h, false, false);
                    let fill_w = (w - 4) * (percent.min(100) as usize) / 100;
                    if fill_w > 0 {
                        draw_rect(fb, x + 2, y + 2, fill_w, h - 4, true, false);
                    }
                }
                WidgetKind::Text => {
                    let slot = self.texts[i];
                    if slot.len > 0 {
                        let pool_len = string_pool.len() as u16;
                        let start = slot.offset.min(pool_len) as usize;
                        let end = (slot.offset + slot.len).min(pool_len) as usize;
                        let txt = &string_pool[start..end];
                        // Clip to the widget's box. Truncate horizontally by the
                        // number of glyphs that fit; truncate vertically by skipping
                        // the draw if the row would overflow.
                        if h >= GLYPH_HEIGHT && w >= GLYPH_WIDTH {
                            let max_chars = w / GLYPH_WIDTH;
                            let clipped = if txt.len() > max_chars {
                                // truncate at a char boundary
                                let mut cut = max_chars;
                                while cut > 0 && !txt.is_char_boundary(cut) {
                                    cut -= 1;
                                }
                                &txt[..cut]
                            } else {
                                txt
                            };
                            draw_str(fb, clipped, x, y, false);
                        }
                    }
                }
            }
        }
    }
}
