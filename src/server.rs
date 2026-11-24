//! The server half (Jade's server data providers and its network handler): answers a player's request about the block
//! or entity they look at, with what only the server knows, and `/jade show|hide` and the
//! `jade:max_position_deviation` rule.
//!
//! Multiplayer rules, as Jade's: a client can ask about anything, so the server checks the request's size and rate,
//! that the target is in the player's own level and within their reach plus `jade:max_position_deviation`
//! (BlockAccessorImpl / EntityAccessorImpl.isOutOfReach), and what an operator alone may see (command blocks, loot
//! tables). The player's own options only narrow what is sent; the server's `server-plugin-overrides.json` wins over them.
use crate::api::{self, El, Line, Request, item_name, tr};
use crate::opts::{self, human, num, on, opt, seconds_marker};
use crate::overlay::NET_DATA;
use mod_api::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The most a client may ask: an honest one asks a few times a second for what it looks at.
const REQUEST_BYTES: usize = 4096;
const SETTINGS_BYTES: usize = 16384;
const REQUESTS_PER_SECOND: u32 = 60;

/// Jade.MAX_POSITION_DEVIATION: the integer game rule `jade:max_position_deviation` (21, 0 to 1000) in Jade's
/// category, the player rules. `/gamerule` sets it and the Game Rules screen shows it; the world saves it.
const RULE_DEFAULT: i32 = 21;
static RULE: OnceLock<IntGameRuleKey> = OnceLock::new();

/// `register`: declares the rule.
pub fn register_rules(r: &mut dyn Registrar) {
    if let Ok(key) = r.game_rule_int("max_position_deviation", "minecraft:player", RULE_DEFAULT, 0, 1000) {
        let _ = RULE.set(key);
    }
}

/// The rule's value in `level`'s world (its default when it can't be read).
fn max_deviation(host: &dyn Host, level: LevelRef) -> i32 {
    RULE.get().and_then(|key| level.game_rule_int(host, *key)).unwrap_or(RULE_DEFAULT)
}

static RATES: Mutex<Option<HashMap<u128, (std::time::Instant, u32)>>> = Mutex::new(None);

thread_local! {
    /// Where the player answered for right now is looking on the block.
    static REQUEST_HIT: RefCell<Option<[f64; 3]>> = const { RefCell::new(None) };
}

pub fn request_hit() -> Option<[f64; 3]> {
    REQUEST_HIT.with_borrow(|h| *h)
}

fn request_allowed(player: u128) -> bool {
    let mut g = RATES.lock().unwrap_or_else(|e| e.into_inner());
    let rates = g.get_or_insert_with(HashMap::new);
    if rates.len() > 4096 {
        rates.clear();
    }
    let now = std::time::Instant::now();
    let (since, count) = rates.entry(player).or_insert((now, 0));
    if now.duration_since(*since).as_secs() >= 1 {
        *since = now;
        *count = 0;
    }
    *count += 1;
    *count <= REQUESTS_PER_SECOND
}

fn cget<'a>(c: &'a Compound, key: &str) -> Option<&'a Tag> {
    c.iter().find(|(k, _)| k.to_string_lossy() == key).map(|(_, v)| v)
}

fn tget<'a>(t: &'a Tag, key: &str) -> Option<&'a Tag> {
    t.get(key)
}

fn int(t: Option<&Tag>) -> Option<i64> {
    match t? {
        Tag::Byte(v) => Some(i64::from(*v)),
        Tag::Short(v) => Some(i64::from(*v)),
        Tag::Int(v) => Some(i64::from(*v)),
        Tag::Long(v) => Some(*v),
        Tag::Float(v) => Some(f32::from_bits(*v) as i64),
        Tag::Double(v) => Some(f64::from_bits(*v) as i64),
        _ => None,
    }
}


fn string(t: Option<&Tag>) -> Option<String> {
    match t? {
        Tag::String(s) => s.to_string().ok(),
        _ => None,
    }
}

/// An item stack's id and count as saved (`{id, count}`).
fn stack_of(t: &Tag) -> Option<(String, u32)> {
    let id = string(tget(t, "id"))?;
    let count = int(tget(t, "count")).or_else(|| int(tget(t, "Count"))).unwrap_or(1).max(0) as u32;
    (id != "minecraft:air" && count > 0).then_some((id, count))
}

/// A saved `Items` list: each stack with its slot.
fn items_of(c: &Compound, key: &str) -> Vec<(i64, String, u32)> {
    let Some(Tag::List(list)) = cget(c, key) else { return Vec::new() };
    list.iter().enumerate().filter_map(|(i, t)| {
        let (id, n) = stack_of(t)?;
        Some((int(tget(t, "Slot")).unwrap_or(i as i64), id, n))
    }).collect()
}

/// Jade.isOutOfReach: the hand's range and the deviation, from where the server has the player; another dimension is
/// always out.
fn out_of_reach(host: &dyn Host, player: PlayerRef, level: LevelRef, at: [f64; 3], base: f64) -> bool {
    let Some(pl) = player.level(host) else { return true };
    if pl != level {
        return true;
    }
    let Some(p) = player.entity().position(host) else { return true };
    let most = base + f64::from(max_deviation(host, level));
    (0..3).map(|i| (p[i].floor() - at[i].floor()).powi(2)).sum::<f64>() > most * most
}

/// `net_from_client` on `jade:request`.
pub fn from_client(host: &mut dyn ServerHost, player: u128, data: &[u8]) {
    if data.len() > SETTINGS_BYTES || !request_allowed(player) {
        return;
    }
    let text = String::from_utf8_lossy(data);
    if let Some(rest) = text.strip_prefix("s ") {
        opts::set_player_values(player, rest);
        // ServerHandshakePacket.shearableBlocks: the blocks the server's loot tables ask shears of.
        if let Some(p) = host.player(player) {
            let msg = crate::facts::shears_message(&*host);
            let _ = p.send_payload(host, &PayloadId(NET_DATA.into()), PayloadBytes(msg.into_bytes()));
        }
        return;
    }
    if data.len() > REQUEST_BYTES {
        return;
    }
    let Some(p) = host.player(player) else { return };
    let Some(level) = p.level(host) else { return };
    let answer = opts::with_player(player, || {
        if let Some(rest) = text.strip_prefix("b ") {
            block_request(host, player, p, level, rest)
        } else if let Some(rest) = text.strip_prefix("e ") {
            entity_request(host, player, p, level, rest)
        } else if let Some(rest) = text.strip_prefix("n ") {
            pick_request(host, player, p, level, rest)
        } else {
            None
        }
    });
    if let Some(answer) = answer {
        let _ = p.send_payload(host, &PayloadId(NET_DATA.into()), PayloadBytes(answer.into_bytes()));
    }
}

fn lines_out(head: String, lines: Vec<Line>) -> String {
    let mut out = head;
    for l in lines {
        out.push('\n');
        out.push_str(&api::encode(&l));
    }
    // Sunrise's cap is 30 000 bytes a message.
    if out.len() > 28_000 {
        let mut cut = 28_000;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        if let Some(nl) = out.rfind('\n') {
            out.truncate(nl);
        }
    }
    out
}

/// `b x y z details slot hx hy hz extra dist`: a block's lines, or the pick when a special entity is nearer.
fn block_request(host: &mut dyn ServerHost, player: u128, p: PlayerRef, level: LevelRef, text: &str) -> Option<String> {
    let w: Vec<&str> = text.split(' ').collect();
    let n = |i: usize| w.get(i).and_then(|v| v.parse::<i64>().ok());
    let f = |i: usize| w.get(i).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite());
    let pos = [n(0)? as i32, n(1)? as i32, n(2)? as i32];
    let details = n(3).unwrap_or(0) != 0;
    let slot = n(4).unwrap_or(-1) as i32;
    let hit = match (n(5), n(6), n(7)) {
        (Some(x), Some(y), Some(z)) => Some([x as f64 / 4096.0, y as f64 / 4096.0, z as f64 / 4096.0]),
        _ => None,
    };
    // The hit must be on (or by) the block it names.
    let hit = hit.filter(|h| (0..3).all(|i| (h[i] - f64::from(pos[i]) - 0.5).abs() <= 1.0));
    let extra = f(8).unwrap_or(0.0).clamp(0.0, 20.0);
    let block_dist = f(9).unwrap_or(f64::INFINITY);
    if let Some(picked) = pick(host, p, level, extra, block_dist, details) {
        return Some(picked);
    }
    // BlockAccessorImpl: within reach of the player only (a client can ask anything).
    if out_of_reach(host, p, level, pos.map(f64::from), 4.5) || !level.is_loaded(host, pos) {
        return None;
    }
    let block = level.block(host, pos)?.id(host).unwrap_or_default().to_owned();
    let dimension = p.dimension(host).map(|d| d.to_string()).unwrap_or_default();
    let req = Request { player, dimension, pos, block: block.clone(), entity: None, details, hit };
    REQUEST_HIT.with_borrow_mut(|h| *h = hit);
    let mut lines = Vec::new();
    vanilla_block(host, &req, p, level, slot, &mut lines);
    for provider in api::registry(|r| r.server.clone()) {
        provider(host, &req, &mut lines);
    }
    REQUEST_HIT.with_borrow_mut(|h| *h = None);
    Some(lines_out(format!("B {} {} {}", pos[0], pos[1], pos[2]), lines))
}

/// `e id details`: the entity on the player's crosshair.
fn entity_request(host: &mut dyn ServerHost, player: u128, p: PlayerRef, level: LevelRef, text: &str) -> Option<String> {
    let w: Vec<&str> = text.split(' ').collect();
    let id: u32 = w.first()?.parse().ok()?;
    let details = w.get(1) == Some(&"1");
    let e = host.entity(id)?;
    if e.level(host) != Some(level) {
        return None;
    }
    let at = e.position(host)?;
    // EntityAccessorImpl: within reach of the player only.
    if out_of_reach(host, p, level, at, 3.0) {
        return None;
    }
    let eye = p.entity().eye_position(host).unwrap_or(at);
    let d = (0..3).map(|i| (at[i] - eye[i]).powi(2)).sum::<f64>().sqrt();
    describe(host, player, p, level, e, details, false, d)
}

/// `n extra details`: nothing on the client's crosshair; the server's pick.
fn pick_request(host: &mut dyn ServerHost, _player: u128, p: PlayerRef, level: LevelRef, text: &str) -> Option<String> {
    let w: Vec<&str> = text.split(' ').collect();
    let extra = w.first().and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite()).unwrap_or(0.0).clamp(0.0, 20.0);
    let details = w.get(1) == Some(&"1");
    Some(pick(host, p, level, extra, f64::INFINITY, details).unwrap_or_else(|| "N".to_owned()))
}

/// RayTracing.getEntityHitResult for what the client's crosshair doesn't pick: dropped items, falling blocks and
/// displays (a display's box widened by 0.3, as Jade's), and with Extended Reach anything past the hand's reach.
/// From where the server has the player's eyes, nearer than the client's block.
fn pick(host: &mut dyn ServerHost, p: PlayerRef, level: LevelRef, extra: f64, block_dist: f64, details: bool) -> Option<String> {
    let me = p.entity();
    let eye = me.eye_position(host)?;
    let look = me.view_vector(host)?;
    let reach = host.living(me).and_then(|l| l.attribute_value(host, "minecraft:entity_interaction_range")).unwrap_or(3.0) + extra;
    let to = [eye[0] + look[0] * reach, eye[1] + look[1] * reach, eye[2] + look[2] * reach];
    // The candidates along the ray (boxes grown by Jade's 0.3, so a display, which has none, is among them), then
    // each one's own box (a display's that 0.3 round its position) met by the ray, nearest first.
    let hits = level.ray_trace_entities(host, eye, to, 0.3, Some(me))?;
    let mut best: Option<(f64, EntityRef)> = None;
    for hit in hits {
        let Some(kind) = hit.entity.kind(host).map(|k| k.id(&*host).map(|n| n.as_str().to_owned()).unwrap_or_default()) else { continue };
        let display = matches!(kind.as_str(), "minecraft:block_display" | "minecraft:item_display");
        let special = display || matches!(kind.as_str(), "minecraft:item" | "minecraft:falling_block");
        if !special && extra <= 0.0 {
            continue;
        }
        let bounds = if display {
            hit.entity.position(host).map(|c| [[c[0] - 0.3, c[1] - 0.3, c[2] - 0.3], [c[0] + 0.3, c[1] + 0.3, c[2] + 0.3]])
        } else {
            hit.entity.bounding_box(host)
        };
        let Some([lo, hi]) = bounds else { continue };
        if let Some(t) = ray_box(eye, look, lo, hi, reach.min(block_dist))
            && best.is_none_or(|(b, _)| t < b)
        {
            best = Some((t, hit.entity));
        }
    }
    // Displays aren't on the entity-list walk `ray_trace_entities` makes: the level's residents within reach, as
    // Jade's pick sees them (a box 0.3 round the display's position).
    let shared: &dyn ServerHost = &*host;
    let _ = level.visit_all_entities(shared, &mut |e| {
        let Some(c) = e.position(shared) else { return std::ops::ControlFlow::Continue(()) };
        if (0..3).map(|i| (c[i] - eye[i]).powi(2)).sum::<f64>() > (reach + 1.0).powi(2) || shared.display(e).is_none() {
            return std::ops::ControlFlow::Continue(());
        }
        let (lo, hi) = ([c[0] - 0.3, c[1] - 0.3, c[2] - 0.3], [c[0] + 0.3, c[1] + 0.3, c[2] + 0.3]);
        if let Some(t) = ray_box(eye, look, lo, hi, reach.min(block_dist))
            && best.is_none_or(|(b, _)| t < b)
        {
            best = Some((t, e));
        }
        std::ops::ControlFlow::Continue(())
    });
    let (d, e) = best?;
    describe(host, p.uuid(), p, level, e, details, true, d)
}

/// Where a ray enters a box, within `reach`.
fn ray_box(eye: [f64; 3], dir: [f64; 3], lo: [f64; 3], hi: [f64; 3], reach: f64) -> Option<f64> {
    let (mut t0, mut t1) = (0.0f64, reach);
    for a in 0..3 {
        if dir[a].abs() < 1e-9 {
            if eye[a] < lo[a] || eye[a] > hi[a] {
                return None;
            }
            continue;
        }
        let (mut ta, mut tb) = ((lo[a] - eye[a]) / dir[a], (hi[a] - eye[a]) / dir[a]);
        if ta > tb {
            std::mem::swap(&mut ta, &mut tb);
        }
        t0 = t0.max(ta);
        t1 = t1.min(tb);
    }
    (t0 <= t1).then_some(t0)
}

/// An entity's answer: `E id picked dist kind`, its custom name (or nothing), its icon item, its lines.
#[allow(clippy::too_many_arguments)]
fn describe(host: &mut dyn ServerHost, player: u128, p: PlayerRef, level: LevelRef, e: EntityRef, details: bool, picked: bool, d: f64) -> Option<String> {
    let kind = e.kind(host)?.id(&*host).map(|n| n.as_str().to_owned()).unwrap_or_default();
    let nbt = e.nbt(host).unwrap_or_default();
    let name = if cget(&nbt, "CustomName").is_some() {
        e.name(host).unwrap_or_default().replace('\n', " ")
    } else if kind == "minecraft:item" {
        // ItemEntity's name is its stack's.
        cget(&nbt, "Item").and_then(stack_of).map(|(id, _)| api::item_name(&id)).unwrap_or_default()
    } else {
        String::new()
    };
    // BlockDisplayProvider and ItemDisplayProvider: what a display shows, through the displays API.
    let shown = host.display(e).and_then(|d| match d.item(host) {
        Some(stack) => Some(stack.item().id(host).unwrap_or_default().to_owned()),
        None => d.block_state(host).and_then(|s| host.block_item(s.block())).map(|i| i.id(host).unwrap_or_default().to_owned()),
    });
    let icon = shown.filter(|id| id != "minecraft:air").unwrap_or_else(|| entity_icon(host, &kind, &nbt));
    let dimension = p.dimension(host).map(|d| d.to_string()).unwrap_or_default();
    let pos = e.block_position(host).unwrap_or([0; 3]);
    let req = Request { player, dimension, pos, block: String::new(), entity: Some(e.id()), details, hit: None };
    let mut lines = Vec::new();
    crate::entities::lines(host, &req, p, level, e, &kind, &nbt, &mut lines);
    for provider in api::registry(|r| r.server_entity.clone()) {
        provider(host, &req, e, &mut lines);
    }
    let head = format!("E {} {} {d} {kind}\n{name}\n{icon}", e.id(), u8::from(picked));
    Some(lines_out(head, lines))
}

/// Entity.getPickResult as the icon: a mob's spawn egg, the framed item, the vehicle's item, a dropped item's own.
fn entity_icon(host: &dyn Host, kind: &str, nbt: &Compound) -> String {
    let item_of = |key: &str| cget(nbt, key).and_then(stack_of).map(|(id, _)| id);
    match kind {
        "minecraft:item" => item_of("Item").unwrap_or_default(),
        "minecraft:item_frame" | "minecraft:glow_item_frame" => item_of("Item").unwrap_or_else(|| kind.to_owned()),
        "minecraft:item_display" => item_of("item").unwrap_or_default(),
        "minecraft:block_display" => cget(nbt, "block_state").and_then(|t| string(tget(t, "Name"))).unwrap_or_default(),
        "minecraft:falling_block" => cget(nbt, "BlockState").and_then(|t| string(tget(t, "Name"))).unwrap_or_default(),
        "minecraft:painting" | "minecraft:armor_stand" | "minecraft:end_crystal" | "minecraft:leash_knot" => kind.to_owned(),
        k if k.ends_with("minecart") || k.ends_with("boat") || k.ends_with("raft") => k.to_owned(),
        _ => {
            let egg = format!("{kind}_spawn_egg");
            if host.item_keys().into_iter().any(|k| k.id(host) == Some(egg.as_str())) { egg } else { String::new() }
        }
    }
}

/// ItemStorageProvider: the stacks, the same items together, as many rows as its amounts allow: a few types by name
/// (Show Name Threshold, or always with Always Use List Mode), the rest as icons.
pub fn storage(host: &dyn Host, details: bool, lines: &mut Vec<Line>, stacks: impl IntoIterator<Item = (String, u32)>) {
    if !on(host, "item_storage") {
        return;
    }
    let mut merged: Vec<(String, u32)> = Vec::new();
    for (id, n) in stacks {
        match merged.iter_mut().find(|(m, _)| *m == id) {
            Some(e) => e.1 = e.1.saturating_add(n),
            None => merged.push((id, n)),
        }
    }
    if on(host, "storage_sort") {
        merged.sort_by_key(|e| std::cmp::Reverse(e.1));
    }
    let amount = num(host, if details { "storage_detailed" } else { "storage_normal" }).clamp(0.0, 512.0) as usize;
    let extra = merged.len().saturating_sub(amount);
    merged.truncate(amount);
    let threshold = num(host, "storage_names") as usize;
    if on(host, "always_list_mode") || merged.len() < threshold {
        // ItemStorageProvider.Client.append: "<count>× <name>".
        for (id, n) in &merged {
            let count = human(i64::from(*n), "");
            lines.push(Line::new().with(Line::small(id)).with(El::Text(format!("§f{count}× {}", item_name(id)))));
        }
    } else {
        for row in merged.chunks((num(host, "storage_per_line").clamp(1.0, 64.0)) as usize) {
            let mut l = Line::new();
            for (id, n) in row {
                l = l.with(Line::item(id, *n));
            }
            lines.push(l);
        }
    }
    if extra > 0 {
        lines.push(Line::text(format!("§7and {extra} more")));
    }
}

fn fluid_color(fluid: &str) -> u32 {
    match fluid {
        "minecraft:water" => 0x3F76E4,
        "minecraft:lava" => 0xD96415,
        "minecraft:powder_snow" => 0xF0F0F0,
        _ => 0x8080FF,
    }
}

/// A fluid's name: its `block.<ns>.<path>` (vanilla's), resolved on the client.
fn fluid_name(fluid: &str) -> String {
    let (ns, path) = fluid.split_once(':').unwrap_or(("minecraft", fluid));
    tr(&format!("block.{ns}.{path}"), path, &[])
}

/// FluidStorageProvider's line in Fluid Storage's Display Style: "Water 4,000 mB" (with Show Details "/ 16,000 mB").
pub fn fluid_line(host: &dyn Host, fluid: Option<&str>, mb: u32, capacity: u32, details: bool) -> Line {
    let style = opt(host, "fluid_style");
    let commas = |n: u32| opts::commas(i64::from(n));
    let text = match fluid {
        Some(f) if mb > 0 => {
            if details && style == "bar" {
                let amounts = tr("jade.fluid.with_capacity", "%s / %s", &[&format!("§f{} mB§r", commas(mb)), &format!("{} mB", commas(capacity))]);
                tr("jade.fluid", "%s %s", &[&fluid_name(f), &amounts])
            } else {
                tr("jade.fluid", "%s %s", &[&fluid_name(f), &format!("§f{} mB", commas(mb))])
            }
        }
        _ => tr("jade.fluid", "%s %s", &[&tr("jade.fluid.empty", "Empty", &[]), &format!("{} mB", commas(capacity))]),
    };
    match style.as_str() {
        "plain" => Line::text(text),
        "icon" => Line::new().with(Line::small("minecraft:bucket")).with(El::Text(text)),
        _ => Line::new().with(El::Bar { frac: mb as f32 / capacity.max(1) as f32, color: fluid.map_or(0x8080FF, fluid_color), text }),
    }
}

/// EnergyStorageProvider through the `ENERGY` capability: any block's FE buffer, from any side.
fn energy_lines(host: &mut dyn ServerHost, level: LevelRef, pos: [i32; 3], lines: &mut Vec<Line>) {
    let sides = std::iter::once(None).chain(Direction::ALL.into_iter().map(Some));
    let mut found = None;
    for side in sides {
        if let Some(cap) = level.capability(host, &ENERGY, pos, side) {
            let (amount, capacity) = (cap.amount(host), cap.capacity(host));
            if capacity > 0 {
                found = Some((amount, capacity));
                break;
            }
        }
    }
    let Some((amount, capacity)) = found else { return };
    let (current, max) = (human(i64::from(amount), "FE"), human(i64::from(capacity), "FE"));
    lines.push(match opt(host, "energy_style").as_str() {
        "plain" => Line::text(tr("jade.energy.text", "Energy: %s", &[&tr("jade.fe", "%s / %s", &[&current, &max])])),
        "icon" => Line::new().with(El::Energy).with(El::Text(tr("jade.fe", "%s / %s", &[&current, &max]))),
        _ => Line::new().with(El::Bar { frac: (amount as f32 / capacity as f32).clamp(0.0, 1.0), color: 0xB01010, text: tr("jade.fe", "%s / %s", &[&format!("§f{current}§r"), &max]) }),
    });
}

/// FluidStorageProvider through the `FLUIDS` capability: each tank of any block, from any side, once.
fn fluid_lines(host: &mut dyn ServerHost, level: LevelRef, pos: [i32; 3], details: bool, lines: &mut Vec<Line>) {
    let sides = std::iter::once(None).chain(Direction::ALL.into_iter().map(Some));
    for side in sides {
        let Some(cap) = level.capability(host, &FLUIDS, pos, side) else { continue };
        let size = cap.size(host).min(16);
        if size == 0 {
            continue;
        }
        let water = host.fluid_key("minecraft:water").map(|k| FluidResource::of(&FluidStack::new(k, 1000)));
        for i in 0..size {
            let res = cap.resource(host, i);
            let amount = cap.amount(host, i);
            let capacity = match (&res, &water) {
                (Some(r), _) => cap.capacity(host, i, Some(r)),
                (None, Some(w)) => cap.capacity(host, i, Some(w)),
                _ => 0,
            };
            let id = res.as_ref().map(|r| match *r.fluid() {
                FluidIdentity::Resolved(k) => k.id(host).unwrap_or_default().to_owned(),
                FluidIdentity::Unresolved(u) => u.id(host),
                _ => String::new(),
            });
            lines.push(fluid_line(host, id.as_deref(), amount, capacity.max(amount), details));
        }
        return;
    }
}

/// ItemStorageProvider through the `ITEMS` capability: any block's slots (a double chest's both halves).
fn item_lines(host: &mut dyn ServerHost, level: LevelRef, pos: [i32; 3], details: bool, lines: &mut Vec<Line>) -> bool {
    let Some(cap) = level.capability(host, &ITEMS, pos, None) else { return false };
    let size = cap.size(host).min(4096);
    let mut stacks = Vec::new();
    for i in 0..size {
        if let Some(r) = cap.resource(host, i) {
            let n = cap.amount(host, i);
            if n > 0 {
                stacks.push((r.item().id(host).unwrap_or_default().to_owned(), n));
            }
        }
    }
    storage(host, details, lines, stacks);
    true
}

/// The server's lines Jade itself gives for a block (VanillaPlugin's server data, the universal providers).
fn vanilla_block(host: &mut dyn ServerHost, req: &Request, p: PlayerRef, level: LevelRef, slot: i32, lines: &mut Vec<Line>) {
    let (pos, block, details) = (req.pos, req.block.as_str(), req.details);
    let privileged = p.is_creative(host) == Some(true) || p.is_spectator(host) == Some(true);
    let op = p.permission_level(host).unwrap_or(0) >= 2;
    let hidden = api::hidden_storage(block);
    // RedstoneProvider: a comparator's output, a calibrated sculk sensor's input.
    let be = host.block_entity(level, pos);
    let data = be.and_then(|b| b.data(host)).unwrap_or_default();
    if on(host, "redstone") {
        if block == "minecraft:comparator" {
            let n = int(cget(&data, "OutputSignal")).unwrap_or(0);
            lines.push(Line::text(tr("tooltip.jade.power", "Power: %d", &[&format!("§f{n}")])));
        } else if block == "minecraft:calibrated_sculk_sensor" {
            let facing = level.block_property(host, pos, "facing");
            let back = match facing.as_deref() {
                Some("north") => (Direction::South, [0, 0, 1]),
                Some("south") => (Direction::North, [0, 0, -1]),
                Some("west") => (Direction::East, [1, 0, 0]),
                _ => (Direction::West, [-1, 0, 0]),
            };
            let at = [pos[0] + back.1[0], pos[1] + back.1[1], pos[2] + back.1[2]];
            let n = level.signal(host, at, back.0).unwrap_or(0);
            lines.push(Line::text(tr("jade.input_signal", "Input: %d", &[&format!("§f{n}")])));
        }
    }
    // The ender chest's contents are the asking player's own.
    if block == "minecraft:ender_chest" {
        let mine = p.entity().nbt(host).unwrap_or_default();
        storage(host, details, lines, items_of(&mine, "EnderItems").into_iter().map(|(_, id, n)| (id, n)));
        return;
    }
    // Progress on what a mod says.
    let mut progress: Vec<(f32, Option<String>)> = Vec::new();
    for f in api::registry(|r| r.progress.clone()) {
        progress.extend(f(host, req));
    }
    let Some(be) = be else {
        // FluidStorageProvider for cauldrons: water, lava or powder snow by level.
        if on(host, "fluid_storage") && (details || !on(host, "fluid_detailed")) {
            let lvl = || level.block_property(host, pos, "level").and_then(|l| l.parse::<u32>().ok()).unwrap_or(0);
            let (fluid, mb) = match block {
                "minecraft:water_cauldron" => (Some("minecraft:water"), lvl() * 1000 / 3),
                "minecraft:lava_cauldron" => (Some("minecraft:lava"), 1000),
                "minecraft:powder_snow_cauldron" => (Some("minecraft:powder_snow"), lvl() * 1000 / 3),
                _ => (None, 0),
            };
            if let Some(f) = fluid {
                lines.push(fluid_line(host, Some(f), mb, 1000, details));
            }
        }
        push_progress(progress, lines);
        return;
    };
    let kind = be.kind(host).and_then(|k| k.id(host).map(str::to_owned)).unwrap_or_default();
    // ItemStorageProvider.putData: the block's loot not rolled for anyone, or its lock, instead of its items.
    if let Some(table) = string(cget(&data, "LootTable")) {
        if on(host, "loot_table") && op {
            lines.push(Line::text(tr("jade.lootTable", "Loot Table: %s", &[&table])));
            lines.push(Line::text(tr("jade.lootTableSeed", "Seed: %s", &[&int(cget(&data, "LootTableSeed")).unwrap_or(0).to_string()])));
        }
        lines.push(Line::text(tr("jade.loot_not_generated", "§oWhat could it be?", &[])));
        return;
    }
    if !privileged && be.lock(host).is_some_and(|l| !l.is_empty()) {
        lines.push(Line::text(tr("jade.locked", "Inventory is locked", &[])));
        return;
    }
    let item = |items: &[(i64, String, u32)], slot: i64| items.iter().find(|(s, _, _)| *s == slot).map_or_else(|| Line::item("minecraft:air", 0), |(_, id, n)| Line::item(id, *n));
    let mut items_done = false;
    match kind.as_str() {
        // FurnaceProvider: input, fuel, the progress arrow, output.
        "minecraft:furnace" | "minecraft:smoker" | "minecraft:blast_furnace" => {
            let items = items_of(&data, "Items");
            if !items.is_empty() && on(host, "furnace") {
                let total = int(cget(&data, "cooking_total_time")).unwrap_or(0);
                let spent = int(cget(&data, "cooking_time_spent")).unwrap_or(0);
                let frac = if total == 0 { 0.0 } else { spent as f32 / total as f32 };
                lines.push(Line::new().with(item(&items, 0)).with(item(&items, 1)).with(El::Arrow(frac)).with(item(&items, 2)));
            }
            items_done = true;
        }
        // BrewingStandProvider: the blaze powder left and the time to brew.
        "minecraft:brewing_stand" => {
            if on(host, "brewing_stand") {
                let fuel = int(cget(&data, "Fuel")).unwrap_or(0);
                let time = int(cget(&data, "BrewTime")).unwrap_or(0);
                let mut l = Line::new().with(Line::small("minecraft:blaze_powder")).with(El::Text(format!("§f{fuel}")));
                if time > 0 {
                    l = l.with(El::Space(5.0)).with(Line::small("minecraft:clock")).with(El::Text(format!("§f{}", seconds_marker(time))));
                }
                lines.push(l);
            }
        }
        // ShelfProvider: the slot pointed at, unless Show Details lists them all.
        k if k == "minecraft:chiseled_bookshelf" || k.ends_with("shelf") => {
            let items = items_of(&data, "Items");
            if slot >= 0 && !details && on(host, "shelf")
                && let Some((_, id, _)) = items.iter().find(|(s, _, _)| *s == i64::from(slot))
            {
                lines.push(Line::new().with(El::Icon(id.clone())));
                lines.push(Line::text(format!("§f{}", item_name(id))));
                items_done = true;
            }
        }
        // CampfireProvider: what cooks, and how far along.
        "minecraft:campfire" => {
            let items = items_of(&data, "Items");
            let ints = |key: &str| match cget(&data, key) {
                Some(Tag::IntArray(v)) => v.clone(),
                _ => Vec::new(),
            };
            let (spent, total) = (ints("CookingTimes"), ints("CookingTotalTimes"));
            let mut l = Line::new();
            for (s, id, n) in &items {
                l = l.with(Line::item(id, *n));
                let i = *s as usize;
                if let (Some(t), Some(c)) = (total.get(i), spent.get(i))
                    && *t > 0
                {
                    l = l.with(El::Text(format!("§f{}", seconds_marker(i64::from((t - c).max(0))))));
                }
            }
            if !l.els.is_empty() {
                lines.push(l);
            }
            items_done = true;
        }
        // JukeboxProvider: the record.
        "minecraft:jukebox" => {
            if on(host, "jukebox") {
                match cget(&data, "RecordItem").and_then(stack_of) {
                    Some((id, _)) => lines.push(Line::new().with(Line::small(&id)).with(El::Text(format!("§f{}", item_name(&id))))),
                    None => lines.push(Line::text(format!("§7{}", tr("tooltip.jade.empty", "Empty", &[])))),
                }
            }
            items_done = true;
        }
        // LecternProvider: the book.
        "minecraft:lectern" => {
            if on(host, "lectern")
                && let Some((id, _)) = cget(&data, "Book").and_then(stack_of)
            {
                lines.push(Line::new().with(Line::small(&id)).with(El::Text(format!("§f{}", item_name(&id)))));
            }
            items_done = true;
        }
        // BeehiveProvider: the bees inside.
        "minecraft:beehive" => {
            if on(host, "beehive") {
                let bees = match cget(&data, "bees") {
                    Some(Tag::List(l)) => l.len(),
                    _ => 0,
                };
                lines.push(Line::text(tr("jade.beehive.bees", "Bees: %s", &[&format!("§f{}", tr("jade.fraction", "%d / %d", &[&bees.to_string(), "3"]))])));
            }
        }
        // CommandBlockProvider: its command, to operators only (commands can hold anything).
        "minecraft:command_block" => {
            if on(host, "command_block") && op
                && let Some(cmd) = string(cget(&data, "Command")).filter(|c| !c.is_empty())
            {
                lines.push(Line::text(format!("§f{cmd}")));
            }
        }
        // MobSpawnerProvider: the title names what it spawns.
        "minecraft:mob_spawner" => {
            if on(host, "mob_spawner")
                && let Some(id) = cget(&data, "SpawnData").and_then(|t| tget(t, "entity")).and_then(|t| string(tget(t, "id")))
            {
                lines.push(Line::new().with(El::Title(tr("jade.spawner", "%s (%s)", &["{0}", &api::entity_name(&id)]))));
            }
        }
        // MobSpawnerCooldownProvider: a trial spawner's next mob and cooldown.
        "minecraft:trial_spawner" if on(host, "mob_spawner") => {
            let next = cget(&data, "spawn_data").and_then(|t| tget(t, "entity")).and_then(|t| string(tget(t, "id")));
            if let Some(id) = next {
                lines.push(Line::new().with(El::Title(tr("jade.spawner", "%s (%s)", &["{0}", &api::entity_name(&id)]))));
            }
            let now = level.game_time(host).unwrap_or(0);
            let ends = int(cget(&data, "cooldown_ends_at")).unwrap_or(0);
            if ends > now {
                lines.push(Line::text(tr("jade.trial_spawner_cd", "Cooldown: %s", &[&format!("§f{}", seconds_marker(ends - now))])));
            }
        }
        _ => {}
    }
    // HopperLockProvider: a locked hopper's title.
    if block == "minecraft:hopper" && level.block_property(host, pos, "enabled").as_deref() == Some("false") && on(host, "redstone") {
        lines.push(Line::new().with(El::Title(tr("jade.access.block.locked", "Locked {0}", &[]))));
    }
    // The universal providers, for every block entity: energy, items, fluids, through the capabilities.
    if on(host, "energy_storage") && !hidden[1] && (details || !on(host, "energy_detailed")) {
        energy_lines(host, level, pos, lines);
    }
    if !items_done && !hidden[0] && !item_lines(host, level, pos, details, lines) {
        // A container the capability doesn't answer for: its saved items.
        storage(host, details, lines, items_of(&data, "Items").into_iter().map(|(_, id, n)| (id, n)));
    }
    if on(host, "fluid_storage") && !hidden[2] && (details || !on(host, "fluid_detailed")) {
        fluid_lines(host, level, pos, details, lines);
    }
    // A mod block with no provider: the ints `progress` and `max_progress` it saves.
    if progress.is_empty() {
        let (now, most) = (int(cget(&data, "progress")), int(cget(&data, "max_progress")));
        if let (Some(now), Some(most)) = (now, most)
            && most > 0
            && now > 0
        {
            progress.push((now as f32 / most as f32, None));
        }
    }
    push_progress(progress, lines);
}

/// ProgressProvider.Client: each bar, its text above it, small.
fn push_progress(progress: Vec<(f32, Option<String>)>, lines: &mut Vec<Line>) {
    for (frac, text) in progress {
        if let Some(text) = text {
            lines.push(Line::new().with(El::Small(text)));
        }
        lines.push(Line::new().with(El::Bar { frac: frac.clamp(0.0, 1.0), color: 0x4FB54F, text: String::new() }));
    }
}

/// `register_commands`: `/jade show|hide <targets>` (JadeServerCommand), each with its executor, and the bare `/jade`
/// opening the commands menu (`commands_menu.rs`).
pub fn register_commands(e: &mut RegisterCommandsEvent<'_>) {
    for (usage, show) in [
        ("jade show <targets:player>", true),
        ("jade hide <targets:player>", false),
        ("jade show", true),
        ("jade hide", false),
    ] {
        e.commands.push(Command::op(usage, 2).executes(move |host, source, invocation| overlay(host, source, invocation, show).ok_or_else(|| Box::new(Text::literal("")))));
    }
    e.commands.push(crate::commands_menu::command());
}

fn permitted(source: &CommandSource) -> bool {
    !matches!(source.permission(), PermissionLevel::All | PermissionLevel::Moderators)
}

/// `/jade show|hide [targets]`: tells the targets' clients (the source's player without targets) to show or hide the
/// overlay, then says how many were told.
fn overlay(host: &mut dyn ServerHost, source: &CommandSource, invocation: &CommandInvocation, show: bool) -> Option<i32> {
    if !permitted(source) {
        return None;
    }
    let targets: Vec<PlayerRef> = match invocation.argument("targets") {
        Some(CommandArgumentValue::Player(p)) => vec![*p],
        _ => match source {
            CommandSource::Player { player, .. } => vec![*player],
            _ => Vec::new(),
        },
    };
    let mut told = 0;
    for p in targets {
        let line = format!("o {}", u8::from(show));
        if p.send_payload(host, &PayloadId(NET_DATA.into()), PayloadBytes(line.into_bytes())).is_ok() {
            told += 1;
        }
    }
    let _ = host.commands().feedback(Text::literal(format!("Overlay {} for {told} players", if show { "shown" } else { "hidden" })), false);
    Some(told)
}
