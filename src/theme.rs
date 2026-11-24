//! Jade's themes (ThemeHelper, Theme, BoxStyle, TextSetting, ColorPalette): `jade_themes/**.json`, this mod's (the
//! jar's) and whatever resource packs add, read by a client reload listener and keyed `namespace:path` as Java's
//! FileToIdConverter does (`jade:dark`, `jade:dark/slim`). The `overlay_theme` setting keeps the main id (`dark`, or
//! `mypack:fancy` outside Jade's namespace) and `theme_style` the part after the slash (`default` for none).
use crate::gfx::{Gfx, fade};
use crate::opts::{self, JsonRef};
use mod_api::serde_json::{self, Value};
use mod_api::*;
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// ColorPalette: ARGB.
#[derive(Clone, Copy, PartialEq)]
pub struct Palette {
    pub normal: u32,
    pub info: u32,
    pub title: u32,
    pub success: u32,
    pub warning: u32,
    pub danger: u32,
    pub failure: u32,
}

impl Palette {
    /// ColorPalette.DEFAULT.
    pub const DEFAULT: Palette = Palette { normal: 0xFFA0_A0A0, info: 0xFFFF_FFFF, title: 0xFFFF_FFFF, success: 0xFF55_FF55, warning: 0xFFFF_C107, danger: 0xFFFF_5555, failure: 0xFFAA_0000 };

    fn parse(v: JsonRef<'_>) -> Palette {
        let v = v.0;
        let d = Palette::DEFAULT;
        let Some(v) = v else { return d };
        let c = |name: &str, or: u32| v.get(name).and_then(|x| x.as_str()).and_then(color).unwrap_or(or);
        Palette { normal: c("normal", d.normal), info: c("info", d.info), title: c("title", d.title), success: c("success", d.success), warning: c("warning", d.warning), danger: c("danger", d.danger), failure: c("failure", d.failure) }
    }
}

/// `Color.split`: what's between the brackets, by commas or spaces (`/` is a space).
fn color_parts(s: &str) -> Option<Vec<String>> {
    let s = s.replace('/', " ");
    let inner = s.get(s.find('(')? + 1..s.find(')')?)?;
    let parts: Vec<&str> = if inner.contains(',') { inner.split(',').collect() } else { inner.split_whitespace().collect() };
    Some(parts.into_iter().map(|p| p.trim().to_owned()).collect())
}

/// `Color.parseDouble`: a number, or a percentage of `limit`.
fn color_number(p: &str, limit: f64) -> Option<f64> {
    match p.trim().strip_suffix('%') {
        Some(pct) => Some(pct.trim().parse::<f64>().ok()? * (limit / 100.0)),
        None => p.trim().parse().ok(),
    }
}

/// `Color.toDegrees`.
fn color_degrees(p: &str) -> Option<f64> {
    let p = p.trim();
    if let Some(v) = p.strip_suffix("deg") {
        v.trim().parse().ok()
    } else if let Some(v) = p.strip_suffix("grad") {
        Some(v.trim().parse::<f64>().ok()? / 400.0 * 360.0)
    } else if let Some(v) = p.strip_suffix("rad") {
        Some(v.trim().parse::<f64>().ok()? * (180.0 / std::f64::consts::PI))
    } else if let Some(v) = p.strip_suffix("turn") {
        Some(v.trim().parse::<f64>().ok()? * 360.0)
    } else {
        color_number(p, 360.0)
    }
}

/// `Color.hsl`, as ARGB.
fn hsl_color(hue: f64, saturation: f64, lightness: f64, opacity: f64) -> u32 {
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let h = hue / 60.0;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h {
        h if (0.0..1.0).contains(&h) => (c, x, 0.0),
        h if (1.0..2.0).contains(&h) => (x, c, 0.0),
        h if (2.0..3.0).contains(&h) => (0.0, c, x),
        h if (3.0..4.0).contains(&h) => (0.0, x, c),
        h if (4.0..5.0).contains(&h) => (x, 0.0, c),
        h if (5.0..6.0).contains(&h) => (c, 0.0, x),
        _ => (0.0, 0.0, 0.0),
    };
    let m = lightness - c / 2.0;
    let channel = |v: f64| (((v + m) * 255.0) as i32).clamp(0, 255) as u32;
    (((opacity.clamp(0.0, 1.0) * 255.0) as u32) << 24) | (channel(r) << 16) | (channel(g) << 8) | channel(b)
}

/// `Color.valueOf`, as ARGB: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa` (the `#` optional), `rgb(r, g, b[, a])`,
/// `hsl(h, s, l[, a])` and `transparent`.
fn color(s: &str) -> Option<u32> {
    let s = s.trim().to_lowercase();
    if s == "transparent" {
        return Some(0);
    }
    if s.contains("rgb") {
        let parts = color_parts(&s)?;
        let channel = |p: &String| -> Option<u32> { Some(color_number(p, 255.0)?.round().clamp(0.0, 255.0) as u32) };
        let (r, g, b) = (channel(parts.first()?)?, channel(parts.get(1)?)?, channel(parts.get(2)?)?);
        let a = parts.get(3).and_then(|p| color_number(p, 1.0)).map_or(255, |a| (a.clamp(0.0, 1.0) * 255.0) as u32);
        return Some((a << 24) | (r << 16) | (g << 8) | b);
    }
    if s.contains("hsl") {
        let parts = color_parts(&s)?;
        let hue = color_degrees(parts.first()?)?;
        let saturation = color_number(parts.get(1)?, 1.0)?;
        let lightness = color_number(parts.get(2)?, 1.0)?;
        let opacity = parts.get(3).and_then(|p| color_number(p, 1.0)).unwrap_or(1.0);
        return Some(hsl_color(hue, saturation, lightness, opacity));
    }
    let hex = s.strip_prefix('#').unwrap_or(&s);
    let full: String = if hex.len() == 3 || hex.len() == 4 { hex.chars().flat_map(|c| [c, c]).collect() } else { hex.to_owned() };
    if full.len() != 6 && full.len() != 8 {
        return None;
    }
    let v = u32::from_str_radix(&full, 16).ok()?;
    Some(if full.len() == 6 { 0xFF00_0000 | v } else { v.rotate_right(8) })
}

/// A chat style's parts, as much of it as the overlay's text can show.
#[derive(Clone, Copy, Default)]
pub(crate) struct StylePart {
    color: Option<u32>,
    bold: Option<bool>,
    italic: Option<bool>,
    underlined: Option<bool>,
    strikethrough: Option<bool>,
    obfuscated: Option<bool>,
}

/// `Style.Serializer`'s fields.
fn style_part(v: JsonRef<'_>) -> StylePart {
    let v = v.0;
    let Some(v) = v else { return StylePart::default() };
    let flag = |k: &str| v.get(k).and_then(|x| x.as_bool());
    let named = |n: &str| -> Option<u32> {
        Some(match n {
            "black" => 0x000000,
            "dark_blue" => 0x0000AA,
            "dark_green" => 0x00AA00,
            "dark_aqua" => 0x00AAAA,
            "dark_red" => 0xAA0000,
            "dark_purple" => 0xAA00AA,
            "gold" => 0xFFAA00,
            "gray" => 0xAAAAAA,
            "dark_gray" => 0x555555,
            "blue" => 0x5555FF,
            "green" => 0x55FF55,
            "aqua" => 0x55FFFF,
            "red" => 0xFF5555,
            "light_purple" => 0xFF55FF,
            "yellow" => 0xFFFF55,
            "white" => 0xFFFFFF,
            other => u32::from_str_radix(other.strip_prefix('#')?, 16).ok()?,
        })
    };
    StylePart {
        color: v.get("color").and_then(|x| x.as_str()).and_then(named),
        bold: flag("bold"),
        italic: flag("italic"),
        underlined: flag("underlined"),
        strikethrough: flag("strikethrough"),
        obfuscated: flag("obfuscated"),
    }
}

/// `ThemeHelper.modName`: the theme's modNameStyle over Formatting's item mod name style (blue italic), as the codes
/// the overlay's text reads.
fn mod_name_code(theme: StylePart) -> String {
    let mut out = match theme.color {
        Some(c) => format!("§#{:06X}", c & 0xFF_FFFF),
        None => "§9".to_owned(),
    };
    if theme.italic.unwrap_or(true) {
        out.push_str("§o");
    }
    if theme.bold.unwrap_or(false) {
        out.push_str("§l");
    }
    if theme.underlined.unwrap_or(false) {
        out.push_str("§n");
    }
    if theme.strikethrough.unwrap_or(false) {
        out.push_str("§m");
    }
    if theme.obfuscated.unwrap_or(false) {
        out.push_str("§k");
    }
    out
}

/// SneakyDetails.Simple: the arrow under a Lite box that has more to show.
#[derive(Clone)]
pub(crate) struct Sneaky {
    sprite: String,
    width: f32,
    height: f32,
    offset: [f32; 2],
    breath: bool,
    distance: f32,
    period: f32,
}

impl Sneaky {
    /// `SneakyDetails.CODEC`; `SneakyDetails.DEFAULT` without one (or for a type Java refuses too).
    fn parse(v: JsonRef<'_>) -> Sneaky {
        let v = v.0;
        let default = Sneaky { sprite: "jade:details_arrow".to_owned(), width: 7.0, height: 5.0, offset: [0.0; 2], breath: true, distance: 1.0, period: 12.0 };
        let Some(v) = v.filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("simple")) else { return default };
        let (Some(sprite), Some(width), Some(height)) = (v.get("sprite").and_then(|x| x.as_str()), v.get("width").and_then(|x| x.as_f64()), v.get("height").and_then(|x| x.as_f64())) else {
            return default;
        };
        let float = |k: &str, or: f64| v.get(k).and_then(|x| x.as_f64()).unwrap_or(or) as f32;
        Sneaky {
            sprite: if sprite.contains(':') { sprite.to_owned() } else { format!("minecraft:{sprite}") },
            width: width as f32,
            height: height as f32,
            offset: [float("offsetX", 0.0), float("offsetY", 0.0)],
            breath: v.get("animation").and_then(|x| x.as_str()) == Some("breath"),
            distance: float("animationDistance", 1.0),
            period: float("animationPeriod", 12.0),
        }
    }
}

/// details_arrow.png, 7 by 5, as (row, from, to, ARGB) pixel runs.
const DETAILS_ARROW: [(u8, u8, u8, u32); 13] = [
    (0, 0, 7, 0xFF66_6666),
    (1, 0, 1, 0xFF66_6666),
    (1, 1, 2, 0xFFD9_D9D9),
    (1, 2, 5, 0xFFFF_FFFF),
    (1, 5, 6, 0xFFD9_D9D9),
    (1, 6, 7, 0xFF66_6666),
    (2, 1, 2, 0xFF66_6666),
    (2, 2, 3, 0xFFD9_D9D9),
    (2, 3, 4, 0xFFFF_FFFF),
    (2, 4, 5, 0xFFD9_D9D9),
    (2, 5, 6, 0xFF66_6666),
    (3, 2, 5, 0xFF66_6666),
    (4, 3, 4, 0xFF66_6666),
];


/// A sprite a theme names, by its id (`jade:dark`) and as the GUI atlas names it.
#[derive(Clone)]
pub(crate) struct Sprite {
    id: String,
    atlas: String,
}

impl Sprite {
    fn new(id: &str) -> Sprite {
        Sprite { id: id.to_owned(), atlas: atlas_name(id) }
    }
}

/// BoxStyle.
#[derive(Clone)]
pub(crate) struct BoxStyle {
    /// `sprite`, with `tooltip`'s `_background` and `_frame` where that is set.
    plain: Option<(Sprite, Option<Sprite>)>,
    with_icon: Option<(Sprite, Option<Sprite>)>,
    tooltip: bool,
    /// ScreenDirection's order: up, down, left, right.
    padding: [f32; 4],
    progress: Palette,
    progress_offset: [f32; 4],
}

impl BoxStyle {
    fn parse(v: JsonRef<'_>) -> BoxStyle {
        let Some(v) = v.0 else { return BoxStyle::parse(JsonRef(Some(&Value::Null))) };
        let tooltip = v.get("tooltip").and_then(|x| x.as_bool()).unwrap_or(false);
        let sprites = |key: &str| -> Option<(Sprite, Option<Sprite>)> {
            let id = v.get(key)?.as_str()?;
            let id = if id.contains(':') { id.to_owned() } else { format!("minecraft:{id}") };
            if tooltip {
                // TooltipRenderUtil.getBackgroundSprite and getFrameSprite.
                let (ns, path) = id.split_once(':')?;
                let mut back = Sprite::new(&format!("{ns}:tooltip/{path}_background"));
                back.id = id.clone();
                Some((back, Some(Sprite::new(&format!("{ns}:tooltip/{path}_frame")))))
            } else {
                Some((Sprite::new(&id), None))
            }
        };
        let floats = |key: &str, or: [f32; 4]| -> [f32; 4] {
            let Some(a) = v.get(key).and_then(|x| x.as_array()) else { return or };
            let mut out = or;
            for (slot, x) in out.iter_mut().zip(a) {
                *slot = x.as_f64().unwrap_or(0.0) as f32;
            }
            out
        };
        BoxStyle {
            plain: sprites("sprite"),
            with_icon: sprites("withIconSprite"),
            tooltip,
            padding: floats("padding", [3.0; 4]),
            progress: Palette::parse(JsonRef(v.get("boxProgressColors"))),
            progress_offset: floats("boxProgressOffset", [0.0; 4]),
        }
    }
}

/// How the overlay's text looks with this theme (TextSetting).
pub struct TextLook {
    /// ColorPalette.normal, for text with no colour of its own (ARGB).
    pub base: u32,
    /// TextSetting.shadow.
    pub shadow: bool,
    /// TextSetting.itemAmountColor (ARGB).
    pub amount: u32,
    palette: Palette,
}

impl TextLook {
    /// A line's text with the colours Jade's providers ask for by message type (IThemeHelper.info, success, ...) as
    /// the theme has them. With the default palette they stay.
    pub fn map<'a>(&self, s: &'a str) -> Cow<'a, str> {
        let p = self.palette;
        if p == Palette::DEFAULT || !s.contains('§') {
            return Cow::Borrowed(s);
        }
        let mut out = String::with_capacity(s.len() + 16);
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '§' {
                out.push(c);
                continue;
            }
            let Some(code) = chars.next() else { break };
            let mapped = match code {
                '7' => Some(p.normal),
                'f' => Some(p.info),
                'a' => Some(p.success),
                '6' | 'e' => Some(p.warning),
                'c' => Some(p.danger),
                '4' => Some(p.failure),
                _ => None,
            };
            match mapped {
                Some(argb) => out.push_str(&format!("§#{:06X}", argb & 0xFF_FFFF)),
                None => {
                    out.push('§');
                    out.push(code);
                }
            }
        }
        Cow::Owned(out)
    }
}

/// `Theme.modifyIcon`'s slot: the sprite and what stands between it and the icon: top, right, bottom, left.
pub struct IconSlot<'a> {
    sprite: &'a Sprite,
    pub padding: [f32; 4],
}

impl IconSlot<'_> {
    /// Draws the slot round an icon of `size` at `at`.
    pub fn draw(&self, g: &mut Gfx<'_>, at: [f32; 2], size: f32, alpha: f32) {
        let [top, right, bottom, left] = self.padding;
        g.nine(&self.sprite.atlas, [at[0] - left, at[1] - top, size + left + right, size + top + bottom], fade(0xFFFF_FFFF, alpha));
    }
}

/// Theme and its holder's autoEnable.
#[derive(Clone)]
pub struct Theme {
    /// `fullId`.
    pub id: String,
    /// `mainId`.
    pub main: String,
    /// `styleId`: empty for a theme's default style.
    pub style: String,
    pub style_name: String,
    tooltip_style: BoxStyle,
    pub colors: Palette,
    shadow: bool,
    /// The title's code (IThemeHelper.title).
    pub title_code: String,
    /// The mod name's codes (IThemeHelper.modName).
    pub mod_code: String,
    amount: u32,
    pub change_opacity: f32,
    auto_enable: bool,
    sneaky: Sneaky,
    icon_slot: Option<Sprite>,
    icon_slot_inflation: f32,
    /// `spriteMapping`: `namespace:path` to `namespace:path`.
    sprite_mapping: HashMap<String, String>,
}

/// A `namespace:path` sprite id as the GUI atlas names it.
fn atlas_name(id: &str) -> String {
    match id.split_once(':') {
        Some(("minecraft", path)) => format!("gui/sprites/{path}"),
        Some((ns, path)) => format!("{ns}:gui/sprites/{path}"),
        None => format!("gui/sprites/{id}"),
    }
}

impl Theme {
    fn parse(id: &str, text: &[u8]) -> Option<Theme> {
        let v: Value = serde_json::from_slice(text).ok()?;
        // ThemeHelper.allowedVersions.
        if !(200..=299).contains(&v.get("version")?.as_i64()?) {
            return None;
        }
        let (ns, path) = id.split_once(':')?;
        let (main, style) = match path.split_once('/') {
            Some((m, s)) => (format!("{ns}:{m}"), s.to_owned()),
            None => (id.to_owned(), String::new()),
        };
        let text_setting = v.get("text");
        let colors = Palette::parse(JsonRef(text_setting.and_then(|t| t.get("colors"))));
        let title = colors.title & 0xFF_FFFF;
        Some(Theme {
            id: id.to_owned(),
            main,
            style,
            style_name: v.get("styleName").and_then(|x| x.as_str()).unwrap_or("jade.default").to_owned(),
            tooltip_style: BoxStyle::parse(JsonRef(Some(v.get("tooltipStyle")?))),
            colors,
            shadow: text_setting.and_then(|t| t.get("shadow")).and_then(|x| x.as_bool()).unwrap_or(true),
            title_code: if title == 0xFF_FFFF { "§f".to_owned() } else { format!("§#{title:06X}") },
            mod_code: mod_name_code(style_part(JsonRef(text_setting.and_then(|t| t.get("modNameStyle"))))),
            amount: text_setting.and_then(|t| t.get("itemAmountColor")).and_then(|x| x.as_str()).and_then(color).unwrap_or(0xFFFF_FFFF),
            change_opacity: v.get("changeOpacity").and_then(|x| x.as_f64()).unwrap_or(0.0).clamp(0.0, 1.0) as f32,
            auto_enable: v.get("autoEnable").and_then(|x| x.as_bool()).unwrap_or(false),
            sneaky: Sneaky::parse(JsonRef(v.get("sneakyDetails"))),
            icon_slot: v.get("iconSlotSprite").and_then(|x| x.as_str()).map(|s| Sprite::new(&if s.contains(':') { s.to_owned() } else { format!("minecraft:{s}") })),
            icon_slot_inflation: v.get("iconSlotInflation").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
            sprite_mapping: v
                .get("spriteMapping")
                .and_then(|x| x.as_object())
                .map(|map| {
                    let full = |s: &str| if s.contains(':') { s.to_owned() } else { format!("minecraft:{s}") };
                    map.iter().filter_map(|(from, to)| Some((full(from), full(to.as_str()?)))).collect()
                })
                .unwrap_or_default(),
        })
    }

    pub fn text_look(&self) -> TextLook {
        TextLook { base: self.colors.normal, shadow: self.shadow, amount: self.amount, palette: self.colors }
    }

    /// BoxStyle's padding: up, down, left, right.
    pub fn padding(&self) -> [f32; 4] {
        self.tooltip_style.padding
    }

    /// `Theme.mapSprite`: the atlas name to draw for a sprite Jade asks for by `namespace:path`.
    pub fn sprite(&self, id: &str) -> String {
        match self.sprite_mapping.get(id) {
            Some(to) => atlas_name(to),
            None => atlas_name(id),
        }
    }

    /// `Theme.modifyIcon`: the slot behind the icon. None without one.
    pub fn icon_slot(&self) -> Option<IconSlot<'_>> {
        let sprite = self.icon_slot.as_ref()?;
        let i = self.icon_slot_inflation;
        Some(IconSlot { sprite, padding: [i; 4] })
    }

    /// `SneakyDetails.Simple.render`: the arrow at the foot of the box. Nothing for the part of its period it is away.
    pub fn draw_sneaky(&self, g: &mut Gfx<'_>, frame: [f32; 4], ticks: f32, opacity: f32) {
        let s = &self.sneaky;
        let x = frame[0] + frame[2] / 2.0 - s.width / 2.0 + s.offset[0];
        let mut y = frame[1] + frame[3] - s.height / 2.0 + s.offset[1];
        let mut alpha = 1.0;
        if s.breath {
            let breath = (ticks / 5.0) % s.period - 2.0;
            if breath > 4.0 {
                return;
            }
            alpha = 1.0 - breath.abs() / 2.0;
            y += s.distance * breath;
            if alpha < 0.016 {
                return;
            }
        }
        let alpha = (alpha * opacity).clamp(0.0, 1.0);
        let name = self.sprite(&s.sprite);
        if !g.sprite(&name, [0.0, 0.0, s.width, s.height], [x, y, s.width, s.height], fade(0xFFFF_FFFF, alpha)) && s.sprite == "jade:details_arrow" {
            for (row, from, to, argb) in DETAILS_ARROW {
                let (from, to, row) = (f32::from(from), f32::from(to), f32::from(row));
                g.fill([x + from, y + row, to - from, 1.0], fade(argb, alpha));
            }
        }
    }
}

/// The colours of Jade's own box sprites, from the jar's PNGs: the fill, the border's top and bottom (the sides go
/// from one to the other), its width, and whether a one pixel ring of the fill goes round it (the
/// `tooltip/*_background` sprites, drawn with the corners cut; they stand one pixel outside the box). Drawn as fills
/// so a translucent box looks as the sprite does, each pixel once.
pub(crate) struct Look {
    fill: u32,
    top: u32,
    bottom: u32,
    border: f32,
    ring: bool,
}

fn builtin_look(id: &str) -> Option<Look> {
    let (fill, top, bottom, border, ring) = match id {
        "jade:dark" => (0xFF13_1313, 0xFF38_3838, 0xFF24_2424, 1.0, true),
        "jade:waila" => (0xFF10_0010, 0xFF50_00FF, 0xFF29_0080, 1.0, true),
        "jade:create" => (0xFF00_0000, 0xFF40_3C37, 0xFF20_1E1C, 1.0, true),
        "jade:dark_slim" => (0xFF13_1313, 0xFF38_3838, 0xFF24_2424, 1.0, false),
        "jade:waila_slim" => (0xFF10_0010, 0xFF50_00FF, 0xFF29_0080, 1.0, false),
        "jade:create_slim" => (0xFF00_0000, 0xFF40_3C37, 0xFF20_1E1C, 1.0, false),
        "jade:top" => (0x8000_6699, 0xFF99_99AD, 0xFF99_99AD, 2.0, false),
        _ => return None,
    };
    Some(Look { fill, top, bottom, border, ring })
}

fn draw_look(g: &mut Gfx<'_>, look: &Look, [x, y, w, h]: [f32; 4], alpha: f32) {
    let (mut x, mut y, mut w, mut h) = (x, y, w, h);
    let fill = fade(look.fill, alpha);
    if look.ring {
        (x, y, w, h) = (x - 1.0, y - 1.0, w + 2.0, h + 2.0);
        g.fill([x + 1.0, y, w - 2.0, 1.0], fill);
        g.fill([x + 1.0, y + h - 1.0, w - 2.0, 1.0], fill);
        g.fill([x, y + 1.0, 1.0, h - 2.0], fill);
        g.fill([x + w - 1.0, y + 1.0, 1.0, h - 2.0], fill);
        (x, y, w, h) = (x + 1.0, y + 1.0, w - 2.0, h - 2.0);
    }
    let b = look.border;
    let (top, bottom) = (fade(look.top, alpha), fade(look.bottom, alpha));
    g.fill([x, y, w, b], top);
    g.fill([x, y + h - b, w, b], bottom);
    g.gradient([x, y + b, b, h - 2.0 * b], top, bottom);
    g.gradient([x + w - b, y + b, b, h - 2.0 * b], top, bottom);
    g.fill([x + b, y + b, w - 2.0 * b, h - 2.0 * b], fill);
}

/// `BoxStyle.render`: the theme's box behind `rect` (with its padding), `alpha` being the overlay's Background
/// Opacity. Jade's own sprites draw as their colours (`builtin_look`); a pack theme's from the atlas, nine-sliced.
pub fn draw_background(g: &mut Gfx<'_>, theme: &Theme, rect: [f32; 4], has_icon: bool, alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let style = &theme.tooltip_style;
    let chosen = if has_icon { style.with_icon.as_ref().or(style.plain.as_ref()) } else { style.plain.as_ref() };
    let Some((back, frame)) = chosen else { return };
    let [x, y, w, h] = rect.map(f32::round);
    if let Some(look) = builtin_look(&back.id) {
        draw_look(g, &look, [x, y, w, h], alpha.min(1.0));
        return;
    }
    let outer = if style.tooltip { [x - 9.0, y - 9.0, w + 18.0, h + 18.0] } else { [x, y, w, h] };
    let white = fade(0xFFFF_FFFF, alpha.min(1.0));
    g.nine(&back.atlas, outer, white);
    if let Some(frame) = frame {
        g.nine(&frame.atlas, outer, white);
    }
}

/// `BoxElementImpl.drawBoxProgressBar`: along the box's foot, in the style's colour for TITLE or FAILURE.
pub fn draw_progress(g: &mut Gfx<'_>, theme: &Theme, [x, y, w, h]: [f32; 4], progress: f32, failed: bool, alpha: f32) {
    let style = &theme.tooltip_style;
    let base = if failed { style.progress.failure } else { style.progress.title };
    let [up, down, left, right] = style.progress_offset;
    let bottom = y + h;
    let width = (w + right - left) * progress.clamp(0.0, 1.0);
    g.fill([x + left, bottom - 1.0 + up, width, 1.0 + down - up], fade(base, alpha.clamp(0.0, 1.0)));
}

/// The themes, sorted as Java's `TreeMap<Identifier, Theme>` is (by path, then namespace): the jar's (this mod's
/// assets) and whatever resource packs add, read by the client reload listener `jade:themes`.
static LOADED: Mutex<Vec<Arc<Theme>>> = Mutex::new(Vec::new());
static RELOADS: AtomicU32 = AtomicU32::new(0);

fn loaded() -> Vec<Arc<Theme>> {
    let l = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    if l.is_empty() {
        drop(l);
        // Before the first reload: Jade's own, as the jar has them.
        return sorted(JAR_THEMES.iter().filter_map(|(id, text)| Theme::parse(id, text.as_bytes()).map(Arc::new)).collect());
    }
    l.clone()
}

fn sorted(mut themes: Vec<Arc<Theme>>) -> Vec<Arc<Theme>> {
    themes.sort_by_key(|t| {
        let (ns, path) = t.id.split_once(':').unwrap_or(("", t.id.as_str()));
        (path.to_owned(), ns.to_owned())
    });
    themes
}

/// assets/jade/jade_themes/ of Jade-mc26.3-Fabric-26.3.5.jar, for the moment before the resources load.
const JAR_THEMES: [(&str, &str); 7] = [
    ("jade:create", r#"{"version":200,"tooltipStyle":{"sprite":"jade:create","tooltip":true}}"#),
    ("jade:dark", r#"{"version":200,"tooltipStyle":{"sprite":"jade:dark","tooltip":true}}"#),
    ("jade:top", r#"{"version":200,"tooltipStyle":{"sprite":"jade:top","borderWidth":2,"padding":[5,4,5,4]}}"#),
    ("jade:waila", r#"{"version":200,"tooltipStyle":{"sprite":"jade:waila","tooltip":true}}"#),
    ("jade:create/slim", r#"{"version":200,"styleName":"jade.slim","tooltipStyle":{"sprite":"jade:create_slim"}}"#),
    ("jade:dark/slim", r#"{"version":200,"styleName":"jade.slim","tooltipStyle":{"sprite":"jade:dark_slim"}}"#),
    ("jade:waila/slim", r#"{"version":200,"styleName":"jade.slim","tooltipStyle":{"sprite":"jade:waila_slim"}}"#),
];

const DEFAULT_THEME: &str = "jade:dark";

/// ThemeHelper as a client reload listener on `jade_themes/*.json`.
pub struct Themes;

impl ClientReloadListener for Themes {
    fn prepare(&self, context: &ClientReloadContext) -> Result<Box<dyn FnOnce() + Send + 'static>, ReloadResourceError> {
        let mut themes = Vec::new();
        for id in context.list() {
            let name = id.to_string();
            if let Some(bytes) = context.read(id)
                && let Some(t) = Theme::parse(&name, bytes)
            {
                themes.push(Arc::new(t));
            }
        }
        let themes = sorted(themes);
        Ok(Box::new(move || {
            *LOADED.lock().unwrap_or_else(|e| e.into_inner()) = themes;
            RELOADS.fetch_add(1, Ordering::Relaxed);
        }))
    }
}

pub fn register(listeners: &mut dyn ClientReloadListeners) {
    let (Ok(id), Ok(converter)) = (Identifier::parse("jade:themes"), FileToIdConverter::json("jade_themes")) else { return };
    let _ = listeners.add_listener(id, converter, Arc::new(Themes));
}

/// A main id as the setting keeps it.
fn stored_main(main: &str) -> String {
    main.strip_prefix("jade:").unwrap_or(main).to_owned()
}

thread_local! {
    static ACTIVE: RefCell<Option<(u32, u32, Arc<Theme>)>> = const { RefCell::new(None) };
    static SEEN_AT: std::cell::Cell<u32> = const { std::cell::Cell::new(u32::MAX) };
}

/// ThemeHelper.apply's autoEnable: a theme new since the themes were last seen, that asks for it, turns on.
fn check_new(h: &dyn ClientHost) {
    let reloads = RELOADS.load(Ordering::Relaxed);
    if SEEN_AT.replace(reloads) == reloads {
        return;
    }
    let themes = loaded();
    let ids: Vec<&str> = themes.iter().map(|t| t.id.as_str()).collect();
    let joined = ids.join(",");
    let seen = opts::opt(h, "themes_seen");
    if seen != joined {
        let known: Vec<&str> = seen.split(',').collect();
        if let Some(new) = themes.iter().find(|t| t.auto_enable && !known.contains(&t.id.as_str())) {
            opts::set(h, "overlay_theme", &stored_main(&new.main));
            opts::set(h, "theme_style", if new.style.is_empty() { "default" } else { &new.style });
            if new.change_opacity != 0.0 {
                opts::set(h, "overlay_alpha", &format!("{}", (new.change_opacity * 100.0).round() as u32));
            }
        }
        opts::set(h, "themes_seen", &joined);
    }
}

/// `overlay.getTheme()`: the theme the settings name, or the same theme's default style, or Jade's default.
pub fn current(h: &dyn ClientHost) -> Arc<Theme> {
    check_new(h);
    let epoch = opts::settings_epoch();
    let reloads = RELOADS.load(Ordering::Relaxed);
    if let Some(t) = ACTIVE.with_borrow(|a| a.as_ref().filter(|(e, r, _)| *e == epoch && *r == reloads).map(|(_, _, t)| t.clone())) {
        return t;
    }
    let themes = loaded();
    let main = opts::opt(h, "overlay_theme");
    let main = if main.contains(':') { main } else { format!("jade:{main}") };
    let style = opts::opt(h, "theme_style");
    let full = if style == "default" || style.is_empty() { main.clone() } else { format!("{main}/{style}") };
    let find = |id: &str| themes.iter().find(|t| t.id == id).cloned();
    let theme = find(&full).or_else(|| find(&main)).or_else(|| find(DEFAULT_THEME)).or_else(|| themes.first().cloned());
    let theme = theme.unwrap_or_else(|| Arc::new(Theme::parse(DEFAULT_THEME, JAR_THEMES[1].1.as_bytes()).expect("Jade's dark theme parses")));
    ACTIVE.with_borrow_mut(|a| *a = Some((epoch, reloads, theme.clone())));
    theme
}

/// `jade.theme.<namespace>.<path>`.
fn theme_name(h: &dyn ClientHost, main: &str) -> String {
    let (ns, path) = main.split_once(':').unwrap_or(("jade", main));
    let fallback = {
        let mut c = path.chars();
        c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str())
    };
    opts::tr(h, &format!("jade.theme.{ns}.{path}"), &fallback)
}

/// The Theme button's label.
pub fn main_label(h: &dyn ClientHost) -> String {
    theme_name(h, &current(h).main)
}

/// How many styles the current theme has (one: the Theme Style button says Unavailable).
pub fn style_count(h: &dyn ClientHost) -> usize {
    let main = current(h).main.clone();
    loaded().iter().filter(|t| t.main == main).count()
}

/// The Theme Style button's label.
pub fn style_label(h: &dyn ClientHost) -> String {
    if style_count(h) <= 1 {
        return opts::tr(h, "jade.unavailable", "Unavailable");
    }
    let key = current(h).style_name.clone();
    let fallback = match key.as_str() {
        "jade.default" => "Default",
        "jade.slim" => "Slim",
        other => other,
    };
    opts::tr(h, &key, fallback)
}

/// The Theme button: the next theme's default style, with its changeOpacity.
pub fn cycle_main(h: &dyn ClientHost, back: bool) {
    let themes = loaded();
    let mains: Vec<&Arc<Theme>> = themes.iter().filter(|t| t.style.is_empty()).collect();
    if mains.is_empty() {
        return;
    }
    let now = current(h).main.clone();
    let at = mains.iter().position(|t| t.id == now).unwrap_or(0);
    let next = if back { mains[(at + mains.len() - 1) % mains.len()] } else { mains[(at + 1) % mains.len()] };
    opts::set(h, "overlay_theme", &stored_main(&next.main));
    opts::set(h, "theme_style", "default");
    if next.change_opacity != 0.0 {
        opts::set(h, "overlay_alpha", &format!("{}", (next.change_opacity * 100.0).round() as u32));
    }
}

/// The Theme Style button: the current theme's next style.
pub fn cycle_style(h: &dyn ClientHost, back: bool) {
    let themes = loaded();
    let theme = current(h);
    let styles: Vec<&Arc<Theme>> = themes.iter().filter(|t| t.main == theme.main).collect();
    if styles.len() <= 1 {
        return;
    }
    let at = styles.iter().position(|t| t.id == theme.id).unwrap_or(0);
    let next = if back { styles[(at + styles.len() - 1) % styles.len()] } else { styles[(at + 1) % styles.len()] };
    opts::set(h, "theme_style", if next.style.is_empty() { "default" } else { &next.style });
}
