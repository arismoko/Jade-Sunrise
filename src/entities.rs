//! Jade's lines for entities, built on the server: on Sunrise's client, entity handles read position, rotation, eye
//! height/position, on-ground, tick count and riding links (`vehicle`/`passengers`), and `Host::living` resolves only
//! the local player; `entity_kind`, `entity_nbt`, `entity_pose`, `entity_leash_holder`, `display`, `item_entity_stack`,
//! `merchant` and other entities' living state stay server-only. This builds VanillaPlugin's entity providers and the
//! AccessibilityPlugin's from the entity's saved data (`Entity.saveWithoutId`, what `/data get entity` shows) and the
//! living, merchant and attribute reads. Text the client must translate is written as markers.
use crate::api::{El, Line, Request, entity_name, item_name, tr};
use crate::opts::{num, on, opt, seconds_marker, tick_duration, trim2};
use crate::server::storage;
use mod_api::*;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

static EFFECT_SEQ: AtomicU64 = AtomicU64::new(1);
/// Per-(entity id, effect id): `(update_seq, last_duration, last_amplifier)` for `StatusEffectsProvider` ordering.
static EFFECT_TIMES: Mutex<Option<HashMap<(u32, String), (u64, i32, i32)>>> = Mutex::new(None);

fn next_effect_seq() -> u64 {
    EFFECT_SEQ.fetch_add(1, Ordering::Relaxed)
}

/// `mob_effect_added`: record when an effect was added or refreshed on an entity so newest effects sort first.
pub fn effect_added(host: &dyn Host, entity: Option<i32>, effect: EffectKey, amplifier: i32) {
    let Some(id) = entity.and_then(|i| u32::try_from(i).ok()) else { return };
    let Some(eff_id) = effect.id(host) else { return };
    let mut g = EFFECT_TIMES.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    if map.len() > 4096 {
        map.clear();
    }
    let seq = next_effect_seq();
    map.entry((id, eff_id.to_owned())).and_modify(|e| { e.0 = seq; e.2 = amplifier; }).or_insert((seq, 0, amplifier));
}

fn effect_order(entity_id: u32, effects: &[(ActiveEffect, String)]) -> Vec<(u64, usize)> {
    let mut g = EFFECT_TIMES.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    if map.len() > 4096 {
        map.clear();
    }
    let active: HashSet<&str> = effects.iter().map(|(_, id)| id.as_str()).collect();
    map.retain(|(eid, eff_id), _| *eid != entity_id || active.contains(eff_id.as_str()));
    let mut out = Vec::with_capacity(effects.len());
    for (idx, (eff, id)) in effects.iter().enumerate() {
        let key = (entity_id, id.clone());
        let seq = match map.get_mut(&key) {
            Some(entry) => {
                if eff.amplifier != entry.2 || (eff.duration < 0 && entry.1 >= 0) || (eff.duration > entry.1 && entry.1 > 0) {
                    entry.0 = next_effect_seq();
                }
                entry.1 = eff.duration;
                entry.2 = eff.amplifier;
                entry.0
            }
            None => {
                let seq = next_effect_seq();
                map.insert(key, (seq, eff.duration, eff.amplifier));
                seq
            }
        };
        out.push((seq, idx));
    }
    out.sort_by(|&(seq_a, ia), &(seq_b, ib)| {
        let (a, ida) = &effects[ia];
        let (b, idb) = &effects[ib];
        seq_b
            .cmp(&seq_a)
            .then_with(|| {
                // MobEffectInstance.compareTo tie-breaker: long/infinite duration, then ambient, then id.
                let long = |d: i32| d < 0 || d > 32147;
                let dur = |d: i32| if d < 0 { i32::MAX } else { d };
                match (long(a.duration), long(b.duration)) {
                    (true, true) => a.ambient.cmp(&b.ambient).then_with(|| ida.cmp(idb)),
                    (false, false) if a.ambient == b.ambient => dur(b.duration).cmp(&dur(a.duration)).then_with(|| ida.cmp(idb)),
                    _ => a.ambient.cmp(&b.ambient).then_with(|| dur(b.duration).cmp(&dur(a.duration))).then_with(|| ida.cmp(idb)),
                }
            })
    });
    out
}

fn cget<'a>(c: &'a Compound, key: &str) -> Option<&'a Tag> {
    c.iter().find(|(k, _)| k.to_string_lossy() == key).map(|(_, v)| v)
}

fn int(t: Option<&Tag>) -> Option<i64> {
    match t? {
        Tag::Byte(v) => Some(i64::from(*v)),
        Tag::Short(v) => Some(i64::from(*v)),
        Tag::Int(v) => Some(i64::from(*v)),
        Tag::Long(v) => Some(*v),
        _ => None,
    }
}

fn float(t: Option<&Tag>) -> Option<f64> {
    match t? {
        Tag::Float(v) => Some(f64::from(f32::from_bits(*v))),
        Tag::Double(v) => Some(f64::from_bits(*v)),
        other => int(Some(other)).map(|v| v as f64),
    }
}

fn string(t: Option<&Tag>) -> Option<String> {
    match t? {
        Tag::String(s) => s.to_string().ok(),
        _ => None,
    }
}

fn flag(c: &Compound, key: &str) -> bool {
    int(cget(c, key)).is_some_and(|v| v != 0)
}

fn stack_id(t: Option<&Tag>) -> Option<String> {
    let t = t?;
    let id = string(t.get("id"))?;
    (id != "minecraft:air").then_some(id)
}

fn items(c: &Compound, key: &str) -> Vec<(String, u32, Option<Compound>)> {
    let Some(Tag::List(list)) = cget(c, key) else { return Vec::new() };
    list.iter()
        .filter_map(|t| {
            let id = stack_id(Some(t))?;
            let count = int(t.get("count")).unwrap_or(1).max(0) as u32;
            let comp = match t.get("components") {
                Some(Tag::Compound(c)) if !c.is_empty() => Some(c.clone()),
                _ => None,
            };
            Some((id, count, comp))
        })
        .collect()
}

/// The living reads Jade's health line takes.
fn access(host: &dyn Host, key: &str) -> bool {
    on(host, "accessibility_plugin") && on(host, key)
}

/// EntityVariantHelper: a variant id's last part, `minecraft:red` to `red`.
fn short(id: &str) -> String {
    id.rsplit(':').next().unwrap_or(id).to_owned()
}

/// Every line Jade's own plugins give for an entity, from the server.
#[allow(clippy::too_many_arguments)]
pub fn lines(host: &mut dyn ServerHost, req: &Request, p: PlayerRef, level: LevelRef, e: EntityRef, kind: &str, nbt: &Compound, lines: &mut Vec<Line>) {
    let details = req.details;
    let living = host.living(e);
    let op = p.permission_level(host).unwrap_or(0) >= 2;
    // EntityHealthAndArmorProvider (not armour stands, not the creaking).
    if let Some(l) = living
        && !matches!(kind, "minecraft:armor_stand" | "minecraft:creaking")
    {
        let health = l.health(host).unwrap_or(0.0).max(0.0);
        let max = l.max_health(host).unwrap_or(20.0).max(1.0);
        let absorb = float(cget(nbt, "AbsorptionAmount")).unwrap_or(0.0).max(0.0) as f32;
        if on(host, "entity_health") {
            lines.push(Line::new().with(El::Hearts { health, max, absorb }));
        }
        let armor = l.attribute_value(host, "minecraft:armor").unwrap_or(0.0) as f32;
        lines.push(Line::new().with(El::Stats { absorb, armor }));
    }
    // AccessibilityPlugin's entity details: the title's patterns.
    if access(host, "access_entity") {
        let mut wrap = |pattern: String| lines.push(Line::new().with(El::Title(pattern)));
        match kind {
            "minecraft:creeper" if flag(nbt, "powered") => wrap(tr("jade.access.creeper.powered", "Charged {0}", &[])),
            "minecraft:wither" if living.is_some_and(|l| matches!((l.health(host), l.max_health(host)), (Some(h), Some(m)) if m > 0.0 && h <= m / 2.0)) => wrap(tr("jade.access.wither.powered", "Powered {0}", &[])),
            "minecraft:zombie_villager" if int(cget(nbt, "ConversionTime")).is_some_and(|t| t > 0) => wrap(tr("jade.access.zombie_villager.curing", "Curing {0}", &[])),
            "minecraft:goat" if !flag(nbt, "HasLeftHorn") && !flag(nbt, "HasRightHorn") && cget(nbt, "HasLeftHorn").is_some() => wrap(tr("jade.access.goat.hornless", "Hornless {0}", &[])),
            "minecraft:bee" => {
                if flag(nbt, "HasNectar") {
                    wrap(tr("jade.access.bee.nectar", "Nectarean {0}", &[]));
                }
                if int(cget(nbt, "anger_end_time")).is_some_and(|t| t > level.game_time(host).unwrap_or(0)) || int(cget(nbt, "AngerTime")).is_some_and(|t| t > 0) {
                    wrap(tr("jade.access.entity.angry", "Angry {0}", &[]));
                }
            }
            "minecraft:slime" | "minecraft:magma_cube" => {
                let size = int(cget(nbt, "Size")).unwrap_or(0) + 1;
                wrap(tr("jade.access.slime.size", "{1,choice,1#Small|2≤Medium|4≤Large|8≤Huge} {0}", &["{0}", &size.to_string()]));
            }
            "minecraft:copper_golem" => match string(cget(nbt, "weather_state")).as_deref() {
                Some("exposed") => wrap(tr("jade.access.entity.exposed", "Exposed {0}", &[])),
                Some("weathered") => wrap(tr("jade.access.entity.weathered", "Weathered {0}", &[])),
                Some("oxidized") => wrap(tr("jade.access.entity.oxidized", "Oxidized {0}", &[])),
                _ => {}
            },
            _ => {}
        }
        let baby = living.and_then(|l| l.baby(host)).unwrap_or(false);
        if baby {
            wrap(tr("jade.access.entity.baby", "Baby {0}", &[]));
        }
        let saddle_occupied = living.and_then(|l| l.with_equipment(host, EquipmentSlot::Saddle, |_| ())).is_some();
        match kind {
            // Copper golem's antenna slot is EquipmentSlot::Saddle; it is shearable when holding a flower there.
            "minecraft:copper_golem" => {
                if saddle_occupied {
                    wrap(tr("jade.access.entity.shearable", "Shearable {0}", &[]));
                }
            }
            "minecraft:sheep" if flag(nbt, "Sheared") || baby => wrap(tr("jade.access.entity.sheared", "Sheared {0}", &[])),
            "minecraft:mooshroom" if baby => wrap(tr("jade.access.entity.sheared", "Sheared {0}", &[])),
            "minecraft:snow_golem" if cget(nbt, "Pumpkin").is_some() && !flag(nbt, "Pumpkin") => wrap(tr("jade.access.entity.sheared", "Sheared {0}", &[])),
            "minecraft:bogged" if flag(nbt, "sheared") || flag(nbt, "Sheared") => wrap(tr("jade.access.entity.sheared", "Sheared {0}", &[])),
            _ => {}
        }
        if kind != "minecraft:copper_golem" && saddle_occupied {
            wrap(tr("jade.access.entity.saddled", "Saddled {0}", &[]));
        }
        const DYES: [&str; 16] = ["white", "orange", "magenta", "light_blue", "yellow", "lime", "pink", "gray", "light_gray", "cyan", "purple", "blue", "brown", "green", "red", "black"];
        let dye = |t: Option<&Tag>| -> Option<String> {
            match t? {
                Tag::String(s) => Some(s.to_string_lossy()),
                other => usize::try_from(int(Some(other))?).ok().and_then(|i| DYES.get(i).copied()).map(str::to_owned),
            }
        };
        let tamed = living.and_then(|l| l.is_tamed(host)).unwrap_or(false) || cget(nbt, "Owner").is_some();
        let color = match kind {
            "minecraft:sheep" | "minecraft:shulker" => dye(cget(nbt, "Color")),
            "minecraft:wolf" | "minecraft:cat" if tamed => dye(cget(nbt, "CollarColor")),
            "minecraft:tropical_fish" => int(cget(nbt, "Variant")).and_then(|v| DYES.get(((v >> 16) & 15) as usize).copied()).map(str::to_owned),
            _ => None,
        };
        if let Some(color) = color {
            let english = format!("{} {{0}}", color.split('_').map(|w| w[..1].to_uppercase() + &w[1..]).collect::<Vec<_>>().join(" "));
            wrap(tr(&format!("jade.access.entity.{color}"), &english, &[]));
        }
    }
    // NpcDescriptionProvider: a mannequin's description after its name.
    if kind == "minecraft:mannequin" && access(host, "access_npc") {
        let d = string(cget(nbt, "description")).unwrap_or_else(|| tr("entity.minecraft.mannequin.label", "NPC", &[]));
        lines.push(Line::new().with(El::Title(tr("jade.access.npc_description", "%s (%s)", &["{0}", &d]))));
    }
    // EntityVariantProvider: what a mob's variant is, plus horse markings.
    if access(host, "access_variant") {
        let variant = match kind {
            "minecraft:painting" => None,
            "minecraft:horse" => {
                const COATS: [&str; 7] = ["white", "creamy", "chestnut", "brown", "black", "gray", "dark_brown"];
                int(cget(nbt, "Variant")).and_then(|v| COATS.get((v & 0xFF) as usize).copied()).map(str::to_owned)
            }
            "minecraft:llama" | "minecraft:trader_llama" => {
                const COATS: [&str; 4] = ["creamy", "white", "brown", "gray"];
                int(cget(nbt, "Variant")).and_then(|v| COATS.get(v as usize).copied()).map(str::to_owned)
            }
            "minecraft:parrot" => {
                const PARROTS: [&str; 5] = ["red_blue", "blue", "green", "yellow_blue", "gray"];
                int(cget(nbt, "Variant")).and_then(|v| PARROTS.get(v as usize).copied()).map(str::to_owned)
            }
            "minecraft:axolotl" => {
                const AXOLOTLS: [&str; 5] = ["lucy", "wild", "gold", "cyan", "blue"];
                int(cget(nbt, "Variant")).and_then(|v| AXOLOTLS.get(v as usize).copied()).map(str::to_owned)
            }
            "minecraft:tropical_fish" => {
                const PATTERNS: [&str; 12] = ["kob", "sunstreak", "snooper", "dasher", "brinely", "spotty", "flopper", "stripey", "glitter", "blockfish", "betty", "clayfish"];
                int(cget(nbt, "Variant")).and_then(|v| PATTERNS.get((((v & 1) * 6) + ((v >> 8) & 0xFF)) as usize).copied()).map(str::to_owned)
            }
            "minecraft:fox" | "minecraft:mooshroom" => string(cget(nbt, "Type")),
            "minecraft:salmon" => string(cget(nbt, "type")),
            "minecraft:rabbit" => match int(cget(nbt, "RabbitType")) {
                Some(0) => Some("brown".to_owned()),
                Some(1) => Some("white".to_owned()),
                Some(2) => Some("black".to_owned()),
                Some(3) => Some("white_splotched".to_owned()),
                Some(4) => Some("gold".to_owned()),
                Some(5) => Some("salt".to_owned()),
                Some(99) => Some("evil".to_owned()),
                _ => None,
            },
            _ => string(cget(nbt, "variant")).or_else(|| string(cget(nbt, "Variant"))).or_else(|| cget(nbt, "VillagerData").and_then(|v| string(v.get("type")))),
        };
        if let Some(v) = variant {
            let mob = short(kind);
            let name = short(&v);
            let words = tr(&format!("jade.access.entity.{mob}.{name}"), &name.replace(['.', '_'], " "), &[]);
            lines.push(Line::text(tr("jade.access.entity.variant", "Variant: %s", &[&words])));
        }
        if kind == "minecraft:horse"
            && let Some(v) = int(cget(nbt, "Variant"))
        {
            const MARKINGS: [&str; 5] = ["none", "white", "white_field", "white_dots", "black_dots"];
            if let Some(m) = MARKINGS.get(((v >> 8) & 0xFF) as usize) {
                let label = tr(&format!("jade.access.entity.horse_markings.{m}"), &m.replace('_', " "), &[]);
                lines.push(Line::text(tr("jade.access.entity.horse_markings", "Markings: %s", &[&label])));
            }
        }
    }
    // HeldItemProvider: both hands for any living entity (including players).
    if access(host, "access_held")
        && let Some(l) = living
    {
        if let Some(item) = l.with_hand_item(host, Hand::Main, |s| s.item()) {
            lines.push(Line::text(tr("jade.access.held_item.main", "Main Hand: %s", &[&item_name(item.id(host).unwrap_or_default())])));
        }
        if let Some(item) = l.with_hand_item(host, Hand::Off, |s| s.item()) {
            lines.push(Line::text(tr("jade.access.held_item.off", "Off Hand: %s", &[&item_name(item.id(host).unwrap_or_default())])));
        }
    }
    // EntityDetailsBodyProvider: pose, leads.
    if access(host, "access_entity") {
        let pose = match (kind, e.pose(host)) {
            ("minecraft:armadillo", _) if string(cget(nbt, "state")).as_deref() == Some("rolling") => Some("1001"),
            ("minecraft:axolotl", _) if flag(nbt, "PlayingDead") => Some("1000"),
            ("minecraft:fox", _) if flag(nbt, "Sleeping") => Some("2"),
            ("minecraft:fox", _) if flag(nbt, "Sitting") => Some("10"),
            ("minecraft:camel" | "minecraft:camel_husk", _) if int(cget(nbt, "LastPoseTick")).is_some_and(|t| t < 0) => Some("10"),
            ("minecraft:panda", _) if flag(nbt, "Sitting") || int(cget(nbt, "SitAmount")).is_some_and(|v| v > 0) => Some("10"),
            (_, Some("sleeping")) => Some("2"),
            (_, Some("swimming")) => Some("3"),
            (_, Some("crouching")) => Some("5"),
            (_, Some("dying")) => Some("7"),
            (_, Some("sitting")) => Some("10"),
            _ if flag(nbt, "Sitting") || flag(nbt, "OrderedToSit") => Some("10"),
            _ => None,
        };
        if let Some(pose) = pose {
            lines.push(Line::text(tr("jade.access.entity.pose", "Pose: %s", &[&tr(&format!("jade.access.entity.pose.{pose}"), "", &[])])));
        }
        match e.leash_holder(host) {
            Some(LeashHolder::Knot(k)) => lines.push(Line::text(tr("jade.access.entity.leashed_to", "Leashed to: %s", &[&format!("{} {} {}", k[0], k[1], k[2])]))),
            Some(LeashHolder::Entity(o)) => {
                let name = o.name(host).unwrap_or_default();
                lines.push(Line::text(tr("jade.access.entity.leashed_to", "Leashed to: %s", &[&name])));
            }
            _ => {}
        }
        // Leashable.leashableLeashedTo: entities leashed to this target within 15 blocks.
        if let Some(at) = e.position(host) {
            let my_id = e.id();
            let my_block = (kind == "minecraft:leash_knot").then(|| e.block_position(host)).flatten();
            let shared: &dyn ServerHost = &*host;
            let mut leashing: Vec<String> = Vec::new();
            let min = [at[0] - 15.0, at[1] - 15.0, at[2] - 15.0];
            let max = [at[0] + 15.0, at[1] + 15.0, at[2] + 15.0];
            let _ = level.visit_entities_in(shared, min, max, &mut |other| {
                if other.id() == my_id {
                    return std::ops::ControlFlow::Continue(());
                }
                let attached = match other.leash_holder(shared) {
                    Some(LeashHolder::Entity(holder)) => holder.id() == my_id,
                    Some(LeashHolder::Knot(k)) => my_block == Some(k),
                    _ => false,
                };
                if attached {
                    let name = other.name(shared).filter(|n| !n.is_empty()).unwrap_or_else(|| {
                        let okind = other.kind(shared).and_then(|k| k.id(shared).map(|n| n.as_str().to_owned())).unwrap_or_default();
                        entity_name(&okind)
                    });
                    leashing.push(name);
                }
                std::ops::ControlFlow::Continue(())
            });
            if !leashing.is_empty() {
                lines.push(Line::text(tr("jade.access.entity.is_leashing", "is Leashing: %s", &[&leashing.join(", ")])));
            }
        }
    }
    // VillagerProfessionProvider: the profession, and its level for one with a job.
    if on(host, "villager_profession")
        && let Some(m) = host.merchant(e)
        && let Some(prof) = m.profession(host)
    {
        let pshort = short(&prof);
        let mut text = tr(&format!("entity.minecraft.villager.{pshort}"), &pshort, &[]);
        let lvl = m.level(host).unwrap_or(0);
        if !matches!(pshort.as_str(), "none" | "nitwit") && (1..=5).contains(&lvl) {
            const LEVELS: [&str; 5] = ["Novice", "Apprentice", "Journeyman", "Expert", "Master"];
            text = format!("{text} - {}", tr(&format!("merchant.level.{lvl}"), LEVELS[lvl as usize - 1], &[]));
        }
        lines.push(Line::text(format!("§f{text}")));
        // VillagerRestockProvider (off by default): ready, or when the next restock can be.
        if on(host, "villager_restock") && m.offers(host).is_some_and(|o| o.iter().any(|o| o.uses > 0)) {
            let last = int(cget(nbt, "LastRestock")).unwrap_or(0);
            let today = int(cget(nbt, "RestocksToday")).unwrap_or(0);
            let limit = today >= 2;
            let interval: i64 = if limit { 12000 } else { 2400 };
            let left = last + interval - level.game_time(host).unwrap_or(0);
            lines.push(Line::text(if left <= 0 {
                format!("§a{}", tr("jade.villagerRestock.ready", "Ready to restock", &[]))
            } else if limit {
                tr("jade.villagerRestock.limit", "Restock limit reached, next in %s", &[&format!("§f{}", seconds_marker(left))])
            } else {
                tr("jade.villagerRestock.time", "Restock in %s", &[&format!("§f{}", seconds_marker(left))])
            }));
        }
    }
    // PetArmorProvider: a pet's body armour ("Show Damageable" leaves out what wears no durability).
    if on(host, "pet_armor")
        && let Some(l) = living
        && let Some((item, comp)) = l.with_equipment(host, EquipmentSlot::Body, |s| (s.item(), s.project().and_then(|st| crate::api::stack_components(&st))))
        && (opt(host, "pet_armor_mode") != "damageable" || host.max_damage(item).unwrap_or(0) > 0)
    {
        let id = item.id(host).unwrap_or_default();
        lines.push(Line::new().with(Line::small_with(id, comp)).with(El::Text(format!("§f{}", item_name(id)))));
    }
    // StatusEffectsProvider: visible effects sorted by update time, coloured by category, 0.75 scale when > 2.
    if on(host, "potion_effects")
        && let Some(l) = living
    {
        let limit = num(host, "effects_limit").clamp(1.0, 99.0) as usize;
        let visible: Vec<(ActiveEffect, String)> = l
            .effects(host)
            .unwrap_or_default()
            .into_iter()
            .filter(|eff| eff.visible)
            .map(|eff| {
                let id = eff.id.id(host).unwrap_or_default().to_owned();
                (eff, id)
            })
            .collect();
        let ordered = effect_order(e.id(), &visible);
        let shown: Vec<&(ActiveEffect, String)> = ordered.into_iter().take(limit).map(|(_, i)| &visible[i]).collect();
        let small = shown.len() > 2;
        for (eff, id) in shown {
            let (ns, path) = id.split_once(':').unwrap_or(("minecraft", id.as_str()));
            let mut name = tr(&format!("effect.{ns}.{path}"), path, &[]);
            if eff.amplifier >= 1 {
                let lvl = eff.amplifier + 1;
                name = format!("{name} {}", tr(&format!("enchantment.level.{lvl}"), &lvl.to_string(), &[]));
            }
            let time = if eff.duration < 0 { tr("effect.duration.infinite", "∞", &[]) } else { tick_duration(i64::from(eff.duration)) };
            // MobEffect.getCategory: IThemeHelper's success, danger and info colours.
            let color = match eff.id.category(host) {
                Some(EffectCategory::Beneficial) => "§a",
                Some(EffectCategory::Harmful) => "§c",
                _ => "§f",
            };
            let text = format!("{color}{}", tr("jade.potion", "%s (%s)", &[&name, &time]));
            lines.push(if small { Line::new().with(El::Small(text)) } else { Line::text(text) });
        }
    }
    // AnimalOwnerProvider: a tamed animal's owner (a player in the world, or the uuid).
    if on(host, "animal_owner")
        && let Some(owner) = living.and_then(|l| l.owner(host))
    {
        let name = host.player(owner).and_then(|o| o.name(host).map(str::to_owned)).unwrap_or_else(|| format!("{owner:032x}"));
        lines.push(Line::text(tr("jade.owner", "Owner: %s", &[&format!("§f{name}")])));
    }
    // MobGrowthProvider: a baby's growing time (Age counts up to 0).
    // AgeableMob.age: a dropped item's `Age` is its lifetime, not this.
    let age = if living.is_some() { int(cget(nbt, "Age")).unwrap_or(0) } else { 0 };
    if on(host, "mob_growth") && age < 0 {
        let left = if flag(nbt, "AgeLocked") { tr("jade.mobgrowth.paused", "Paused", &[]) } else { seconds_marker(-age) };
        lines.push(Line::text(tr("jade.mobgrowth.time", "Growing time: %s", &[&format!("§f{left}")])));
    }
    // MobBreedingProvider: an allay's duplication cooldown; in love, or the breeding cooldown.
    if on(host, "mob_breeding") {
        if kind == "minecraft:allay" {
            let cd = int(cget(nbt, "DuplicationCooldown")).unwrap_or(0);
            if cd > 0 {
                lines.push(Line::text(tr("jade.mobduplication.time", "Duplication CD: %s", &[&format!("§f{}", seconds_marker(cd))])));
            }
        } else if living.is_some() && int(cget(nbt, "InLove")).is_some_and(|v| v > 0) {
            lines.push(Line::text(tr("jade.mobbreeding.fed", "Breeding CD: In Love", &[])));
        } else if age > 0 {
            lines.push(Line::text(tr("jade.mobbreeding.time", "Breeding CD: %s", &[&format!("§f{}", seconds_marker(age))])));
        }
    }
    // HorseStatsProvider: a llama's strength; a horse's jump height and speed (out of the most with Show Details).
    if on(host, "entity_stats")
        && let Some(l) = living
    {
        let stat = |key: &str, english: &str, v: f64, most: f64| {
            let v = trim2(v);
            let shown = if details { tr("jade.fraction", "%d / %d", &[&v, &trim2(most)]) } else { v };
            tr(key, english, &[&format!("§f{shown}")])
        };
        match kind {
            "minecraft:llama" | "minecraft:trader_llama" => lines.push(Line::text(stat("jade.llamaStrength", "Strength: %s", int(cget(nbt, "Strength")).unwrap_or(1).max(1) as f64, 5.0))),
            "minecraft:horse" | "minecraft:donkey" | "minecraft:mule" | "minecraft:skeleton_horse" | "minecraft:zombie_horse" | "minecraft:camel" => {
                let jump = |j: f64| 4.53680079 * j * j + 1.6143173 * j - 0.22656224;
                let speed = |v: f64| v * 43.17181546666666 - 3.39999999E-7;
                let j = l.attribute_value(host, "minecraft:jump_strength").unwrap_or(0.7);
                let s = l.attribute_value(host, "minecraft:movement_speed").unwrap_or(0.225);
                lines.push(Line::text(stat("jade.horseStat.jump", "Jump height: %s", jump(j), jump(1.0))));
                lines.push(Line::text(stat("jade.horseStat.speed", "Speed: %s", speed(s), speed(0.3375))));
            }
            _ => {}
        }
    }
    // ZombieVillagerProvider: the time left to cure.
    if kind == "minecraft:zombie_villager"
        && on(host, "zombie_villager")
        && let Some(t) = int(cget(nbt, "ConversionTime")).filter(|t| *t > 0)
    {
        lines.push(Line::text(tr("jade.zombieConversion.time", "Conversion time: %s", &[&format!("§f{}", seconds_marker(t))])));
    }
    // NextEntityDropProvider: a grown chicken's next egg, an armadillo's next scute.
    let grown = !living.and_then(|l| l.baby(host)).unwrap_or(false);
    if on(host, "next_entity_drop") && grown {
        match kind {
            "minecraft:chicken" => {
                if let Some(t) = int(cget(nbt, "EggLayTime")).filter(|t| *t < 48000) {
                    lines.push(Line::text(tr("jade.nextEgg", "Next egg: %s", &[&format!("§f{}", seconds_marker(t))])));
                }
            }
            "minecraft:armadillo" => {
                if let Some(t) = int(cget(nbt, "scute_time")).filter(|t| *t < 48000) {
                    lines.push(Line::text(tr("jade.nextScute", "Next scute: %s", &[&format!("§f{}", seconds_marker(t))])));
                }
            }
            "minecraft:sniffer" => {
                let ttl = cget(nbt, "Brain").and_then(|b| b.get("memories")).and_then(|m| m.get("minecraft:sniff_cooldown")).and_then(|c| int(c.get("ttl")));
                if let Some(t) = ttl.filter(|t| *t > 0 && *t < 48000) {
                    lines.push(Line::text(tr("jade.nextSniff", "Dig CD: %s", &[&format!("§f{}", seconds_marker(t))])));
                }
            }
            _ => {}
        }
    }
    // SulfurCubeProvider: what a sulfur cube swallowed.
    if kind == "minecraft:sulfur_cube"
        && on(host, "sulfur_cube")
        && let Some((item, comp)) = living.and_then(|l| l.with_equipment(host, EquipmentSlot::Head, |s| (s.item(), s.project().and_then(|st| crate::api::stack_components(&st)))))
    {
        let id = item.id(host).unwrap_or_default().to_owned();
        lines.push(Line::new().with(Line::small_with(&id, comp)).with(El::Text(format!("§f{}", item_name(&id)))));
    }
    match kind {
        // ItemFrameProvider: the framed item's name.
        "minecraft:item_frame" | "minecraft:glow_item_frame" => {
            if on(host, "item_frame")
                && let Some(id) = stack_id(cget(nbt, "Item"))
            {
                lines.push(Line::text(format!("§f{}", item_name(&id))));
            }
        }
        // PaintingProvider: the variant's title (yellow) and author (grey).
        "minecraft:painting" => {
            if on(host, "painting")
                && let Some(v) = string(cget(nbt, "variant"))
            {
                let (ns, path) = v.split_once(':').unwrap_or(("minecraft", &v));
                lines.push(Line::text(format!("§e{}", tr(&format!("painting.{ns}.{path}.title"), "", &[]))));
                lines.push(Line::text(format!("§7{}", tr(&format!("painting.{ns}.{path}.author"), "", &[]))));
                if on(host, "registry_special") {
                    lines.push(Line::text(format!("§8{}", tr("config.jade.plugin_jade.registry_name.special.painting", "Painting: %s", &[&v]))));
                }
            }
        }
        // ArmorStandProvider: each worn or held item, small, with its name.
        "minecraft:armor_stand" => {
            if on(host, "armor_stand")
                && let Some(l) = living
            {
                for slot in [EquipmentSlot::Head, EquipmentSlot::Chest, EquipmentSlot::Legs, EquipmentSlot::Feet, EquipmentSlot::OffHand, EquipmentSlot::MainHand] {
                    if let Some((item, comp)) = l.with_equipment(host, slot, |s| (s.item(), s.project().and_then(|st| crate::api::stack_components(&st)))) {
                        let id = item.id(host).unwrap_or_default();
                        lines.push(Line::new().with(Line::small_with(id, comp)).with(El::Text(format!("§f{}", item_name(id)))));
                    }
                }
            }
        }
        // ContainerEntityProvider: a chest minecart's or boat's items, or its loot not rolled yet.
        k if k.ends_with("minecart") || k.ends_with("chest_boat") || k.ends_with("chest_raft") => {
            if cget(nbt, "LootTable").is_some() {
                lines.push(Line::text(tr("jade.loot_not_generated", "§oWhat could it be?", &[])));
            } else if on(host, "container_entity") {
                storage(host, details, lines, items(nbt, "Items"));
            }
            // MobSpawnerProvider.ForEntity.
            if on(host, "mob_spawner")
                && let Some(id) = cget(nbt, "SpawnData").and_then(|t| t.get("entity")).and_then(|t| string(t.get("id")))
            {
                lines.push(Line::new().with(El::Title(tr("jade.spawner", "%s (%s)", &["{0}", &entity_name(&id)]))));
            }
        }
        _ => {}
    }
    // ItemStorageProvider through AbstractHorseAccess: a chested horse's or llama's items.
    if flag(nbt, "ChestedHorse") {
        storage(host, details, lines, items(nbt, "Items"));
    }
    // EntityLootTableProvider: for game masters.
    if on(host, "loot_table") && op && living.is_some() && kind != "minecraft:player" {
        let table = string(cget(nbt, "DeathLootTable")).unwrap_or_else(|| {
            let (ns, path) = kind.split_once(':').unwrap_or(("minecraft", kind));
            format!("{ns}:entities/{path}")
        });
        lines.push(Line::text(tr("jade.lootTable", "Loot Table: %s", &[&table])));
    }
    // WaxedProvider.EntityComponent: a waxed copper golem shows as its statue.
    if kind == "minecraft:copper_golem" && on(host, "waxed") && int(cget(nbt, "next_weather_age")).is_some_and(|v| v == -2) {
        let weather = match string(cget(nbt, "weather_state")).as_deref() {
            Some("exposed") => "exposed_",
            Some("weathered") => "weathered_",
            Some("oxidized") => "oxidized_",
            _ => "",
        };
        let mut l = Line::new().with(El::Icon(format!("minecraft:waxed_{weather}copper_golem_statue"))).with(El::Badge);
        if access(host, "access_entity") {
            l = l.with(El::Title(tr("jade.access.waxed", "Waxed {0}", &[])));
        }
        lines.push(l);
    }
    let _ = p;
}
