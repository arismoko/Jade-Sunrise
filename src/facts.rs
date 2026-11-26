//! The block facts Jade's tooltips read, asked of the game at runtime (no vanilla data ships with the mod):
//!
//! - hardness, `requiresCorrectToolForDrops` and explosion resistance through `host.block_states()`, read
//!   once per block (its default state, as Java's `BlockBehaviour.Properties` are per block) and kept until the player
//!   leaves the world;
//! - the blocks shears harvest (`LootTableMineableCollector`): the server reads its own loot tables as they load
//!   (`loot_table_load`, read only) and sends the list to each client with its settings answer, as Java's
//!   `ServerHandshakePacket.shearableBlocks` does.
//!
//! The speed and jump factors come from `BlockStates::speed_factor` and `jump_factor` (the block's declared values).
//! Fire's ignite and burn odds have no read in the mod API yet, so their `BlockPropertiesProvider` lines are left out
//! (see jade-facts.md).

use mod_api::serde_json::Value;
use mod_api::*;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// What `BlockPropertiesProvider` and `HarvestToolProvider` read of a block.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Fact {
    /// `BlockBehaviour.Properties.destroyTime` (`getDestroySpeed`): -1 unbreakable.
    pub hardness: f32,
    /// `BlockState.requiresCorrectToolForDrops()`.
    pub requires_tool: bool,
    /// `Block.getExplosionResistance()`.
    pub explosion_resistance: Option<f32>,
    /// `Block.getSpeedFactor()`: 1.0 for most blocks, 0.4 for soul sand and honey.
    pub speed_factor: Option<f32>,
    /// `Block.getJumpFactor()`: 1.0 for most blocks, 0.5 for honey.
    pub jump_factor: Option<f32>,
}

thread_local! {
    /// Facts by block id, read once each.
    static FACTS: std::cell::RefCell<HashMap<String, Fact>> = std::cell::RefCell::new(HashMap::new());
}

/// A block's facts by its id, from its default state. None while the block's key or state cannot be resolved
/// (nothing is kept then, so it is asked again).
pub(crate) fn fact(h: &dyn Host, block: &str) -> Option<Fact> {
    if let Some(f) = FACTS.with_borrow(|m| m.get(block).copied()) {
        return Some(f);
    }
    let state = h
        .block_state_id(block)
        .map(|s| s.block())
        .and_then(|k| h.block_state_default(k))?;
    let states = h.block_states();
    let f = Fact {
        hardness: states.destroy_speed(state)?,
        requires_tool: states.requires_correct_tool(state).unwrap_or(false),
        explosion_resistance: states.explosion_resistance(state),
        speed_factor: states.speed_factor(state),
        jump_factor: states.jump_factor(state),
    };
    FACTS.with_borrow_mut(|m| m.insert(block.to_owned(), f));
    Some(f)
}

// ---- Shearable blocks (LootTableMineableCollector) ----

/// What one loot table says about shears: the item specs of the `match_tool` conditions plain shears could pass,
/// and the tables it nests by id.
#[derive(Default, Debug, Clone, PartialEq)]
struct TableScan {
    specs: Vec<Spec>,
    nested: Vec<String>,
}

/// A `match_tool` predicate's `items` that shears may pass.
#[derive(Debug, Clone, PartialEq)]
enum Spec {
    /// No `items`: any item.
    Any,
    /// `#tag`, judged when sent (tags are known then).
    Tag(String),
}

#[derive(Default)]
struct Server {
    tables: HashMap<String, TableScan>,
    last: String,
}

static SERVER: Mutex<Option<Server>> = Mutex::new(None);

const SHEARS: &str = "minecraft:shears";

/// `loot_table_load` on the server: note what the table asks of shears. Tables come once each in id order per load,
/// so an id not after the last one starts a new load.
pub(crate) fn loot_table_loaded(id: &str, table: &Value) {
    let scan = scan_table(table);
    let mut guard = SERVER.lock().unwrap_or_else(|e| e.into_inner());
    let s = guard.get_or_insert_with(Server::default);
    if id <= s.last.as_str() {
        s.tables.clear();
    }
    s.last = id.to_owned();
    if scan != TableScan::default() {
        s.tables.insert(id.to_owned(), scan);
    }
}

fn scan_table(table: &Value) -> TableScan {
    let mut out = TableScan::default();
    for pool in table
        .get("pools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for entry in pool
            .get("entries")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            scan_entry(entry, &mut out, 0);
        }
    }
    out
}

/// `doLootPoolEntry`: the entry's conditions, an alternatives entry's children, a nested table.
fn scan_entry(entry: &Value, out: &mut TableScan, depth: u32) {
    if depth > 16 {
        return;
    }
    for c in entry
        .get("conditions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        scan_condition(c, out, depth + 1);
    }
    let kind = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind.strip_prefix("minecraft:").unwrap_or(kind) {
        "alternatives" => {
            for child in entry
                .get("children")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                scan_entry(child, out, depth + 1);
            }
        }
        "loot_table" => match entry.get("value") {
            Some(Value::String(id)) => out.nested.push(id.clone()),
            Some(inline @ Value::Object(_)) => {
                let inner = scan_table(inline);
                out.specs.extend(inner.specs);
                out.nested.extend(inner.nested);
            }
            _ => {}
        },
        _ => {}
    }
}

/// `isCorrectConditions`: a `match_tool` plain shears pass (or any of an `any_of`'s terms).
fn scan_condition(c: &Value, out: &mut TableScan, depth: u32) {
    if depth > 16 {
        return;
    }
    let kind = c
        .get("condition")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind.strip_prefix("minecraft:").unwrap_or(kind) {
        "match_tool" => {
            if let Some(spec) = c.get("predicate").map_or(Some(Spec::Any), shears_spec) {
                out.specs.push(spec);
            }
        }
        "any_of" => {
            for t in c
                .get("terms")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                scan_condition(t, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// Whether a default stack of shears can pass an item predicate: its `items` name shears (or a tag, judged later, or
/// are absent) and it asks nothing a plain stack lacks (a count above one, enchantments, components).
fn shears_spec(p: &Value) -> Option<Spec> {
    let obj = p.as_object()?;
    for (k, v) in obj {
        match k.as_str() {
            "items" => {}
            "count" => {
                let one = match v {
                    Value::Number(n) => n.as_f64() == Some(1.0),
                    Value::Object(r) => {
                        r.get("min")
                            .and_then(Value::as_f64)
                            .is_none_or(|m| m <= 1.0)
                            && r.get("max")
                                .and_then(Value::as_f64)
                                .is_none_or(|m| m >= 1.0)
                    }
                    _ => false,
                };
                if !one {
                    return None;
                }
            }
            _ => return None,
        }
    }
    match obj.get("items") {
        None => Some(Spec::Any),
        Some(Value::String(s)) => match s.strip_prefix('#') {
            Some(tag) => Some(Spec::Tag(tag.to_owned())),
            None => (full(s) == SHEARS).then_some(Spec::Any),
        },
        Some(Value::Array(ids)) => ids
            .iter()
            .filter_map(Value::as_str)
            .any(|s| full(s) == SHEARS)
            .then_some(Spec::Any),
        _ => None,
    }
}

fn full(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// The server's answer naming the blocks shears harvest: `h <block> <block>...`, the blocks whose `<ns>:blocks/<path>`
/// table asks for shears, and the tripwire as Java adds it.
pub(crate) fn shears_message(host: &dyn Host) -> String {
    let guard = SERVER.lock().unwrap_or_else(|e| e.into_inner());
    let tables = guard.as_ref().map(|s| &s.tables);
    let shears = Identifier::parse(SHEARS)
        .ok()
        .and_then(|id| host.item_key(&id));
    let tags = host.tags();
    let pass = |s: &Spec| match s {
        Spec::Any => true,
        Spec::Tag(t) => {
            shears
                .zip(
                    Identifier::parse(&full(t))
                        .ok()
                        .and_then(|id| tags.key(&id)),
                )
                .and_then(|(k, tag)| tags.is_in_tag(k, tag.typed()))
                == Some(true)
        }
    };
    let mut memo: HashMap<String, bool> = HashMap::new();
    let mut blocks: Vec<String> = Vec::new();
    if let Some(tables) = tables {
        for id in tables.keys() {
            let Some((ns, path)) = id.split_once(':') else {
                continue;
            };
            let Some(block) = path.strip_prefix("blocks/") else {
                continue;
            };
            let block = format!("{ns}:{block}");
            if host.block_state_id(&block).is_some() && asks_shears(tables, id, &pass, &mut memo, 0)
            {
                blocks.push(block);
            }
        }
    }
    blocks.push("minecraft:tripwire".to_owned());
    blocks.sort();
    blocks.dedup();
    format!("h {}", blocks.join(" "))
}

fn asks_shears(
    tables: &HashMap<String, TableScan>,
    id: &str,
    pass: &dyn Fn(&Spec) -> bool,
    memo: &mut HashMap<String, bool>,
    depth: u32,
) -> bool {
    if let Some(&b) = memo.get(id) {
        return b;
    }
    // Cut cycles: a table being judged counts as no for itself.
    memo.insert(id.to_owned(), false);
    let b = depth < 16
        && tables.get(id).is_some_and(|t| {
            t.specs.iter().any(pass)
                || t.nested
                    .iter()
                    .any(|n| asks_shears(tables, &full(n), pass, memo, depth + 1))
        });
    memo.insert(id.to_owned(), b);
    b
}

/// The client's copy of the server's list, and how many lists it has had (for caches keyed by it).
static CLIENT: Mutex<(u32, Option<HashSet<String>>)> = Mutex::new((0, None));

/// The client took a `h ...` answer.
pub(crate) fn shears_received(rest: &str) {
    let set: HashSet<String> = rest
        .split(' ')
        .filter(|s| !s.is_empty())
        .take(1 << 16)
        .map(str::to_owned)
        .collect();
    let mut c = CLIENT.lock().unwrap_or_else(|e| e.into_inner());
    c.0 = c.0.wrapping_add(1);
    c.1 = Some(set);
}

/// Whether the server named `block` as one shears harvest. Without a Jade server, the tripwire alone, as Java's
/// default shears tier.
pub(crate) fn shearable(block: &str) -> bool {
    let c = CLIENT.lock().unwrap_or_else(|e| e.into_inner());
    match &c.1 {
        Some(set) => set.contains(block),
        None => block == "minecraft:tripwire",
    }
}

/// Changes whenever the shears list does.
pub(crate) fn shears_generation() -> u32 {
    CLIENT.lock().unwrap_or_else(|e| e.into_inner()).0
}

/// The player left the world: its server's list and the facts read there go.
pub(crate) fn forget_client() {
    let mut c = CLIENT.lock().unwrap_or_else(|e| e.into_inner());
    c.0 = c.0.wrapping_add(1);
    c.1 = None;
    drop(c);
    FACTS.with_borrow_mut(HashMap::clear);
}

#[cfg(test)]
mod tests {
    use super::*;
    use mod_api::serde_json::json;

    #[test]
    fn short_grass_asks_shears() {
        let t = json!({"pools": [{"entries": [{"type": "minecraft:alternatives", "children": [
            {"type": "minecraft:item", "name": "minecraft:short_grass", "conditions": [{"condition": "minecraft:match_tool", "predicate": {"items": "minecraft:shears"}}]},
            {"type": "minecraft:item", "name": "minecraft:wheat_seeds", "conditions": [{"condition": "minecraft:random_chance", "chance": 0.125}]}
        ]}]}]});
        assert_eq!(scan_table(&t).specs, vec![Spec::Any]);
    }

    #[test]
    fn leaves_any_of_shears_or_silk() {
        let t = json!({"pools": [{"entries": [{"type": "alternatives", "children": [
            {"type": "item", "name": "minecraft:oak_leaves", "conditions": [{"condition": "any_of", "terms": [
                {"condition": "match_tool", "predicate": {"items": ["minecraft:shears"]}},
                {"condition": "match_tool", "predicate": {"predicates": {"minecraft:enchantments": [{"enchantments": "minecraft:silk_touch"}]}}}
            ]}]}
        ]}]}]});
        assert_eq!(scan_table(&t).specs, vec![Spec::Any]);
    }

    #[test]
    fn silk_touch_and_pickaxes_do_not_count() {
        let t = json!({"pools": [{"entries": [
            {"type": "item", "name": "minecraft:glass", "conditions": [{"condition": "match_tool", "predicate": {"predicates": {"minecraft:enchantments": [{"enchantments": "minecraft:silk_touch"}]}}}]},
            {"type": "item", "name": "minecraft:stone", "conditions": [{"condition": "match_tool", "predicate": {"items": "minecraft:diamond_pickaxe"}}]}
        ]}]});
        assert_eq!(scan_table(&t), TableScan::default());
    }

    #[test]
    fn tags_and_nested_tables() {
        let t = json!({"pools": [{"entries": [
            {"type": "item", "conditions": [{"condition": "match_tool", "predicate": {"items": "#c:tools/shear"}}]},
            {"type": "loot_table", "value": "mymod:blocks/shared"}
        ]}]});
        let s = scan_table(&t);
        assert_eq!(s.specs, vec![Spec::Tag("c:tools/shear".into())]);
        assert_eq!(s.nested, vec!["mymod:blocks/shared".to_owned()]);
        let mut tables = HashMap::new();
        tables.insert(
            "mymod:blocks/a".to_owned(),
            TableScan {
                specs: vec![],
                nested: vec!["mymod:blocks/b".into()],
            },
        );
        tables.insert(
            "mymod:blocks/b".to_owned(),
            TableScan {
                specs: vec![Spec::Any],
                nested: vec!["mymod:blocks/a".into()],
            },
        );
        let mut memo = HashMap::new();
        assert!(asks_shears(
            &tables,
            "mymod:blocks/a",
            &|s| *s == Spec::Any,
            &mut memo,
            0
        ));
    }

    #[test]
    fn a_new_load_starts_over() {
        let shears = json!({"pools": [{"entries": [{"type": "item", "conditions": [{"condition": "match_tool", "predicate": {"items": "minecraft:shears"}}]}]}]});
        loot_table_loaded("minecraft:blocks/vine", &shears);
        loot_table_loaded("minecraft:blocks/zz", &json!({}));
        loot_table_loaded("minecraft:blocks/fern", &shears);
        let guard = SERVER.lock().unwrap();
        let keys: Vec<&String> = guard.as_ref().unwrap().tables.keys().collect();
        assert_eq!(keys, vec!["minecraft:blocks/fern"]);
    }
}
