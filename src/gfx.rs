//! Drawing on Sunrise's `DrawContext`: legacy `§` text as styled components, scaled text through a canvas scope,
//! items by id, Jade's own sprites (shipped in `assets/jade/textures/gui/sprites`, so in the GUI atlas as
//! `jade:<name>`), and a transform for the overlay's Display Scale and its fade.
use mod_api::client::hud_layout::PoseOp;
use mod_api::text::TextStyle;
use mod_api::*;
use std::cell::RefCell;
use std::collections::HashMap;

/// Text with legacy codes (`§a`, `§l`, `§r`) and `§#RRGGBB` colours as a component: spans with their styles, under a
/// root with none (the draw's colour shows through where a span has no colour).
pub fn rich(s: &str) -> Text {
    let mut root = Text::literal("");
    let mut style = TextStyle::default();
    let mut buf = String::new();
    let mut chars = s.chars().peekable();
    let flush = |root: &mut Text, buf: &mut String, style: &TextStyle| {
        if !buf.is_empty() {
            root.children
                .push(Text::literal(std::mem::take(buf)).with_style(style.clone()));
        }
    };
    while let Some(c) = chars.next() {
        if c != '§' {
            buf.push(c);
            continue;
        }
        let Some(code) = chars.next() else { break };
        flush(&mut root, &mut buf, &style);
        let code = code.to_ascii_lowercase();
        let named = |n: &str| Some(n.to_owned());
        let color = match code {
            '0' => named("black"),
            '1' => named("dark_blue"),
            '2' => named("dark_green"),
            '3' => named("dark_aqua"),
            '4' => named("dark_red"),
            '5' => named("dark_purple"),
            '6' => named("gold"),
            '7' => named("gray"),
            '8' => named("dark_gray"),
            '9' => named("blue"),
            'a' => named("green"),
            'b' => named("aqua"),
            'c' => named("red"),
            'd' => named("light_purple"),
            'e' => named("yellow"),
            'f' => named("white"),
            '#' => {
                let hex: String = (0..6).filter_map(|_| chars.next()).collect();
                Some(format!("#{}", hex.to_uppercase()))
            }
            _ => None,
        };
        if let Some(color) = color {
            // A colour code resets the decorations, as vanilla's legacy formatting does.
            style = TextStyle::default();
            style.color = Some(color);
            continue;
        }
        match code {
            'l' => style.bold = Some(true),
            'o' => style.italic = Some(true),
            'n' => style.underlined = Some(true),
            'm' => style.strikethrough = Some(true),
            'k' => style.obfuscated = Some(true),
            'r' => style = TextStyle::default(),
            _ => {}
        }
    }
    flush(&mut root, &mut buf, &style);
    root
}

thread_local! {
    /// Item ids to keys, built once from `item_keys()`.
    static ITEMS: RefCell<Option<HashMap<String, ItemKey>>> = const { RefCell::new(None) };
    /// Text widths, by text.
    static WIDTHS: RefCell<HashMap<String, f32>> = RefCell::new(HashMap::new());
}

pub fn item_key(h: &dyn Host, id: &str) -> Option<ItemKey> {
    ITEMS.with_borrow_mut(|m| {
        let map = m.get_or_insert_with(|| {
            h.item_keys()
                .into_iter()
                .map(|k| (k.id(h).unwrap_or_default().to_owned(), k))
                .collect()
        });
        map.get(id).copied()
    })
}

/// A text's width in GUI pixels (codes left out of what is measured, bold counted).
pub fn width(h: &dyn ClientHost, s: &str) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    if let Some(w) = WIDTHS.with_borrow(|m| m.get(s).copied()) {
        return w;
    }
    let w = h.text_width(&rich(s)) as f32;
    WIDTHS.with_borrow_mut(|m| {
        if m.len() > 4096 {
            m.clear();
        }
        m.insert(s.to_owned(), w);
    });
    w
}

/// ARGB with its alpha multiplied.
pub fn fade(argb: u32, alpha: f32) -> u32 {
    let a = ((argb >> 24) as f32 * alpha.clamp(0.0, 1.0)) as u32;
    (argb & 0x00FF_FFFF) | (a << 24)
}

/// A draw with a transform: `screen = pivot + (p - pivot) * scale`, and every colour's alpha times `alpha`.
pub struct Gfx<'a> {
    pub d: &'a mut dyn DrawContext,
    pub h: &'a dyn ClientHost,
    pub pivot: [f32; 2],
    pub scale: f32,
    pub alpha: f32,
}

impl<'a> Gfx<'a> {
    pub fn new(d: &'a mut dyn DrawContext, h: &'a dyn ClientHost) -> Self {
        Self {
            d,
            h,
            pivot: [0.0; 2],
            scale: 1.0,
            alpha: 1.0,
        }
    }

    pub fn size(&self) -> [f32; 2] {
        self.d.size()
    }

    fn p(&self, [x, y]: [f32; 2]) -> [f32; 2] {
        [
            self.pivot[0] + (x - self.pivot[0]) * self.scale,
            self.pivot[1] + (y - self.pivot[1]) * self.scale,
        ]
    }

    fn r(&self, [x, y, w, h]: [f32; 4]) -> [f32; 4] {
        let [x, y] = self.p([x, y]);
        [x, y, w * self.scale, h * self.scale]
    }

    pub fn fill(&mut self, rect: [f32; 4], argb: u32) {
        if rect[2] <= 0.0 || rect[3] <= 0.0 {
            return;
        }
        let r = self.r(rect);
        let c = fade(argb, self.alpha);
        if c >> 24 != 0 {
            self.d.fill(r, c);
        }
    }

    /// A vertical gradient, as rows of one GUI pixel.
    pub fn gradient(&mut self, [x, y, w, h]: [f32; 4], top: u32, bottom: u32) {
        let rows = h.max(0.0).ceil() as i32;
        for i in 0..rows {
            let t = if rows > 1 {
                i as f32 / (rows - 1) as f32
            } else {
                0.0
            };
            let mix = |s: u32| {
                let a = ((top >> s) & 0xFF) as f32;
                let b = ((bottom >> s) & 0xFF) as f32;
                ((a + (b - a) * t).round() as u32 & 0xFF) << s
            };
            let c = mix(24) | mix(16) | mix(8) | mix(0);
            self.fill([x, y + i as f32, w, 1.0f32.min(h - i as f32)], c);
        }
    }

    /// Text at `at`, `size` times the transform's scale; `base` (ARGB) for what has no colour of its own.
    pub fn text_scaled(&mut self, s: &str, at: [f32; 2], size: f32, base: u32, shadow: bool) {
        if s.is_empty() {
            return;
        }
        let [x, y] = self.p(at);
        let k = self.scale * size;
        let color = fade(base, self.alpha);
        if color >> 24 == 0 {
            return;
        }
        paint_text(self.d, &rich(s), [x, y], k, color, shadow);
    }

    pub fn text(&mut self, s: &str, at: [f32; 2], base: u32, shadow: bool) {
        self.text_scaled(s, at, 1.0, base, shadow);
    }

    /// An item's icon by id, `size` GUI pixels square before the transform. False for an unknown item.
    pub fn item(&mut self, id: &str, at: [f32; 2], size: f32) -> bool {
        let Some(key) = item_key(self.h, id) else {
            return false;
        };
        let at = self.p(at);
        self.d.item(key, at, size * self.scale)
    }

    /// An item's icon with optional component patch through `DrawContext::item_stack`, falling back to `item`.
    pub fn item_with(
        &mut self,
        id: &str,
        components: Option<&Compound>,
        at: [f32; 2],
        size: f32,
    ) -> bool {
        let Some(key) = item_key(self.h, id) else {
            return false;
        };
        if let Some(patch) = components
            && !patch.is_empty()
        {
            let mut stack = ItemStack::of(key);
            if stack.set_components(patch.clone()).is_ok() {
                let at = self.p(at);
                if self.d.item_stack(&stack, at, size * self.scale) {
                    return true;
                }
            }
        }
        let at = self.p(at);
        self.d.item(key, at, size * self.scale)
    }

    /// A fluid's look (`DrawContext::fluid`), `size` GUI pixels square before the transform.
    pub fn fluid(&mut self, id: &str, at: [f32; 2], size: f32) {
        let at = self.p(at);
        self.d.fluid(id, at, size * self.scale);
    }

    /// A GUI sprite (atlas name) stretched over `rect`, tinted white with the transform's alpha. False when the atlas
    /// hasn't it.
    pub fn sprite(&mut self, name: &str, src: [f32; 4], rect: [f32; 4], tint: u32) -> bool {
        let r = self.r(rect);
        let t = fade(tint, self.alpha);
        self.d.sprite(name, src, r, Some(t))
    }

    /// A nine-sliced GUI sprite (its `.png.mcmeta` scaling) over `rect`.
    pub fn nine(&mut self, name: &str, rect: [f32; 4], tint: u32) {
        let r = self.r(rect);
        let t = fade(tint, self.alpha);
        self.d.gui_nine_slice(name, r, t);
    }
}

/// `text` at `at`, scaled by `k`, in a draw group of its own: the styled text through a canvas, in a child scope that
/// translates and scales. A refused draw adds nothing.
fn paint_text(
    d: &mut dyn DrawContext,
    text: &Text,
    at: [f32; 2],
    k: f32,
    color: u32,
    shadow: bool,
) {
    let _ = d.draw_gui(&mut |canvas| {
        canvas.scope(
            &[PoseOp::Translate(at[0], at[1]), PoseOp::Scale(k, k)],
            None,
            &mut |child| child.text(text, [0.0, 0.0], color, shadow),
        )
    });
}

/// `Util.getMillis`' zero: when the mod first drew a scrolling label.
fn started() -> std::time::Instant {
    static AT: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *AT.get_or_init(std::time::Instant::now)
}

/// renderScrollingString's offset for text `overflow` pixels too wide, `secs` seconds in: it eases from the start to
/// the end and back, a round trip every `max(overflow / 2, 3)` seconds, in whole pixels.
pub fn scroll_offset(overflow: f32, secs: f64) -> f32 {
    let overflow = f64::from(overflow.max(0.0));
    let period = (overflow * 0.5).max(3.0);
    let t = (std::f64::consts::FRAC_PI_2 * (std::f64::consts::TAU * secs / period).cos()).sin()
        / 2.0
        + 0.5;
    (t * overflow) as i32 as f32
}

/// ARGB from its channels.
pub fn rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    u32::from_be_bytes([a, r, g, b])
}

/// Literal text in one colour (`0xRRGGBB`).
pub fn colored(s: impl Into<String>, rgb: u32) -> Text {
    let mut style = TextStyle::default();
    style.color = Some(format!("#{:06X}", rgb & 0xFF_FFFF));
    Text::literal(s.into()).with_style(style)
}

/// AbstractSliderButton's value for a pointer at `x` over a track at `track_x`, `track_w` wide (its handle 8 wide).
pub fn slider_value(track_x: f32, track_w: f32, x: f32) -> f32 {
    ((x - (track_x + 4.0)) / (track_w - 8.0).max(1.0)).clamp(0.0, 1.0)
}

impl Gfx<'_> {
    /// Text in `rgb` (`0xRRGGBB`, opaque; ARGB as it is when it has alpha).
    pub fn text_rgb(&mut self, s: &str, at: [f32; 2], rgb: u32, shadow: bool) {
        let argb = if rgb >> 24 == 0 {
            0xFF00_0000 | rgb
        } else {
            rgb
        };
        self.text(s, at, argb, shadow);
    }

    /// A component at `at`, `base` (ARGB) where it has no colour.
    pub fn text_comp(&mut self, t: &Text, at: [f32; 2], base: u32, shadow: bool) {
        let [x, y] = [
            self.pivot[0] + (at[0] - self.pivot[0]) * self.scale,
            self.pivot[1] + (at[1] - self.pivot[1]) * self.scale,
        ];
        let k = self.scale;
        let color = fade(base, self.alpha);
        paint_text(self.d, t, [x, y], k, color, shadow);
    }

    /// A component between `x0` and `x1`, centred on the band `y0`..`y1` (AbstractWidget.renderScrollingString): one
    /// that doesn't fit slides back and forth inside the band, clipped to it.
    pub fn scrolling(&mut self, t: &Text, x0: f32, x1: f32, y0: f32, y1: f32, base: u32) {
        let w = self.h.text_width(t) as f32;
        let y = ((y0 + y1 - 9.0) / 2.0 + 1.0).floor();
        if w <= x1 - x0 {
            self.text_comp(t, [x0, y], base, true);
            return;
        }
        let offset = scroll_offset(w - (x1 - x0), started().elapsed().as_secs_f64());
        self.clipped([x0, y0, x1 - x0, y1 - y0], |g| {
            g.text_comp(t, [x0 - offset, y], base, true)
        });
    }

    /// `GuiGraphics.enableScissor` over `rect` (before the transform), `disableScissor` after `draw`, which draws through
    /// the same transform on the clipped context. When the game refuses the scissor, `draw` is not called.
    pub fn clipped(&mut self, rect: [f32; 4], draw: impl FnOnce(&mut Gfx<'_>)) {
        let r = self.r(rect);
        let (h, pivot, scale, alpha) = (self.h, self.pivot, self.scale, self.alpha);
        let mut draw = Some(draw);
        let _ = self.d.with_scissor(r, &mut |d| {
            if let Some(draw) = draw.take() {
                draw(&mut Gfx {
                    d,
                    h,
                    pivot,
                    scale,
                    alpha,
                });
            }
        });
    }

    /// A button as the game draws one (AbstractButton.renderWidget): the state's nine-sliced sprite, inactive before
    /// highlighted, and the label centred, or scrolling between 2 pixels in from each side when it doesn't fit, white or
    /// grey (`0xA0A0A0`) when inactive.
    /// Drawn here since Dev f8aa80e took `DrawContext::widget_button` away (mounted screen widgets replace it).
    pub fn button(&mut self, rect: [f32; 4], label: &Text, active: bool, hover: bool) {
        let [x, y, w, h] = rect;
        let sprite = if !active {
            "widget/button_disabled"
        } else if hover {
            "widget/button_highlighted"
        } else {
            "widget/button"
        };
        self.d.gui_nine_slice(sprite, rect, 0xFFFF_FFFF);
        let color = if active { 0xFFFF_FFFF } else { 0xFFA0_A0A0 };
        // ActiveTextCollector.acceptScrollingWithDefaultCenter: a label that fits is centred on the button,
        // `clamp(centerX, left + width / 2, right - width / 2)` in whole pixels.
        let (left, right) = ((x + 2.0) as i32, (x + w - 2.0) as i32);
        let width = self.h.text_width(label);
        if width <= right - left {
            let centre = ((left + right) / 2).clamp(left + width / 2, right - width / 2);
            let top = ((y + y + h - 9.0) / 2.0 + 1.0).floor();
            self.text_comp(label, [(centre - width / 2) as f32, top], color, true);
            return;
        }
        self.scrolling(label, x + 2.0, x + w - 2.0, y, y + h, color);
    }

    /// A nine-sliced GUI sprite by its full atlas name.
    pub fn nine_raw(&mut self, sprite: &str, rect: [f32; 4], color: u32) {
        self.d.gui_nine_slice(sprite, rect, color);
    }

    /// AbstractSliderButton: the track, the handle at `t`, the value centred.
    #[allow(clippy::too_many_arguments)]
    pub fn slider(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        label: &Text,
        t: f32,
        hover: bool,
        active: bool,
        dragging: bool,
    ) {
        let track = if hover || dragging {
            "widget/slider_highlighted"
        } else {
            "widget/slider"
        };
        self.d.gui_nine_slice(track, [x, y, w, 20.0], 0xFFFF_FFFF);
        let handle = if (hover || dragging) && active {
            "widget/slider_handle_highlighted"
        } else {
            "widget/slider_handle"
        };
        let hx = x + (t.clamp(0.0, 1.0) * (w - 8.0)).floor();
        self.d
            .gui_nine_slice(handle, [hx, y, 8.0, 20.0], 0xFFFF_FFFF);
        let tw = self.h.text_width(label) as f32;
        let color = if active { 0xFFFF_FFFF } else { 0xFFA0_A0A0 };
        self.text_comp(label, [x + ((w - tw) / 2.0).floor(), y + 6.0], color, true);
    }

    /// A vanilla tooltip at the pointer.
    pub fn tooltip(&mut self, lines: &[(Text, u32)], x: f32, y: f32) {
        let texts: Vec<Text> = lines.iter().map(|(t, _)| t.clone()).collect();
        let _ = self.d.gui_tooltip(&texts, [x, y]);
    }
}

#[cfg(test)]
mod tests {
    use super::scroll_offset;

    #[test]
    fn scrolling_label_round_trip() {
        // A 40 px overflow: a 20 s round trip; at the start it is at the far end, half way at a quarter, back at a half.
        assert_eq!(scroll_offset(40.0, 0.0), 40.0);
        assert_eq!(scroll_offset(40.0, 5.0), 20.0);
        assert_eq!(scroll_offset(40.0, 10.0), 0.0);
        assert_eq!(scroll_offset(40.0, 20.0), 40.0);
        // A small overflow still takes three seconds.
        assert_eq!(scroll_offset(2.0, 1.5), 0.0);
        assert_eq!(scroll_offset(0.0, 7.0), 0.0);
    }
}
