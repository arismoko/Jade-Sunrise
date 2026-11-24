//! The overlay (OverlayRenderer, BoxElementImpl, WailaTickHandler) on the client: what the player looks at, the box's
//! lines from Jade's client providers, the plugins' and the server's, and the box drawn in the HUD layer `jade:overlay`
//! with the theme's frame.
//!
//! The client reads blocks itself (state, block entity data) and tracked entities' position, rotation, eye position,
//! tick count and riding links (`vehicle`/`passengers`). `Host::living` on a client resolves only the local player, and
//! `entity_kind`, `entity_nbt`, `entity_pose`, `entity_leash_holder`, `display`, `item_entity_stack` and `merchant` are
//! server-only, so an entity's kind, name, icon and server lines come back from the server (`server.rs`), asked for a
//! few times a second while the player looks.
use crate::api::{self, El, Line, Target, TR_CLOSE, TR_OPEN, TR_SEP};
use crate::gfx::{self, Gfx};
use crate::opts::{self, fill, num, on, opt, plain, tr, trf, trim2};
use crate::theme;
use mod_api::prelude::*;
use mod_api::*;
use std::cell::RefCell;
use std::time::Instant;

pub const NET_REQUEST: &str = "jade:request";
pub const NET_DATA: &str = "jade:data";
pub const LAYER: &str = "jade:overlay";

/// What the client sees under the crosshair this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Seen {
    /// The block (what it passes for), its real id, state string, where the ray meets it and how far that is.
    Block { pos: [i32; 3], block: String, real: String, state: String, hit: [f64; 3], dist: f64, face: [i32; 3] },
    /// An entity on the crosshair (or the server's pick), where the ray meets it and how far.
    Entity { id: u32, point: [f64; 3], dist: f64 },
}

/// An entity as the server described it.
#[derive(Clone, Debug)]
pub struct EntityData {
    pub id: u32,
    pub kind: String,
    pub name: String,
    pub icon: String,
    pub lines: Vec<String>,
    pub picked: bool,
    pub dist: f64,
    pub at: Instant,
}

#[derive(Default)]
pub struct Client {
    pub fade: f32,
    pub fade_at: Option<Instant>,
    /// The overlay was drawn last frame (Boss Bar Overlap: Hide Boss Bar).
    pub showing: bool,
    /// `/jadec pin`: this target stays on the box.
    pub pinned: Option<Seen>,
    pub pin_requested: bool,
    /// What draw saw, for the tick's request.
    pub seen: Option<Seen>,
    /// The overlay is up (not hidden), so the server is asked.
    pub active: bool,
    pub block_data: Option<([i32; 3], Vec<String>, Instant)>,
    pub entity_data: Option<EntityData>,
    pub asked: Option<(String, Instant)>,
    /// Show Overlay toggled off; Toggle Fluid on.
    pub hidden: bool,
    pub liquids: bool,
    pub keys_down: Vec<&'static str>,
    pub details: bool,
    pub progress_alpha: f32,
    pub narrate: bool,
    pub tts_on: bool,
    pub lookup: Option<bool>,
    pub open_config: bool,
    pub use_profile: Option<u8>,
    pub hint: Option<u8>,
    pub powder_at: Option<Instant>,
    /// The settings epoch last sent to the server.
    pub sent_epoch: u32,
    /// Boss bars drawn this frame, and the last frame's.
    pub bars_now: u32,
    pub bars: u32,
    /// The box built this frame for the config screens' preview.
    pub built: Option<BuiltBox>,
    /// Narrate Key Binds' line, said at the next tick.
    pub say: Option<String>,
    /// Narrate Target's line (the box drawn), said at the next tick: the draw has no mutable host.
    pub speak: Option<String>,
    /// The last box Narrate Target said (WailaTickHandler's dedupe in Toggle mode).
    pub said: String,
}

thread_local! {
    pub static CLIENT: RefCell<Client> = RefCell::new(Client::default());
    static STARTED: Instant = Instant::now();
}

pub fn with<R>(f: impl FnOnce(&mut Client) -> R) -> R {
    CLIENT.with_borrow_mut(f)
}

/// A translatable marker's text in this client's language (`api::tr`), with its arguments resolved first.
pub fn resolve(h: &dyn ClientHost, s: &str) -> String {
    if !s.contains(TR_OPEN) {
        return s.to_owned();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = String::new();
    while i < chars.len() {
        if chars[i] == TR_OPEN {
            out.push_str(&marker(h, &chars, &mut i, 0));
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn marker(h: &dyn ClientHost, c: &[char], i: &mut usize, depth: u8) -> String {
    *i += 1;
    let mut fields = Vec::new();
    let mut cur = String::new();
    while *i < c.len() {
        match c[*i] {
            TR_OPEN if depth < 8 => {
                let inner = marker(h, c, i, depth + 1);
                cur.push_str(&inner);
                continue;
            }
            TR_SEP => fields.push(std::mem::take(&mut cur)),
            TR_CLOSE => {
                *i += 1;
                break;
            }
            ch => cur.push(ch),
        }
        *i += 1;
    }
    fields.push(cur);
    let key = fields.first().cloned().unwrap_or_default();
    let fallback = fields.get(1).cloned().unwrap_or_default();
    let args: Vec<&str> = fields.iter().skip(2).map(String::as_str).collect();
    match key.as_str() {
        "@item" => item_name(h, args.first().copied().unwrap_or(&fallback)),
        "@entity" => entity_name(h, args.first().copied().unwrap_or(&fallback)),
        _ => fill(&tr(h, &key, &fallback), &args),
    }
}

/// An item's name: `item.<ns>.<path>`, else `block.<ns>.<path>`, else its path in words.
pub fn item_name(h: &dyn ClientHost, id: &str) -> String {
    let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
    h.translate(&format!("item.{ns}.{path}")).or_else(|| h.translate(&format!("block.{ns}.{path}"))).unwrap_or_else(|| words(path))
}

/// A block's name as Jade titles it: its item's (`pick` result), else its own.
pub fn block_name(h: &dyn ClientHost, block: &str) -> String {
    let (ns, path) = block.split_once(':').unwrap_or(("minecraft", block));
    if let Some(item) = block_item(h, block) {
        return item_name(h, &item);
    }
    h.translate(&format!("block.{ns}.{path}")).unwrap_or_else(|| words(path))
}

pub fn entity_name(h: &dyn ClientHost, id: &str) -> String {
    let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id));
    h.translate(&format!("entity.{ns}.{path}")).unwrap_or_else(|| words(path))
}

fn words(path: &str) -> String {
    path.split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A block's key by its id.
pub fn bkey(h: &dyn Host, id: &str) -> Option<BlockKey> {
    h.block_state_id(id).map(|s| s.block())
}

/// The block's item id (`Block.asItem`), when it has one.
pub fn block_item(h: &dyn ClientHost, block: &str) -> Option<String> {
    let key = bkey(h, block)?;
    let item = h.block_item(key)?;
    let id = item.id(h).unwrap_or_default().to_owned();
    (id != "minecraft:air").then_some(id)
}

/// A mod id's name as Jade shows it under the box (ModIdentification.getModName: with Translate Mod Name on, the
/// language's `jade.modName.<id>` or `itemGroup.<id>`, then what a mod registered, then the id in words).
pub fn mod_name(h: &dyn ClientHost, ns: &str) -> String {
    if on(h, "mod_name_translated") {
        for key in [format!("jade.modName.{ns}"), format!("itemGroup.{ns}")] {
            if let Some(t) = h.translate(&key).filter(|t| !t.is_empty()) {
                return t;
            }
        }
    }
    let registered = api::registry(|r| r.mod_names.iter().find(|(n, _)| *n == ns).map(|(_, name)| (*name).to_owned()));
    if let Some(name) = registered {
        return name;
    }
    match ns {
        "minecraft" => "Minecraft".to_owned(),
        "jade" => "Jade".to_owned(),
        "ae2" => "Applied Energistics 2".to_owned(),
        "ironfurnaces" => "Iron Furnaces".to_owned(),
        "powah" => "Powah".to_owned(),
        "apothic_spawners" => "Apothic Spawners".to_owned(),
        "hostilenetworks" => "Hostile Neural Networks".to_owned(),
        "enderio" => "Ender IO".to_owned(),
        "tconstruct" => "Tinkers' Construct".to_owned(),
        "elevatorid" => "Elevator Mod".to_owned(),
        other => words(other),
    }
}

/// A block state string's properties.
pub fn props(state: &str) -> Vec<(String, String)> {
    let Some(inner) = state.split_once('[').and_then(|(_, r)| r.strip_suffix(']')) else { return Vec::new() };
    inner.split(',').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_owned(), v.to_owned())).collect()
}

pub fn prop(state: &str, key: &str) -> Option<String> {
    props(state).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// The level this client shows.
pub fn level(h: &dyn ClientHost) -> Option<LevelRef> {
    h.level(h.dimension())
}

fn state_at(h: &dyn ClientHost, level: LevelRef, pos: [i32; 3]) -> Option<(String, String)> {
    let state = level.block_state(h, pos)?;
    let s = h.block_state_string(state)?;
    let id = s.split('[').next().unwrap_or(&s).to_owned();
    Some((id, s))
}

/// The local player, as a living entity.
pub fn me(h: &dyn ClientHost) -> Option<(PlayerRef, Option<LivingRef>)> {
    let p = h.local_player()?;
    Some((p, h.living(p.entity())))
}

/// The eye (the camera) and the view's direction.
pub fn aim(h: &dyn ClientHost) -> Option<([f64; 3], [f64; 3])> {
    let c = h.camera()?;
    let (yaw, pitch) = (f64::from(c.yaw).to_radians(), f64::from(c.pitch).to_radians());
    let dir = [-yaw.sin() * pitch.cos(), -pitch.sin(), yaw.cos() * pitch.cos()];
    Some((c.position, dir))
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt()
}

/// The first fluid along the view (ClipContext.Fluid.ANY): its distance, position and state. None with the eye in a
/// fluid when it must come from outside (RayTracing's check of the eye's block).
fn fluid_hit(h: &dyn ClientHost, reach: f64, from_outside: bool) -> Option<(f64, [i32; 3], String, String)> {
    let (eye, dir) = aim(h)?;
    let level = level(h)?;
    let fluid_at = |p: [i32; 3]| -> Option<(String, String)> {
        let (id, s) = state_at(h, level, p)?;
        let f = match id.as_str() {
            "minecraft:water" | "minecraft:lava" => id.clone(),
            _ if s.contains("waterlogged=true") => "minecraft:water".to_owned(),
            _ => return None,
        };
        Some((f, s))
    };
    let mut pos = eye.map(|v| v.floor() as i32);
    if from_outside && fluid_at(pos).is_some() {
        return None;
    }
    let step = dir.map(|d| if d > 0.0 { 1 } else { -1 });
    let delta = dir.map(|d| if d == 0.0 { f64::INFINITY } else { (1.0 / d).abs() });
    let mut t_max = [0.0; 3];
    for a in 0..3 {
        let edge = f64::from(pos[a]) + if step[a] > 0 { 1.0 } else { 0.0 };
        t_max[a] = if dir[a] == 0.0 { f64::INFINITY } else { (edge - eye[a]) / dir[a] };
    }
    for _ in 0..512 {
        let a = if t_max[0] <= t_max[1] && t_max[0] <= t_max[2] { 0 } else if t_max[1] <= t_max[2] { 1 } else { 2 };
        let t = t_max[a];
        if t > reach {
            return None;
        }
        pos[a] += step[a];
        t_max[a] += delta[a];
        if let Some((name, s)) = fluid_at(pos) {
            return Some((t, pos, name, s));
        }
    }
    None
}

fn ignored_block(h: &dyn ClientHost, block: &str) -> bool {
    let (blocks, _) = opts::ignore_lists(h);
    if blocks.iter().any(|b| b == block) {
        return true;
    }
    let Some(key) = bkey(h, block) else { return false };
    blocks.iter().filter_map(|t| t.strip_prefix('#')).any(|tag| h.block_in_tag(key, tag) == Some(true))
}

pub fn ignored_entity(h: &dyn ClientHost, kind: &str) -> bool {
    opts::ignore_lists(h).1.iter().any(|e| e == kind)
}

fn creative(h: &dyn ClientHost) -> bool {
    me(h).is_some_and(|(p, _)| p.is_creative(h) == Some(true) || p.is_spectator(h) == Some(true))
}

/// JadeClient.builtInOverrides: what a block passes for: a trapped chest as its chest, an infested block as its host,
/// powder snow as snow (unless the player was in some these 8 seconds), a suspicious block as what it turns into. Not
/// in creative or spectator, nor with the narrator on.
fn camouflage(h: &dyn ClientHost, block: &str) -> Option<String> {
    if creative(h) || !on(h, "builtin_camouflage") || with(|c| c.tts_on) {
        return None;
    }
    let (ns, path) = block.split_once(':')?;
    let host_block = if let Some(rest) = path.strip_prefix("trapped_") {
        format!("{ns}:{rest}")
    } else if let Some(rest) = path.strip_suffix("_trapped_chest") {
        format!("{ns}:{rest}_chest")
    } else if let Some(rest) = path.strip_prefix("infested_") {
        format!("{ns}:{rest}")
    } else if let Some(rest) = path.strip_prefix("suspicious_") {
        format!("{ns}:{rest}")
    } else if block == "minecraft:powder_snow" && with(|c| c.powder_at.is_none_or(|at| at.elapsed().as_millis() > 8000)) {
        "minecraft:snow_block".to_owned()
    } else {
        return None;
    };
    bkey(h, &host_block).map(|_| host_block)
}

/// What the player looks at: the nearer of the block (or fluid) and the entity under the crosshair.
pub fn look(h: &dyn ClientHost) -> Option<Seen> {
    if !on(h, "display_tooltip") {
        return None;
    }
    let (eye, dir) = aim(h)?;
    let level = level(h)?;
    let extra = num(h, "reach_distance").clamp(0.0, 20.0);
    let reach = me(h).and_then(|(_, l)| l?.attribute_value(h, "minecraft:block_interaction_range")).unwrap_or(4.5);
    let mut block: Option<Seen> = None;
    let mut entity: Option<Seen> = None;
    match h.crosshair_target() {
        Some(CrosshairTarget::Block { pos, state, hit }) if extra <= 0.0 => {
            let p = [pos.x, pos.y, pos.z];
            if let Some(s) = h.block_state_string(state) {
                let id = s.split('[').next().unwrap_or(&s).to_owned();
                block = Some(Seen::Block { pos: p, block: id.clone(), real: id, state: s, hit: hit.point, dist: dist(eye, hit.point), face: hit.face });
            }
        }
        Some(CrosshairTarget::Entity { entity: e, point }) => {
            entity = Some(Seen::Entity { id: e.id(), point, dist: dist(eye, point) });
        }
        _ => {}
    }
    // Extended Reach: the block along the view past the hand's reach.
    if extra > 0.0
        && let Some(hit) = h.raycast((reach + extra) as f32)
    {
        let p = [hit.block_x, hit.block_y, hit.block_z];
        let point = [hit.x, hit.y, hit.z];
        if let Some((id, s)) = state_at(h, level, p) {
            let face = face_of(point, p);
            block = Some(Seen::Block { pos: p, block: id.clone(), real: id, state: s, hit: point, dist: dist(eye, point), face });
        }
    }
    let blocks_on = on(h, "display_blocks");
    let block = block.filter(|b| match b {
        Seen::Block { block, .. } => blocks_on && !block.ends_with(":air") && !ignored_block(h, block),
        _ => false,
    });
    let entity = entity.filter(|_| on(h, "display_entities"));
    // RayTracing.rayTrace: fluids from outside them with Any (or Toggle Fluid), with Fallback only with no block.
    let mode = opt(h, "display_fluids");
    let any = mode == "any" || with(|c| c.liquids);
    let fluid = if blocks_on && (any || (mode == "fallback" && block.is_none())) { fluid_hit(h, reach + extra, any) } else { None };
    let bd = |b: &Option<Seen>| match b {
        Some(Seen::Block { dist, .. }) => *dist,
        _ => f64::INFINITY,
    };
    let block = match fluid {
        Some((fd, pos, name, s)) if fd < bd(&block) => {
            let point = [eye[0] + dir[0] * fd, eye[1] + dir[1] * fd, eye[2] + dir[2] * fd];
            Some(Seen::Block { pos, block: name.clone(), real: name, state: s, hit: point, dist: fd, face: [0, 1, 0] })
        }
        _ => block,
    };
    let chosen = match (block, entity) {
        (Some(b), Some(e)) => {
            let ed = match &e {
                Seen::Entity { dist, .. } => *dist,
                _ => f64::INFINITY,
            };
            if ed < bd(&Some(b.clone())) { Some(e) } else { Some(b) }
        }
        (b, e) => b.or(e),
    };
    Some(match chosen? {
        Seen::Block { pos, block, real, state, hit, dist, face } => {
            let shown = camouflage(h, &block).unwrap_or_else(|| block.clone());
            Seen::Block { pos, block: shown, real, state, hit, dist, face }
        }
        other => other,
    })
}

/// The face of a block a point on its surface is on.
fn face_of(point: [f64; 3], pos: [i32; 3]) -> [i32; 3] {
    let rel: Vec<f64> = (0..3).map(|i| point[i] - f64::from(pos[i])).collect();
    let mut best = ([0, 1, 0], f64::INFINITY);
    for a in 0..3 {
        for (v, n) in [(0.0, -1), (1.0, 1)] {
            let d = (rel[a] - v).abs();
            if d < best.1 {
                let mut f = [0; 3];
                f[a] = n;
                best = (f, d);
            }
        }
    }
    best.0
}

/// VanillaPlugin's harvest plugin: each ToolType's tiers, lowest first, and its skipInstaBreakingBlock.
const TOOL_TYPES: [(&[&str], bool); 6] = [
    (&["minecraft:wooden_pickaxe", "minecraft:golden_pickaxe", "minecraft:stone_pickaxe", "minecraft:copper_pickaxe", "minecraft:iron_pickaxe", "minecraft:diamond_pickaxe", "minecraft:netherite_pickaxe"], true),
    (&["minecraft:wooden_axe", "minecraft:golden_axe", "minecraft:stone_axe", "minecraft:copper_axe", "minecraft:iron_axe", "minecraft:diamond_axe", "minecraft:netherite_axe"], true),
    (&["minecraft:wooden_shovel", "minecraft:golden_shovel", "minecraft:stone_shovel", "minecraft:copper_shovel", "minecraft:iron_shovel", "minecraft:diamond_shovel", "minecraft:netherite_shovel"], true),
    (&["minecraft:wooden_hoe", "minecraft:golden_hoe", "minecraft:stone_hoe", "minecraft:copper_hoe", "minecraft:iron_hoe", "minecraft:diamond_hoe", "minecraft:netherite_hoe"], true),
    (&["minecraft:wooden_sword"], true),
    (&["minecraft:shears"], false),
];

/// A vanilla tool's Tool component rule as tags: the tier's `incorrect_for_<tier>_tool` denies, its kind's
/// `mineable/<kind>` mines and drops (a sword: cobweb; shears: what the server's loot tables ask shears of).
fn tool_correct(h: &dyn ClientHost, tool: &str, block: &str) -> bool {
    let Some(key) = bkey(h, block) else { return false };
    let path = tool.strip_prefix("minecraft:").unwrap_or(tool);
    let tag = |t: &str| h.block_in_tag(key, t) == Some(true);
    if path == "shears" {
        return crate::facts::shearable(block) || matches!(block, "minecraft:cobweb" | "minecraft:redstone_wire" | "minecraft:tripwire") || tag("minecraft:leaves") || tag("minecraft:wool");
    }
    let Some((tier, kind)) = path.rsplit_once('_') else { return false };
    if kind == "sword" {
        return block == "minecraft:cobweb";
    }
    tag(&format!("minecraft:mineable/{kind}")) && !tag(&format!("minecraft:incorrect_for_{tier}_tool"))
}

/// The tick the cache was filled on, and each block's harvest tools.
type HarvestCache = ((u32, u32), std::collections::HashMap<String, Vec<String>>);

thread_local! {
    static HARVEST: RefCell<HarvestCache> = RefCell::new(((u32::MAX, u32::MAX), Default::default()));
}

/// HarvestToolProvider.getTool, cached per block: the lowest tier of each tool type that suits it.
fn harvest_tools(h: &dyn ClientHost, block: &str, insta: bool) -> Vec<String> {
    let epoch = (opts::settings_epoch(), crate::facts::shears_generation());
    if let Some(t) = HARVEST.with_borrow_mut(|(at, cache)| {
        if *at != epoch {
            *at = epoch;
            cache.clear();
        }
        cache.get(block).cloned()
    }) {
        return t;
    }
    let mut tools = Vec::new();
    for (tiers, skip_insta) in TOOL_TYPES {
        if skip_insta && insta {
            continue;
        }
        if let Some(t) = tiers.iter().find(|t| tool_correct(h, t, block)) {
            tools.push((*t).to_owned());
        }
    }
    HARVEST.with_borrow_mut(|(_, cache)| cache.insert(block.to_owned(), tools.clone()));
    tools
}

/// The held item's correctness for a block's drops (`ItemStack.isCorrectToolForDrops`), as the game answers it for
/// the held stack: a mod's tool counts by its own `minecraft:tool` rules. Where the game has no answer, the vanilla
/// tools' rules as tags.
fn held_correct(h: &dyn ClientHost, block: &str) -> bool {
    let Some(held) = me(h).and_then(|(_, l)| l?.with_hand_item(h, Hand::Main, |s| s.project()).flatten()) else { return false };
    let state = bkey(h, block).and_then(|k| h.block_state_default(k));
    if let Some(correct) = state.and_then(|s| h.block_states().is_correct_tool_for_drops(&held, s)) {
        return correct;
    }
    tool_correct(h, held.item().id(h).unwrap_or_default(), block)
}

/// `BlockState.requiresCorrectToolForDrops()` as the game answers it (a mod's block too).
fn requires_tool(h: &dyn ClientHost, block: &str) -> bool {
    crate::facts::fact(h, block).is_some_and(|f| f.requires_tool)
}

/// HarvestToolProvider: the tools the block wants, with a tick or cross where the block needs the right one.
fn harvest_tool(h: &dyn ClientHost, block: &str) -> Option<Vec<El>> {
    if creative(h) && !on(h, "harvest_creative") {
        return None;
    }
    if me(h).is_some_and(|(p, _)| p.game_mode(h) == Some(GameMode::Adventure)) {
        return None;
    }
    let hardness = crate::facts::fact(h, block).map_or(0.0, |f| f.hardness);
    if hardness < 0.0 {
        return on(h, "harvest_unbreakable").then(|| vec![El::Text(format!("§c{}", tr(h, "jade.harvest_tool.unbreakable", "Unbreakable")))]);
    }
    let requires = requires_tool(h, block);
    if !requires && !on(h, "harvest_effective") {
        return None;
    }
    let tools = harvest_tools(h, block, !requires && hardness == 0.0);
    if tools.is_empty() {
        return None;
    }
    let mut els: Vec<El> = tools.iter().map(|t| Line::small(t)).collect();
    if requires {
        els.push(El::Mark(if held_correct(h, block) { "§a✔".to_owned() } else { "§c✕".to_owned() }));
    }
    Some(els)
}

fn access(h: &dyn ClientHost, key: &str) -> bool {
    on(h, "accessibility_plugin") && on(h, key)
}

fn block_entity_data(h: &dyn ClientHost, pos: [i32; 3]) -> Option<Compound> {
    let level = level(h)?;
    h.block_entity(level, pos)?.data(h)
}

fn tag_str(t: Option<&Tag>) -> Option<String> {
    match t? {
        Tag::String(s) => s.to_string().ok(),
        _ => None,
    }
}

fn cget<'a>(c: &'a Compound, key: &str) -> Option<&'a Tag> {
    c.iter().find(|(k, _)| k.to_string_lossy() == key).map(|(_, v)| v)
}

/// Jade's own client lines for a block (VanillaPlugin's client components): growth, redstone, note blocks, TNT,
/// beehives' honey, enchanting power, signs and block details.
fn vanilla_block(h: &dyn ClientHost, pos: [i32; 3], block: &str, real: &str, state: &str, details: bool, lines: &mut Vec<Line>) {
    let p = |k: &str| prop(state, k);
    // CropProgressProvider.
    if let Some(age) = p("age").and_then(|a| a.parse::<u32>().ok()).filter(|_| on(h, "crop_progress")) {
        let most = bkey(h, block).and_then(|k| h.block_states().definition(k)).and_then(|defs| defs.iter().find(|d| d.name == "age").map(|d| d.values.iter().filter_map(|v| v.parse::<u32>().ok()).max().unwrap_or(7))).unwrap_or(7).max(1);
        let crop = block.ends_with("_crop")
            || matches!(block, "minecraft:wheat" | "minecraft:carrots" | "minecraft:potatoes" | "minecraft:beetroots" | "minecraft:melon_stem" | "minecraft:pumpkin_stem" | "minecraft:sweet_berry_bush" | "minecraft:nether_wart" | "minecraft:cocoa" | "minecraft:torchflower_crop" | "minecraft:pitcher_crop");
        if crop {
            lines.push(Line::text(if age >= most { format!("§a{}", tr(h, "tooltip.jade.crop_mature", "Mature")) } else { trf(h, "tooltip.jade.crop_growth", "Growth: %s", &[&format!("§f{}%", age * 100 / most)]) }));
        }
    }
    // RedstoneProvider: a lever's state, a repeater's delay, a comparator's mode, redstone power.
    let redstone = on(h, "redstone");
    if redstone && block == "minecraft:lever" {
        let lit = p("powered").as_deref() == Some("true");
        let s = if lit { format!("§a{}", tr(h, "tooltip.jade.state_on", "On")) } else { format!("§c{}", tr(h, "tooltip.jade.state_off", "Off")) };
        lines.push(Line::text(trf(h, "tooltip.jade.state", "State: %s", &[&s])));
    }
    if redstone && block == "minecraft:repeater" {
        lines.push(Line::text(trf(h, "tooltip.jade.delay", "Delay: %d", &[&format!("§f{}", p("delay").unwrap_or_default())])));
    }
    if redstone && block == "minecraft:comparator" {
        let m = if p("mode").as_deref() == Some("subtract") { tr(h, "tooltip.jade.mode_subtractor", "Subtractor") } else { tr(h, "tooltip.jade.mode_comparator", "Comparator") };
        lines.push(Line::text(trf(h, "tooltip.jade.mode", "Mode: %s", &[&format!("§f{m}")])));
    }
    if let Some(power) = p("power").filter(|_| redstone)
        && !matches!(block, "minecraft:comparator" | "minecraft:lever" | "minecraft:repeater")
    {
        lines.push(Line::text(trf(h, "tooltip.jade.power", "Power: %d", &[&format!("§f{power}")])));
    }
    // NoteBlockProvider.
    if block == "minecraft:note_block" && on(h, "note_block") {
        let inst = p("instrument").unwrap_or_default();
        let fallback = match inst.as_str() {
            "basedrum" => "Bass Drum", "snare" => "Snare Drum", "hat" => "Clicks and Sticks", "bass" => "Bass", "flute" => "Flute", "bell" => "Bells",
            "guitar" => "Guitar", "chime" => "Chimes", "xylophone" => "Xylophone", "iron_xylophone" => "Vibraphone", "cow_bell" => "Cow Bell",
            "didgeridoo" => "Didgeridoo", "bit" => "Square Wave", "banjo" => "Banjo", "pling" => "Electric Piano", "zombie" => "Zombie",
            "skeleton" => "Skeleton", "creeper" => "Creeper", "dragon" => "Dragon", "wither_skeleton" => "Wither Skeleton", "piglin" => "Piglin",
            "custom_head" => "Custom Head", _ => "Harp",
        };
        let name = tr(h, &format!("jade.instrument.{inst}"), fallback);
        let note: usize = p("note").and_then(|n| n.parse().ok()).unwrap_or(0);
        const NOTES: [&str; 12] = ["F♯", "G", "G♯", "A", "A♯", "B", "C", "C♯", "D", "D♯", "E", "F"];
        let octave = if note < 6 { 1 } else if note < 18 { 2 } else { 3 };
        lines.push(Line::text(format!("§f{name} - {}{octave}", NOTES[note % 12])));
    }
    // TNTStabilityProvider.
    if block == "minecraft:tnt" && p("unstable").as_deref() == Some("true") && on(h, "tnt_stability") {
        lines.push(Line::text(format!("§c{}", tr(h, "jade.tnt.unstable", "Unstable"))));
    }
    // BeehiveProvider.Client: the honey level.
    if let Some(hl) = p("honey_level").filter(|_| on(h, "beehive")) {
        lines.push(Line::text(trf(h, "jade.beehive.honey", "Honey: %s", &[&format!("§f{}", trf(h, "jade.fraction", "%d / %d", &[&hl, "5"]))])));
    }
    // EnchantmentPowerProvider: any block whose enchanting power bonus is positive.
    if on(h, "enchantment_power")
        && let Some(level) = level(h)
        && let Some(bs) = if block == real { level.block_state(h, pos) } else { bkey(h, block).and_then(|k| h.block_state_default(k)) }
        && let Ok(power) = level.get_enchant_power_bonus(h, pos, bs, pos)
        && power.is_finite()
        && power > 0.0
    {
        lines.push(Line::text(trf(h, "jade.ench_power", "Ench Power: %s", &[&format!("§f{}", trim2(f64::from(power)))])));
    }
    // SignProvider: the side the player faces, line by line.
    if access(h, "access_sign")
        && block.ends_with("_sign")
        && let Some(data) = block_entity_data(h, pos)
    {
        let front = sign_front_faces(h, pos, state);
        lines.push(Line::text(if front { tr(h, "jade.access.sign.front", "Front:") } else { tr(h, "jade.access.sign.back", "Back:") }));
        const LINES: [(&str, &str); 4] = [("jade.access.sign.line1", "First line: %s"), ("jade.access.sign.line2", "Second line: %s"), ("jade.access.sign.line3", "Third line: %s"), ("jade.access.sign.line4", "Fourth line: %s")];
        for (i, line) in sign_lines(&data, front).iter().enumerate().take(4) {
            lines.push(Line::text(if details { trf(h, LINES[i].0, LINES[i].1, &[line]) } else { format!("§f{line}") }));
        }
    }
    if access(h, "access_block") {
        // BlockDetailsBodyProvider.
        let dir_name = |d: &str| tr(h, &format!("jade.{d}"), &words(d));
        if block == "minecraft:redstone_wire" {
            let sides: Vec<String> = ["north", "south", "west", "east"].into_iter().filter(|d| p(d).is_some_and(|v| v != "none")).map(dir_name).collect();
            lines.push(Line::text(if sides.is_empty() { tr(h, "jade.access.block.redstone_wire.dot", "Shape: Dot") } else { trf(h, "jade.access.block.redstone_wire", "Shape: %s", &[&sides.join(", ")]) }));
        } else if !(block.ends_with("campfire") || matches!(block, "minecraft:decorated_pot" | "minecraft:small_dripleaf" | "minecraft:big_dripleaf" | "minecraft:big_dripleaf_stem" | "minecraft:pink_petals" | "minecraft:wildflowers")) {
            if let Some(shape) = p("shape").filter(|_| block.ends_with("rail")) {
                let english = words(&shape.replace("north_south", "north-south").replace("east_west", "east-west"));
                lines.push(Line::text(tr(h, &format!("jade.access.block.rail.{shape}"), &english)));
            }
            if let Some(facing) = p("facing") {
                lines.push(Line::text(trf(h, "jade.access.block.facing", "Facing: %s", &[&dir_name(&facing)])));
            }
        }
        // BlockAmountProvider.
        if let Some(n) = p("pickles").or_else(|| p("candles")).or_else(|| p("eggs")) {
            lines.push(Line::text(trf(h, "jade.access.block.amount", "Amount: {0}", &[&n])));
        }
        if let Some(n) = p("bites") {
            lines.push(Line::text(trf(h, "jade.access.block.bites", "Bites: {0}", &[&n])));
        }
        if let Some(n) = p("layers") {
            lines.push(Line::text(trf(h, "jade.access.block.layers", "Layers: {0}", &[&n])));
        }
        if let Some(n) = p("level").filter(|_| block.ends_with("_cauldron")) {
            lines.push(Line::text(trf(h, "jade.access.block.level", "Level: {0}", &[&n])));
        }
    }
    // TotalEnchantmentPowerProvider: the enchanting power contributed by valid surrounding blocks.
    if block == "minecraft:enchanting_table"
        && on(h, "total_enchantment_power")
        && let Some(level) = level(h)
    {
        let mut power = 0.0f32;
        for dx in -2..=2i32 {
            for dz in -2..=2i32 {
                if dx.abs() != 2 && dz.abs() != 2 {
                    continue;
                }
                for dy in 0..=1 {
                    let mid = [pos[0] + dx / 2, pos[1] + dy, pos[2] + dz / 2];
                    if !level.block(h, mid).is_some_and(|b| h.block_in_tag(b, "minecraft:enchantment_power_transmitter") == Some(true)) {
                        continue;
                    }
                    let shelf = [pos[0] + dx, pos[1] + dy, pos[2] + dz];
                    if let Some(bs) = level.block_state(h, shelf)
                        && let Ok(bonus) = level.get_enchant_power_bonus(h, shelf, bs, pos)
                        && bonus.is_finite()
                        && bonus > 0.0
                    {
                        power += bonus;
                    }
                }
            }
        }
        if power > 0.0 {
            lines.push(Line::text(trf(h, "jade.ench_power", "Ench Power: %s", &[&format!("§f{}", trim2(f64::from(power)))])));
        }
    }
}

/// A sign's lines, front or back, as plain text (`front_text.messages`, JSON components or strings).
fn sign_lines(data: &Compound, front: bool) -> Vec<String> {
    let Some(Tag::Compound(side)) = cget(data, if front { "front_text" } else { "back_text" }) else { return Vec::new() };
    let Some(Tag::List(msgs)) = cget(side, "messages") else { return Vec::new() };
    msgs.iter()
        .map(|m| match m {
            Tag::String(s) => {
                let s = s.to_string_lossy();
                // A JSON component's text, or the string itself.
                mod_api::serde_json::from_str::<mod_api::serde_json::Value>(&s).ok().and_then(|v| v.as_str().map(str::to_owned).or_else(|| v.get("text").and_then(|t| t.as_str()).map(str::to_owned))).unwrap_or(s)
            }
            Tag::Compound(c) => tag_str(cget(c, "text")).unwrap_or_default(),
            _ => String::new(),
        })
        .collect()
}

/// SignBlockEntity.isFacingFrontText: the side of the sign towards the player.
fn sign_front_faces(h: &dyn ClientHost, pos: [i32; 3], state: &str) -> bool {
    let Some((eye, _)) = aim(h) else { return true };
    let rotation = match prop(state, "rotation").and_then(|r| r.parse::<f32>().ok()) {
        Some(r) => r * 22.5,
        None => match prop(state, "facing").as_deref() {
            Some("north") => 180.0,
            Some("west") => 90.0,
            Some("east") => 270.0,
            _ => 0.0,
        },
    };
    let (dx, dz) = (eye[0] - (f64::from(pos[0]) + 0.5), eye[2] - (f64::from(pos[2]) + 0.5));
    let toward = (dz.atan2(dx).to_degrees() - 90.0) as f32;
    let diff = (toward - rotation + 540.0).rem_euclid(360.0) - 180.0;
    diff.abs() <= 90.0
}

/// AccessibilityPlugin.replaceTitle for a block: the patterns of `jade.access.block.*` its state calls for.
fn access_title(h: &dyn ClientHost, pos: [i32; 3], block: &str, state: &str, mut title: String) -> String {
    if !access(h, "access_block") {
        return title;
    }
    let mut wrap = |pattern: &str| title = fill(pattern, &[&title]);
    let p = |k: &str| prop(state, k);
    let yes = |k: &str| p(k).as_deref() == Some("true");
    if p("open").is_some() && block != "minecraft:barrel" {
        wrap(&if yes("open") { tr(h, "jade.access.block.door_open", "{0}") } else { tr(h, "jade.access.block.door_closed", "Closed {0}") });
    }
    for (key, pattern) in [("waterlogged", "Waterlogged {0}"), ("lit", "Lit {0}"), ("inverted", "Inverted {0}"), ("eye", "{0} with Eye"), ("ominous", "Ominous {0}")] {
        if yes(key) {
            wrap(&tr(h, &format!("jade.access.block.{key}"), pattern));
        }
    }
    if p("moisture").as_deref() == Some("7") {
        wrap(&tr(h, "jade.access.block.hydrated", "Hydrated {0}"));
    }
    for (key, lang, pattern) in [("locked", "jade.access.block.locked", "Locked {0}"), ("extended", "jade.access.block.extended", "Extended {0}"), ("can_summon", "jade.access.block.summonable", "Summonable {0}")] {
        if yes(key) {
            wrap(&tr(h, lang, pattern));
        }
    }
    if block.ends_with("_stairs") && p("half").as_deref() == Some("top") {
        wrap(&tr(h, "jade.access.block.upside_down", "Upside-down {0}"));
    }
    match p("hatch").as_deref() {
        Some("1") => wrap(&tr(h, "jade.access.block.hatch.1", "Slightly Cracked {0}")),
        Some("2") => wrap(&tr(h, "jade.access.block.hatch.2", "Cracked {0}")),
        _ => {}
    }
    if yes("powered") && (block == "minecraft:repeater" || block.ends_with("rail") || block.ends_with("_shelf")) {
        wrap(&tr(h, "jade.access.block.powered", "Powered {0}"));
    }
    let active = p("vault_state").as_deref() == Some("active") || p("trial_spawner_state").as_deref() == Some("active") || (block == "minecraft:creaking_heart" && p("creaking_heart_state").as_deref() != Some("uprooted"));
    if active {
        wrap(&tr(h, "jade.access.block.active", "Active {0}"));
    }
    // WaxedProvider.BlockComponent.
    if on(h, "waxed") && block.ends_with("_sign") && block_entity_data(h, pos).is_some_and(|d| matches!(cget(&d, "is_waxed"), Some(Tag::Byte(1)))) {
        wrap(&tr(h, "jade.access.waxed", "Waxed {0}"));
    }
    title
}

/// The owner's name on a player head (its `profile`: a name, or a compound's `name`).
fn head_owner(h: &dyn ClientHost, pos: [i32; 3]) -> Option<String> {
    let data = block_entity_data(h, pos)?;
    match cget(&data, "profile")? {
        Tag::String(s) => s.to_string().ok(),
        Tag::Compound(c) => tag_str(cget(c, "name")),
        _ => None,
    }
    .filter(|n| !n.is_empty())
}

/// The information options: BlockFaceProvider, CoordinatesProvider, DistanceProvider, BlockStatesProvider,
/// BlockPropertiesProvider, RegistryNameProvider.
fn info_lines(h: &dyn ClientHost, seen: &Seen, kind: Option<&str>, lines: &mut Vec<Line>) {
    let (eye, _) = aim(h).unwrap_or(([0.0; 3], [0.0; 3]));
    if let Seen::Block { face, .. } = seen
        && on(h, "block_face")
        && let Some(first) = lines.first_mut()
    {
        const FACES: [([i32; 3], &str); 6] = [([0, -1, 0], "down"), ([0, 1, 0], "up"), ([0, 0, -1], "north"), ([0, 0, 1], "south"), ([-1, 0, 0], "west"), ([1, 0, 0], "east")];
        if let Some((_, name)) = FACES.iter().find(|(d, _)| d == face) {
            first.els.push(El::Text(format!("§f{}", trf(h, "jade.blockFace", " (%s)", &[&tr(h, &format!("jade.{name}"), &words(name))]))));
        }
    }
    let (coords, distance) = (on(h, "coordinates"), on(h, "distance"));
    if coords || distance {
        let (at, d) = match seen {
            Seen::Block { pos, dist, .. } => (*pos, *dist),
            Seen::Entity { point, dist, .. } => (point.map(|v| v.floor() as i32), *dist),
        };
        let d = format!("{d:.1}");
        let d = d.strip_suffix(".0").unwrap_or(&d).to_owned();
        if coords {
            // Relative only while Ctrl is held.
            let ctrl = h.modifiers().bits & 2 != 0;
            let at = if on(h, "coordinates_rel") && ctrl { [at[0] - eye[0].floor() as i32, at[1] - eye[1].floor() as i32, at[2] - eye[2].floor() as i32] } else { at };
            let mut l = trf(h, "jade.blockpos", "%s %s %s", &[&format!("§#EF9A9A{}", at[0]), &format!("§#A5D6A7{}", at[1]), &format!("§#90CAF9{}", at[2])]);
            if distance {
                l.push_str(&format!("§r{}", trf(h, "jade.distance1", " [%s]", &[&d])));
            }
            lines.push(Line::text(l));
        } else {
            lines.push(Line::text(trf(h, "jade.distance2", "Distance: %s", &[&d])));
        }
    }
    let show_id = match opt(h, "registry_name").as_str() {
        "advanced" => h.gui_options().advanced_item_tooltips,
        "false" => false,
        _ => true,
    };
    match seen {
        Seen::Block { pos, block, real, state, .. } => {
            // A camouflaged block shows its disguise's default state.
            let state = if block == real { state.clone() } else { bkey(h, block).and_then(|k| h.block_state_default(k)).and_then(|s| h.block_state_string(s)).unwrap_or_else(|| block.clone()) };
            if on(h, "block_states") {
                let mut ps = props(&state);
                ps.sort();
                for (k, v) in ps {
                    lines.push(Line::text(format!("§7{k}: §f{v}")));
                }
            }
            if on(h, "block_properties") {
                // BlockPropertiesProvider. Speed and jump factors and fire's odds have no read in the mod API yet, so
                // their lines are left out until it has one (jade-facts.md).
                let f = crate::facts::fact(h, block).unwrap_or_default();
                lines.push(Line::text(trf(h, "jade.block_destroy_time", "Destroy time: %s", &[&format!("§f{}", trim2(f64::from(f.hardness)))])));
                if let Some(r) = f.explosion_resistance {
                    lines.push(Line::text(trf(h, "jade.block_explosion_resistance", "Explosion resistance: %s", &[&format!("§f{}", trim2(f64::from(r)))])));
                }
                if let Some(fr) = f.friction {
                    lines.push(Line::text(format!("Friction: §f{}", trim2(f64::from(fr)))));
                }
            }
            if show_id {
                lines.push(Line::text(format!("§8{block}")));
                if on(h, "registry_special") {
                    if block == real
                        && let Some(level) = level(h)
                        && let Some(be) = h.block_entity(level, *pos)
                        && let Some(kind) = be.kind(h)
                    {
                        let id = kind.id(h).unwrap_or_default();
                        if !id.is_empty() {
                            lines.push(Line::text(format!("§8{}", trf(h, "config.jade.plugin_jade.registry_name.special.block_entity_type", "BlockEntity: %s", &[id]))));
                        }
                    }
                    let flowing = prop(&state, "level").is_some_and(|l| l != "0");
                    let fluid = match block.as_str() {
                        "minecraft:water" if flowing => Some("minecraft:flowing_water"),
                        "minecraft:lava" if flowing => Some("minecraft:flowing_lava"),
                        "minecraft:water" => Some("minecraft:water"),
                        "minecraft:lava" => Some("minecraft:lava"),
                        _ if state.contains("waterlogged=true") => Some("minecraft:water"),
                        _ => None,
                    };
                    if let Some(fluid) = fluid {
                        lines.push(Line::text(format!("§8{}", trf(h, "config.jade.plugin_jade.registry_name.special.fluid", "Fluid: %s", &[fluid]))));
                    }
                }
            }
        }
        Seen::Entity { .. } => {
            if show_id && let Some(kind) = kind {
                lines.push(Line::text(format!("§8{kind}")));
            }
        }
    }
}

/// What a server line says about the box rather than in it: the icon, the title's pattern and the mob's stats.
fn apply_directives(h: &dyn ClientHost, lines: &mut Vec<Line>, icon: &mut Option<String>, badge: &mut bool) {
    let mut titles: Vec<String> = Vec::new();
    let mut stats = None;
    let mut any = false;
    for l in lines.iter_mut() {
        l.els.retain(|e| match e {
            El::Icon(id) => {
                *icon = Some(id.clone());
                any = true;
                false
            }
            El::Badge => {
                *badge = true;
                any = true;
                false
            }
            El::Title(t) => {
                titles.push(t.clone());
                any = true;
                false
            }
            El::Stats { absorb, armor } => {
                stats = Some((*absorb, *armor));
                any = true;
                false
            }
            _ => true,
        });
    }
    if !any {
        return;
    }
    let mut row = 0;
    lines.retain(|l| {
        row += 1;
        row == 1 || !l.els.is_empty()
    });
    if let Some(El::Text(title)) = lines.first_mut().and_then(|l| l.els.first_mut()) {
        for pattern in &titles {
            let now = title.strip_prefix("§f").unwrap_or(title.as_str()).to_owned();
            *title = format!("§f{}", fill(pattern, &[&now]));
        }
    }
    // EntityHealthAndArmorProvider.Client: both on one line only when both are numbers.
    if let Some((absorb, armor)) = stats {
        let mut hearts_at = None;
        for (i, l) in lines.iter_mut().enumerate() {
            for e in &mut l.els {
                if let El::Hearts { absorb: a, .. } = e {
                    *a = absorb;
                    hearts_at = Some(i);
                }
            }
        }
        lines.retain(|l| !l.els.iter().any(|e| matches!(e, El::Armor(_))));
        if armor > 0.0 && on(h, "entity_armor") {
            let armor_text = armor > num(h, "armor_max") as f32;
            let hearts_text = hearts_at.is_some_and(|i| {
                lines[i].els.iter().any(|e| match e {
                    El::Hearts { max, absorb, .. } => (((max.ceil() + absorb.ceil()) / 2.0).ceil() * 2.0) > num(h, "health_max") as f32 || on(h, "health_fractions"),
                    _ => false,
                })
            });
            match hearts_at {
                Some(i) if armor_text && hearts_text => {
                    lines[i].els.push(El::Space(4.0));
                    lines[i].els.push(El::Armor(armor));
                }
                Some(i) => lines.insert(i + 1, Line::new().with(El::Armor(armor))),
                None => lines.insert(lines.len().min(1), Line::new().with(El::Armor(armor))),
            }
        }
    }
}

/// SelectableSlotContainer.getHitSlot: the shelf or chiseled bookshelf slot the crosshair is on (-1 for none).
pub fn shelf_slot(pos: [i32; 3], block: &str, state: &str, hit: [f64; 3], face: [i32; 3]) -> i32 {
    let (rows, cols) = if block == "minecraft:chiseled_bookshelf" {
        (2, 3)
    } else if block.ends_with("_shelf") {
        (1, 3)
    } else {
        return -1;
    };
    let want: [i32; 3] = match prop(state, "facing").as_deref() {
        Some("north") => [0, 0, -1],
        Some("south") => [0, 0, 1],
        Some("west") => [-1, 0, 0],
        Some("east") => [1, 0, 0],
        _ => return -1,
    };
    if face != want {
        return -1;
    }
    let rel = |a: usize| hit[a] - f64::from(pos[a]);
    let (fx, fy) = match want {
        [0, 0, -1] => (1.0 - rel(0), rel(1)),
        [0, 0, 1] => (rel(0), rel(1)),
        [-1, 0, 0] => (rel(2), rel(1)),
        _ => (1.0 - rel(2), rel(1)),
    };
    let section = |c: f64, n: i32| ((c * f64::from(n)).floor() as i32).clamp(0, n - 1);
    section(fx, cols) + section(1.0 - fy, rows) * cols
}

/// What the overlay shows this frame, ready to draw (BoxElementImpl's tooltip and icon).
#[derive(Clone, Debug)]
pub struct BuiltBox {
    pub lines: Vec<Line>,
    pub icon: Option<String>,
    pub badge: bool,
    /// BreakingProgressProvider: how far, whether it fails, and its alpha.
    pub mining: Option<(f32, bool, f32)>,
    /// Tooltip.sneakyDetails.
    pub sneaky: bool,
}

/// The box for what is seen: None when nothing can be said yet (an entity the server hasn't described).
pub fn build(h: &dyn ClientHost, seen: &Seen) -> Option<BuiltBox> {
    let details = with(|c| c.details);
    let theme = theme::current(h);
    let mut badge = false;
    let (mut icon, title, ns, kind) = match seen {
        Seen::Block { pos, block, state, .. } => {
            let target = Target::Block { pos: *pos, block: block.clone(), state: state.clone(), hit: [0.0; 3] };
            let named = api::registry(|r| r.names.clone()).into_iter().find_map(|f| f(h, &target));
            match named {
                Some((icon, title, ns)) => (Some(icon).filter(|i| gfx::item_key(h, i).is_some()), title, ns, None),
                None if matches!(block.as_str(), "minecraft:water" | "minecraft:lava") => {
                    let name = h.translate(&format!("block.{}", block.replace(':', "."))).unwrap_or_else(|| words(block.trim_start_matches("minecraft:")));
                    (Some(format!("{block}_bucket")), name, "minecraft".to_owned(), None)
                }
                // PlayerHeadProvider: a head with an owner is named after them.
                None if matches!(block.as_str(), "minecraft:player_head" | "minecraft:player_wall_head") && on(h, "player_head") => {
                    let title = match head_owner(h, *pos) {
                        Some(owner) => tr(h, "block.minecraft.player_head.named", "%s's Head").replace("%s", &owner),
                        None => item_name(h, "minecraft:player_head"),
                    };
                    (Some("minecraft:player_head".to_owned()), title, "minecraft".to_owned(), None)
                }
                None => {
                    // WaxedProvider.BlockComponent.getIcon.
                    if on(h, "waxed") && block.ends_with("_sign") && block_entity_data(h, *pos).is_some_and(|d| matches!(cget(&d, "is_waxed"), Some(Tag::Byte(1)))) {
                        badge = true;
                    }
                    let ns = block.split_once(':').map_or("minecraft", |(n, _)| n).to_owned();
                    (block_item(h, block), block_name(h, block), ns, None)
                }
            }
        }
        Seen::Entity { id, .. } => {
            let data = with(|c| c.entity_data.clone()).filter(|d| d.id == *id && d.at.elapsed().as_millis() < 2000)?;
            if ignored_entity(h, &data.kind) || (!on(h, "display_bosses") && matches!(data.kind.as_str(), "minecraft:ender_dragon" | "minecraft:wither")) {
                return None;
            }
            let title = if data.name.is_empty() { entity_name(h, &data.kind) } else { resolve(h, &data.name) };
            let ns = data.kind.split_once(':').map_or("minecraft", |(n, _)| n).to_owned();
            (Some(data.icon.clone()).filter(|i| !i.is_empty() && gfx::item_key(h, i).is_some()), title, ns, Some(data.kind.clone()))
        }
    };
    let title = match seen {
        Seen::Block { pos, block, state, real, .. } if on(h, "accessibility_plugin") => {
            let state = if block == real { state.clone() } else { block.clone() };
            access_title(h, *pos, block, &state, title)
        }
        _ => title,
    };
    // The title line: the name, the harvest tool at its end (HarvestToolProvider).
    let mut title_line = Line::new();
    if on(h, "object_name") {
        title_line = title_line.with(El::Text(format!("§f{title}")));
    }
    let mut tool_line = None;
    if let Seen::Block { block, .. } = seen
        && on(h, "harvest_tool")
        && let Some(tool) = harvest_tool(h, block)
    {
        if on(h, "harvest_new_line") {
            tool_line = Some(Line { els: tool });
        } else {
            title_line = title_line.with(El::Space(6.0));
            title_line.els.extend(tool);
        }
    }
    let mut lines = vec![title_line];
    lines.extend(tool_line);
    let target = match seen {
        Seen::Block { pos, block, real, state, hit, .. } => {
            vanilla_block(h, *pos, block, real, state, details, &mut lines);
            Target::Block { pos: *pos, block: block.clone(), state: state.clone(), hit: *hit }
        }
        Seen::Entity { id, .. } => Target::Entity { id: *id, kind: kind.clone().unwrap_or_default() },
    };
    for p in api::registry(|r| r.client.clone()) {
        p(h, &target, &mut lines);
    }
    // The server's lines, while fresh.
    with(|c| match seen {
        Seen::Block { pos, .. } => {
            if let Some((p, server, at)) = &c.block_data
                && p == pos
                && at.elapsed().as_secs() < 2
            {
                lines.extend(server.iter().map(|s| api::decode(s)));
            }
        }
        Seen::Entity { id, .. } => {
            if let Some(d) = c.entity_data.as_ref().filter(|d| d.id == *id) {
                lines.extend(d.lines.iter().map(|s| api::decode(s)));
            }
        }
    });
    // Markers in every text, in this client's language.
    for l in &mut lines {
        for e in &mut l.els {
            match e {
                El::Text(t) | El::Small(t) | El::Mark(t) | El::Title(t) => *t = resolve(h, t),
                El::Bar { text, .. } => *text = resolve(h, text),
                _ => {}
            }
        }
    }
    apply_directives(h, &mut lines, &mut icon, &mut badge);
    // IThemeHelper.title: the title in the theme's colour.
    if on(h, "object_name")
        && let Some(El::Text(text)) = lines.first_mut().and_then(|l| l.els.first_mut())
        && let Some(rest) = text.strip_prefix("§f")
    {
        *text = format!("{}{rest}", theme.title_code);
    }
    // Display Mode Lite: the name and the mod only, the rest while Show Details is held.
    let mut sneaky = false;
    if opt(h, "display_mode") == "lite" && !details {
        sneaky = lines.len() > 1;
        lines.truncate(1);
    }
    info_lines(h, seen, kind.as_deref(), &mut lines);
    if on(h, "mod_name") {
        let text = format!("{}{}", theme.mod_code, mod_name(h, &ns));
        lines.push(if on(h, "mod_name_smaller") { Line::new().with(El::Small(text)) } else { Line::text(text) });
    }
    if on(h, "debug_mode") {
        lines.push(Line::text(format!("§8{seen:?}")));
    }
    // BreakingProgressProvider: a line along the box's foot while the block is mined.
    let mining = match (seen, me(h).and_then(|(p, _)| p.block_breaking(h))) {
        (Seen::Block { pos, block, .. }, Some(BlockBreakingState::Breaking { position, progress, .. })) if [position.x, position.y, position.z] == *pos && on(h, "breaking_progress") => {
            let alpha = with(|c| {
                c.progress_alpha = (c.progress_alpha + 0.05).min(0.6);
                c.progress_alpha
            });
            let ok = !requires_tool(h, block) || held_correct(h, block);
            Some((progress, !ok, alpha))
        }
        _ => {
            with(|c| c.progress_alpha = 0.0);
            None
        }
    };
    Some(BuiltBox { lines, icon, badge, mining, sneaky })
}

/// `OverlayRenderer.renderOverlay`'s stand-in while the position is adjusted with nothing looked at: the Grass Block.
pub fn sample_box(h: &dyn ClientHost) -> BuiltBox {
    let theme = theme::current(h);
    let title = format!("{}{}", theme.title_code, item_name(h, "minecraft:grass_block"));
    let modname = format!("{}{}", theme.mod_code, mod_name(h, "minecraft"));
    BuiltBox { lines: vec![Line::text(title), Line::text(modname)], icon: None, badge: false, mining: None, sneaky: false }
}

/// An element's size.
fn size(h: &dyn ClientHost, e: &El) -> (f32, f32) {
    match e {
        El::Text(t) => (gfx::width(h, t), 9.0),
        El::Item { small: true, .. } => (10.0, 9.0),
        El::Item { .. } => (18.0, 18.0),
        El::Arrow(_) => (22.0, 16.0),
        El::Bar { text, .. } => ((gfx::width(h, text) + 6.0).max(100.0), 12.0),
        El::Small(t) => (gfx::width(h, t) * 0.75, 7.0),
        El::Hearts { max, absorb, .. } => {
            let icons = ((max.ceil() + absorb.ceil()) / 2.0).ceil().max(1.0);
            let line = num(h, "health_per_line").max(1.0) as f32;
            if icons * 2.0 > num(h, "health_max") as f32 || on(h, "health_fractions") {
                (gfx::width(h, &format!("{:.0}/{:.0}", 0, max)) + 10.0, 9.0)
            } else {
                let per = icons.min(line);
                let rows = (icons / line).ceil();
                (8.0 * per + 1.0, 5.0 + 4.0 * rows)
            }
        }
        El::Armor(a) => {
            if *a > num(h, "armor_max") as f32 {
                (gfx::width(h, &format!("{a:.0}")) + 10.0, 9.0)
            } else {
                let icons = (a / 2.0).ceil();
                (8.0 * icons.min(10.0) + 1.0, 5.0 + 4.0 * (icons / 10.0).ceil())
            }
        }
        El::Space(w) => (*w, 0.0),
        El::Energy => (10.0, 9.0),
        El::Mark(_) | El::Icon(_) | El::Badge | El::Title(_) | El::Stats { .. } => (0.0, 0.0),
    }
}

fn hud_sprite(g: &mut Gfx<'_>, theme: &theme::Theme, name: &str, rect: [f32; 4]) {
    let atlas = theme.sprite(&format!("minecraft:{name}"));
    g.sprite(&atlas, [0.0, 0.0, 9.0, 9.0], rect, 0xFFFF_FFFF);
}

fn draw_el(g: &mut Gfx<'_>, theme: &theme::Theme, e: &El, x: f32, y: f32, h_line: f32, look: &theme::TextLook) {
    let h = g.h;
    let (_, eh) = size(h, e);
    let y = y + ((h_line - eh) / 2.0).floor();
    match e {
        El::Text(t) => g.text(&look.map(t), [x, y], look.base, look.shadow),
        El::Item { id, count, small } => {
            if id == "minecraft:air" {
                return;
            }
            if *small {
                g.item(id, [x, y], 8.0);
            } else {
                g.item(id, [x + 1.0, y + 1.0], 16.0);
                if *count > 1 {
                    let s = if *count >= 10_000 { format!("{}k", count / 1000) } else { count.to_string() };
                    let w = gfx::width(h, &s);
                    g.text(&s, [x + 18.0 - w, y + 10.0], look.amount, look.shadow);
                }
            }
        }
        // JadeUI.progressArrow: jade:progress_base, then jade:progress clipped to the done part.
        El::Arrow(f) => {
            g.sprite(&theme.sprite("jade:progress_base"), [0.0, 0.0, 22.0, 16.0], [x, y, 22.0, 16.0], 0xFFFF_FFFF);
            let done = (22.0 * f.clamp(0.0, 1.0)).floor();
            if done > 0.0 {
                g.sprite(&theme.sprite("jade:progress"), [0.0, 0.0, done, 16.0], [x, y, done, 16.0], 0xFFFF_FFFF);
            }
        }
        // A fluid or energy bar with its text.
        El::Bar { frac, color, text } => {
            let (w, _) = size(h, e);
            g.fill([x, y, w, 12.0], 0xFF2A_2A2A);
            g.fill([x + 1.0, y + 1.0, ((w - 2.0) * frac.clamp(0.0, 1.0)).floor(), 10.0], 0xFF00_0000 | color);
            g.text(&look.map(text), [x + 3.0, y + 2.0], 0xFFFF_FFFF, look.shadow);
        }
        // HealthElement: hearts in rows (or "health/max" past Max Health for Render).
        El::Hearts { health, max, absorb } => {
            let icons = ((max.ceil() + absorb.ceil()) / 2.0).ceil().max(1.0) as i32;
            let line = num(h, "health_per_line").max(1.0) as i32;
            if icons * 2 > num(h, "health_max") as i32 || on(h, "health_fractions") {
                hud_sprite(g, theme, "hud/heart/full", [x, y, 9.0, 9.0]);
                let t = format!("§f{:.0}/{:.0}", (health + absorb).ceil(), max.ceil());
                g.text(&t, [x + 10.0, y + 1.0], 0xFFFF_FFFF, look.shadow);
                return;
            }
            let hearts = health.round() * 0.5;
            let per = icons.min(line);
            let rows = (icons + line - 1) / line;
            for i in (1..=icons).rev() {
                let col = (i - 1) % per;
                let row = (i - 1) / per;
                let px = x + 8.0 * col as f32;
                let py = y + 4.0 * (rows - 1 - row) as f32;
                hud_sprite(g, theme, "hud/heart/container", [px, py, 9.0, 9.0]);
                let absorbing = i > (max * 0.5).ceil() as i32;
                let (full, half) = if absorbing { ("hud/heart/absorbing_full", "hud/heart/absorbing_half") } else { ("hud/heart/full", "hud/heart/half") };
                let cur = if absorbing { (max.ceil() + absorb) * 0.5 } else { hearts };
                if (i as f32) <= cur.floor() {
                    hud_sprite(g, theme, full, [px, py, 9.0, 9.0]);
                } else if (i as f32) < cur + 1.0 {
                    hud_sprite(g, theme, half, [px, py, 9.0, 9.0]);
                }
            }
        }
        El::Small(t) => g.text_scaled(&look.map(t), [x, y], 0.75, look.base, look.shadow),
        // ArmorElement: armour icons (or the number past Max Armors for Render).
        El::Armor(a) => {
            if *a > num(h, "armor_max") as f32 {
                hud_sprite(g, theme, "hud/armor_full", [x, y, 9.0, 9.0]);
                g.text(&format!("§f{a:.0}"), [x + 10.0, y + 1.0], 0xFFFF_FFFF, look.shadow);
                return;
            }
            let half = a * 0.5;
            let icons = half.ceil() as i32;
            for i in 1..=icons {
                let px = x + 8.0 * ((i - 1) % 10) as f32;
                let py = y + 4.0 * ((i - 1) / 10) as f32;
                let name = if (i as f32) <= half.floor() { "hud/armor_full" } else { "hud/armor_half" };
                hud_sprite(g, theme, name, [px, py, 9.0, 9.0]);
            }
        }
        // HarvestToolProvider's tick or cross: three quarters, over the last tool's corner.
        El::Mark(t) => {
            let (mx, my) = (x - 3.0, y - (h_line / 2.0).floor() + 3.0);
            g.text_scaled(&look.map(t), [mx, my], 0.75, look.base, look.shadow);
        }
        El::Energy => {
            g.sprite(&theme.sprite("jade:energy"), [0.0, 0.0, 16.0, 16.0], [x, y, 9.0, 9.0], 0xFFFF_FFFF);
        }
        El::Space(_) | El::Icon(_) | El::Badge | El::Title(_) | El::Stats { .. } => {}
    }
}

/// One axis of the overlay's position when it isn't a preset: `custom:<overlayPos>:<anchor>`.
pub fn custom_axis(value: &str) -> Option<(f32, f32)> {
    let mut w = value.strip_prefix("custom:")?.split(':');
    Some((w.next()?.parse().ok()?, w.next()?.parse().ok()?))
}

pub(crate) struct Placed {
    x: f32,
    y: f32,
    h: f32,
    icon_w: f32,
    title_extra: f32,
    sizes: Vec<(f32, f32)>,
    frame: [f32; 4],
    pivot: [f32; 2],
    scale: f32,
    rect: [f32; 4],
}

/// `BoxElementImpl.updateExpectedRect`.
fn place(h: &dyn ClientHost, [sw, sh]: [f32; 2], b: &BuiltBox, [pad_up, pad_down, pad_left, pad_right]: [f32; 4], adjusting: bool, theme: &theme::Theme) -> Placed {
    let line_size = |l: &Line| -> (f32, f32) {
        let mut w = 0.0f32;
        let mut lh = 0.0f32;
        for (i, e) in l.els.iter().enumerate() {
            let (ew, eh) = size(h, e);
            w += ew + if i > 0 { 1.0 } else { 0.0 };
            lh = lh.max(eh);
        }
        (w, lh.max(9.0))
    };
    let sizes: Vec<(f32, f32)> = b.lines.iter().map(line_size).collect();
    let icon_pos = opt(h, "display_item");
    let has_icon = b.icon.is_some() && icon_pos != "hide";
    let inline = icon_pos == "inline";
    let slot = theme.icon_slot().filter(|_| has_icon && !inline).map_or([0.0; 4], |s| s.padding);
    let icon_w = if !has_icon || inline { 0.0 } else { 20.0 + slot[1] + slot[3] };
    let title_extra = if inline && has_icon { 11.0 } else { 0.0 };
    let w = sizes.iter().enumerate().map(|(i, s)| s.0 + if i == 0 { title_extra } else { 0.0 }).fold(0.0, f32::max) + icon_w;
    let content_h = sizes.iter().map(|s| s.1 + 1.0).sum::<f32>() - 1.0;
    let content_h = if has_icon && !inline && slot != [0.0; 4] { content_h.max(16.0 + slot[0] + slot[2]) } else { content_h };
    let scale = num(h, "scale") as f32;
    let scale = if scale > 0.0 { scale } else { 1.0 };
    let (bw, bh) = (w + pad_left + pad_right, content_h + pad_up + pad_down);
    let bars = with(|c| c.bars);
    let push = !adjusting && bars > 0 && opt(h, "boss_bar_overlap") == "push";
    let flip = on(h, "flip_main_hand") && h.gui_options().main_hand_left;
    let side = opt(h, "overlay_x");
    let (x, pivot_x) = match custom_axis(&side) {
        Some((px, ax)) => {
            let (px, ax) = if flip { (1.0 - px, 1.0 - ax) } else { (px, ax) };
            let left = (sw * px - bw * scale * ax).floor();
            (left + pad_left, left)
        }
        None => {
            let mut x = match side.as_str() {
                "left" => 12.0,
                "right" => (sw - w - 12.0).floor(),
                _ => ((sw - w) / 2.0).floor(),
            };
            if flip {
                x = (sw - w - x).floor();
            }
            (x, sw / 2.0)
        }
    };
    let row = opt(h, "position");
    let (y, pivot_y) = match custom_axis(&row) {
        Some((py, ay)) => {
            let mut top = (sh * (1.0 - py) - bh * scale * ay).floor();
            let left = x - pad_left;
            let bar_bottom = 19.0 * bars as f32;
            if push && top < bar_bottom && left < sw / 2.0 + 91.0 && left + bw * scale > sw / 2.0 - 91.0 {
                top = bar_bottom;
            }
            (top + pad_up, top)
        }
        None => {
            let mut y = match row.as_str() {
                "1" => (sh / 3.0).floor(),
                "2" => ((sh - content_h) / 2.0).floor(),
                "3" => (sh - content_h - 60.0).floor(),
                _ => 8.0,
            };
            if push && !matches!(row.as_str(), "1" | "2" | "3") {
                y += 19.0 * bars as f32;
            }
            (y, y - 12.0)
        }
    };
    let (left, top) = (x - pad_left, y - pad_up);
    let rect = [pivot_x + (left - pivot_x) * scale, pivot_y + (top - pivot_y) * scale, bw * scale, bh * scale];
    Placed { x, y, h: content_h, icon_w, title_extra, sizes, frame: [left, top, bw, bh], pivot: [pivot_x, pivot_y], scale, rect }
}

/// `TooltipAnimation.expectedRect` for a box.
pub fn box_rect(h: &dyn ClientHost, size: [f32; 2], b: &BuiltBox, adjusting: bool) -> [f32; 4] {
    let theme = theme::current(h);
    place(h, size, b, theme.padding(), adjusting, &theme).rect
}

/// The box: the theme's frame round the icon column and the lines. Its rectangle on the screen.
pub fn draw_box(d: &mut dyn DrawContext, h: &dyn ClientHost, b: &BuiltBox, adjusting: bool, fade_in: bool) -> [f32; 4] {
    let theme = theme::current(h);
    let look = theme.text_look();
    let screen = d.size();
    let p = place(h, screen, b, theme.padding(), adjusting, &theme);
    let (x, y, ch, icon_w, title_extra) = (p.x, p.y, p.h, p.icon_w, p.title_extra);
    let icon_pos = opt(h, "display_item");
    let inline = icon_pos == "inline";
    let icon = b.icon.as_ref().filter(|_| icon_pos != "hide");
    // Overlay Animation: fades in over a tenth of a second.
    let alpha_fade = if fade_in && on(h, "animation") {
        with(|c| {
            let now = Instant::now();
            let dt = c.fade_at.map_or(0.0, |t| now.duration_since(t).as_secs_f32());
            c.fade_at = Some(now);
            c.fade = (c.fade + dt / 0.1).min(1.0);
            c.fade
        })
    } else {
        1.0
    };
    let mut g = Gfx::new(d, h);
    g.pivot = p.pivot;
    g.scale = p.scale;
    g.alpha = alpha_fade;
    let alpha = (num(h, "overlay_alpha") / 100.0).clamp(0.0, 1.0) as f32;
    theme::draw_background(&mut g, &theme, p.frame, icon.is_some(), alpha);
    if let Some(icon) = icon {
        let (at, isize) = if inline {
            ([x, y], 9.0)
        } else {
            let iy = if icon_pos == "centered" { y + ((ch - 16.0) / 2.0).max(0.0).floor() } else { y + ((ch - 16.0) / 2.0).clamp(0.0, 2.0) };
            match theme.icon_slot() {
                Some(slot) => {
                    let [top, _, bottom, left] = slot.padding;
                    let iy = if icon_pos == "centered" { y + ((ch - 16.0 - top - bottom) / 2.0).max(0.0).floor() + top } else { y + top };
                    let at = [x + left, iy];
                    slot.draw(&mut g, at, 16.0, alpha);
                    (at, 16.0)
                }
                None => ([x, iy], 16.0),
            }
        };
        g.item(icon, at, isize);
        // WaxedProvider.BlockComponent.waxedIcon: a honeycomb at half size on top.
        if b.badge {
            g.item("minecraft:honeycomb", [at[0] + isize / 2.0, at[1] + isize / 2.0], isize / 2.0);
        }
    }
    let text_alpha = num(h, "text_background_opacity");
    let mut ly = y;
    for (row, (l, (lw, lh))) in b.lines.iter().zip(&p.sizes).enumerate() {
        if text_alpha > 0.0 {
            let a = (text_alpha / 100.0 * 255.0) as u32;
            g.fill([x + icon_w - 1.0, ly - 1.0, lw + 2.0, lh + 1.0], a << 24);
        }
        let mut lx = x + icon_w + if row == 0 { title_extra } else { 0.0 };
        for e in &l.els {
            draw_el(&mut g, &theme, e, lx, ly, *lh, &look);
            lx += size(h, e).0 + 1.0;
        }
        ly += lh + 1.0;
    }
    if let Some((progress, failed, progress_alpha)) = b.mining {
        theme::draw_progress(&mut g, &theme, p.frame, progress, failed, progress_alpha * alpha);
    }
    if b.sneaky {
        let ticks = STARTED.with(|t| t.elapsed().as_secs_f32()) * 20.0;
        theme.draw_sneaky(&mut g, p.frame, ticks, 1.0);
    }
    p.rect
}

/// The HUD layer: the box, unless something hides it.
pub fn draw_layer(h: &dyn ClientHost, d: &mut dyn DrawContext) {
    // Narrate Target pressed: the box drawn now is said (nothing when there is none).
    let narrate_pressed = with(|c| std::mem::take(&mut c.narrate));
    with(|c| {
        c.bars = c.bars_now;
        c.bars_now = 0;
        c.showing = false;
    });
    let previewing = crate::config_ui::wants_box();
    // Hide from GUIs (a config screen with the preview shows it over itself), the player list, Show Overlay off.
    let screen_hides = !previewing && !h.playing() && on(h, "hide_from_guis");
    let list_hides = on(h, "hide_from_tab_list") && crate::keys::player_list_down(h);
    if !h.on(opts::ID) || h.local_player().is_none() || screen_hides || list_hides || with(|c| c.hidden) {
        with(|c| {
            c.seen = None;
            c.active = false;
        });
        return;
    }
    // JadeClient.inPowderSnowTime: the player's feet in powder snow.
    if let Some((p, _)) = me(h)
        && let Some(at) = p.entity().position(h)
        && let Some(level) = level(h)
        && level.block(h, at.map(|v| v.floor() as i32)).is_some_and(|b| b.id(h) == Some("minecraft:powder_snow"))
    {
        with(|c| c.powder_at = Some(Instant::now()));
    }
    let pinned = with(|c| c.pinned.clone());
    let seen = pinned.or_else(|| look(h));
    if with(|c| std::mem::take(&mut c.pin_requested)) {
        with(|c| c.pinned = if c.pinned.is_some() { None } else { seen.clone() });
    }
    // The server's pick (a dropped item, a falling block, a display, a mob past the hand's reach) when nearer.
    let seen = match (seen, with(|c| c.entity_data.clone())) {
        (s, Some(d)) if d.picked && d.at.elapsed().as_millis() < 600 => {
            let nearer = match &s {
                Some(Seen::Block { dist, .. }) => d.dist < *dist,
                Some(Seen::Entity { .. }) => false,
                None => true,
            };
            if nearer {
                let point = aim(h).map(|(e, dir)| [e[0] + dir[0] * d.dist, e[1] + dir[1] * d.dist, e[2] + dir[2] * d.dist]).unwrap_or([0.0; 3]);
                Some(Seen::Entity { id: d.id, point, dist: d.dist })
            } else {
                s
            }
        }
        (s, _) => s,
    };
    with(|c| {
        c.seen = seen.clone();
        c.active = true;
    });
    // Boss Bar Overlap's Hide Tooltip.
    let bars = with(|c| c.bars);
    let at_top = opt(h, "position");
    if bars > 0 && opt(h, "boss_bar_overlap") == "hide_tip" && !previewing && (at_top == "0" || at_top.starts_with("custom:")) {
        return;
    }
    let Some(seen) = seen else {
        with(|c| {
            c.progress_alpha = 0.0;
            c.fade = 0.0;
            c.lookup = None;
        });
        return;
    };
    let Some(built) = build(h, &seen) else { return };
    with(|c| c.showing = true);
    if previewing {
        with(|c| c.built = Some(built));
        return;
    }
    // WailaTickHandler.narrate: a press says the box; Toggle mode says each new one (dedupe).
    let text = spoken(&built);
    if !text.is_empty() {
        let say = with(|c| {
            let say = narrate_pressed || (c.tts_on && c.said != text);
            if say {
                c.said.clone_from(&text);
            }
            say
        });
        if say {
            with(|c| c.speak = Some(text));
        }
    }
    draw_box(d, h, &built, false, true);
}

/// Mod Name on Item Tooltip: the stack's mod at the end of its tooltip.
pub fn item_tooltip(h: &dyn ClientHost, e: &mut ItemTooltipEvent<'_>) {
    if !h.on(opts::ID) || !on(h, "item_mod_name") {
        return;
    }
    let id = e.stack.item().id(h).unwrap_or_default().to_owned();
    let ns = id.split_once(':').map_or("minecraft", |(n, _)| n);
    let mut style = mod_api::text::TextStyle::default();
    style.color = Some("blue".to_owned());
    style.italic = Some(true);
    let _ = e.lines.push(Text::literal(mod_name(h, ns)).with_style(style), u32::MAX);
}

/// The plain text of a box, line by line (what Narrate Target would say).
pub fn spoken(b: &BuiltBox) -> String {
    b.lines
        .iter()
        .map(|l| l.els.iter().filter_map(|e| if let El::Text(t) = e { Some(plain(t)) } else { None }).collect::<Vec<_>>().join(" "))
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>()
        .join(". ")
}
