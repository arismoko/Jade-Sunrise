//! Jade's provider API for other mods (IWailaPlugin's registrations, as this port has them).
//!
//! Another mod adds lines to the overlay by naming `jade` as an optional dependency in its `sunrise-mod.toml`
//! (`[dependencies] jade = { version = "^26", optional = true }`) and registering its providers from any hook that has
//! a host, behind `mod_api::if_installed!(host, jade, { ... })`. Registering is idempotent, so a mod can do it in each
//! `client_setup` and `server_starting` it gets. See the README's "Provider API" section.
//!
//! Where a provider runs decides what it can read:
//! - a client provider (`register_client`) runs on the client, every frame, with what the client knows: the block
//!   state and the block entity data the server syncs;
//! - a server provider (`register_server`, `register_server_entity`) runs on the server while it answers a player's
//!   request (a few times a second while they look), with the whole world. Its lines travel to that player. Server
//!   text can't be translated where it is made (a server has no language): write [`tr`] markers, which the client
//!   resolves in its own language.
//!
//! Energy, fluids and items need no provider: Jade reads every block's `ENERGY`, `FLUIDS` and `ITEMS` capabilities
//! itself (EnergyStorageProvider, FluidStorageProvider, ItemStorageProvider), so any mod's FE buffer, tank or
//! slots show up. [`hide_storage`] leaves a mod's blocks out of those lines when it draws its own.
use mod_api::*;
use std::sync::Mutex;

/// One of a line's elements (JadeUI.text, item, smallItem, progressArrow, a progress bar, HealthElement, ArmorElement,
/// spacer).
#[derive(Clone, Debug, PartialEq)]
pub enum El {
    /// Text at three quarters (Mod Name: Smaller, ProgressView's label).
    Small(String),
    /// Text, `§` codes and `§#RRGGBB` colours allowed, [`tr`] markers resolved on the client.
    Text(String),
    /// An item's icon: `small` at half size (JadeUI.smallItem), else 16 with its count.
    Item { id: String, count: u32, small: bool },
    /// The progress arrow (JadeUI.progressArrow), 0 to 1.
    Arrow(f32),
    /// A progress bar (energy, ProgressView), 0 to 1, its colour `0xRRGGBB` and the text on it.
    Bar { frac: f32, color: u32, text: String },
    /// A fluid icon drawn via `DrawContext::fluid`.
    Fluid(String),
    /// A fluid progress bar filled with the fluid's own look (`DrawContext::fluid`).
    FluidBar { fluid: String, frac: f32, text: String },
    Hearts { health: f32, max: f32, absorb: f32 },
    Armor(f32),
    Space(f32),
    /// Jade's energy icon (`jade:energy`).
    Energy,
    /// The tick or cross after the harvest tool's icon.
    Mark(String),
    /// Not drawn: the box's icon becomes this item (IComponentProvider.getIcon).
    Icon(String),
    /// Not drawn: a honeycomb over the icon (WaxedProvider.waxedIcon).
    Badge,
    /// Not drawn: the title with `{0}` as what it is now (ITooltip.replace(CORE_OBJECT_NAME, ...)).
    Title(String),
    /// Not drawn: EntityHealthAndArmorProvider's absorption and armour from the server.
    Stats { absorb: f32, armor: f32 },
}

/// One line of the box: its elements left to right.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line {
    pub els: Vec<El>,
}

impl Line {
    /// A line of text.
    pub fn text(t: impl Into<String>) -> Self {
        Self { els: vec![El::Text(t.into())] }
    }
    pub fn new() -> Self {
        Self::default()
    }
    /// The line with one more element.
    pub fn with(mut self, e: El) -> Self {
        self.els.push(e);
        self
    }
    /// An item's icon with its count.
    pub fn item(id: &str, count: u32) -> El {
        El::Item { id: id.to_owned(), count, small: false }
    }
    /// An item's small icon (JadeUI.smallItem).
    pub fn small(id: &str) -> El {
        El::Item { id: id.to_owned(), count: 1, small: true }
    }
}

const SEP: char = '\u{1f}';
const CMD: char = '\u{1}';

/// A line as text: elements apart by `\u{1f}`, each but text starting `\u{1}`. What crosses the network.
pub fn encode(line: &Line) -> String {
    let clean = |t: &str| t.replace([CMD, SEP, '\n'], " ");
    let parts: Vec<String> = line
        .els
        .iter()
        .map(|e| match e {
            El::Text(t) => clean(t),
            El::Small(t) => format!("{CMD}m {}", clean(t)),
            El::Mark(t) => format!("{CMD}k {}", clean(t)),
            El::Icon(id) => format!("{CMD}I {id}"),
            El::Badge => format!("{CMD}W"),
            El::Title(t) => format!("{CMD}T {}", clean(t)),
            El::Stats { absorb, armor } => format!("{CMD}S {absorb} {armor}"),
            El::Item { id, count, small } => format!("{CMD}i {id} {count} {}", u8::from(*small)),
            El::Arrow(f) => format!("{CMD}a {f}"),
            El::Bar { frac, color, text } => format!("{CMD}b {frac} {color} {}", clean(text)),
            El::Fluid(f) => format!("{CMD}f {}", clean(f)),
            El::FluidBar { fluid, frac, text } => format!("{CMD}F {frac} {} {}", clean(fluid), clean(text)),
            El::Hearts { health, max, absorb } => format!("{CMD}h {health} {max} {absorb}"),
            El::Armor(a) => format!("{CMD}r {a}"),
            El::Space(w) => format!("{CMD}s {w}"),
            El::Energy => format!("{CMD}e"),
        })
        .collect();
    parts.join(&SEP.to_string())
}

/// `encode`'s inverse. Unknown elements are left out, so a newer server's lines still read.
pub fn decode(s: &str) -> Line {
    let mut line = Line::new();
    for part in s.split(SEP) {
        let Some(cmd) = part.strip_prefix(CMD) else {
            if !part.is_empty() {
                line.els.push(El::Text(part.to_owned()));
            }
            continue;
        };
        let w: Vec<&str> = cmd.splitn(4, ' ').collect();
        let f = |i: usize| w.get(i).and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite()).unwrap_or(0.0);
        let rest = || cmd.get(2..).unwrap_or("").to_owned();
        match w.first().copied() {
            Some("i") => line.els.push(El::Item {
                id: w.get(1).copied().unwrap_or("minecraft:air").to_owned(),
                count: f(2).max(0.0) as u32,
                small: w.get(3) == Some(&"1"),
            }),
            Some("a") => line.els.push(El::Arrow(f(1))),
            Some("b") => line.els.push(El::Bar { frac: f(1), color: f(2) as u32, text: w.get(3).copied().unwrap_or("").to_owned() }),
            Some("f") => line.els.push(El::Fluid(rest())),
            Some("F") => line.els.push(El::FluidBar { fluid: w.get(2).copied().unwrap_or("").to_owned(), frac: f(1), text: w.get(3).copied().unwrap_or("").to_owned() }),
            Some("h") => line.els.push(El::Hearts { health: f(1), max: f(2), absorb: f(3) }),
            Some("r") => line.els.push(El::Armor(f(1))),
            Some("s") => line.els.push(El::Space(f(1).clamp(0.0, 200.0))),
            Some("e") => line.els.push(El::Energy),
            Some("m") => line.els.push(El::Small(rest())),
            Some("k") => line.els.push(El::Mark(rest())),
            Some("I") => line.els.push(El::Icon(w.get(1).copied().unwrap_or("minecraft:air").to_owned())),
            Some("W") => line.els.push(El::Badge),
            Some("T") => line.els.push(El::Title(rest())),
            Some("S") => line.els.push(El::Stats { absorb: f(1), armor: f(2) }),
            _ => {}
        }
    }
    line
}

pub(crate) const TR_OPEN: char = '\u{2}';
pub(crate) const TR_SEP: char = '\u{3}';
pub(crate) const TR_CLOSE: char = '\u{4}';

/// `Component.translatable(key, args)` as text: a marker the client replaces with the pattern of `key` in its own
/// language (`fallback` when it has none), `%s`, `%d`, `%1$s` and `{0}` filled with `args`. Args may hold markers too.
/// What a server provider writes where a Java one would write a translatable component.
pub fn tr(key: &str, fallback: &str, args: &[&str]) -> String {
    let clean = |t: &str| t.replace([TR_SEP, '\n'], " ");
    let mut out = String::new();
    out.push(TR_OPEN);
    out.push_str(&clean(key));
    out.push(TR_SEP);
    out.push_str(&clean(fallback));
    for a in args {
        out.push(TR_SEP);
        out.push_str(&clean_arg(a));
    }
    out.push(TR_CLOSE);
    out
}

/// An argument as a marker carries it: a separator outside a nested marker can't stand, one inside belongs to it.
fn clean_arg(a: &str) -> String {
    let mut depth = 0u32;
    a.chars()
        .map(|c| match c {
            TR_OPEN => {
                depth += 1;
                c
            }
            TR_CLOSE => {
                depth = depth.saturating_sub(1);
                c
            }
            TR_SEP if depth == 0 => ' ',
            '\n' => ' ',
            _ => c,
        })
        .collect()
}

/// An item's name (its hover name, `item.<ns>.<path>` or `block.<ns>.<path>`) as a marker the client resolves.
pub fn item_name(id: &str) -> String {
    tr("@item", id, &[id])
}

/// An entity type's name (`entity.<ns>.<path>`) as a marker the client resolves.
pub fn entity_name(id: &str) -> String {
    tr("@entity", id, &[id])
}

/// A fluid's name (`ClientHost::fluid_name`) as a marker the client resolves.
pub fn fluid_name(id: &str) -> String {
    tr("@fluid", id, &[id])
}

/// What the player looks at.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A block (or a fluid): its position, its id (`minecraft:chest`; what it passes for under Jade's built-in
    /// camouflage), its state as `minecraft:chest[facing=north,...]`, and where the crosshair meets it.
    Block { pos: [i32; 3], block: String, state: String, hit: [f64; 3] },
    /// An entity: its network id and type (`minecraft:zombie`), once the server has said what it is.
    Entity { id: u32, kind: String },
}

/// A request the server answers: what the asking player looks at, and how.
#[derive(Clone, Debug)]
pub struct Request {
    /// The asking player.
    pub player: u128,
    /// Their dimension (`minecraft:overworld`).
    pub dimension: String,
    /// The block (for an entity request, the block the entity stands in).
    pub pos: [i32; 3],
    /// The block's id, as the server sees it.
    pub block: String,
    /// The entity, for an entity request.
    pub entity: Option<u32>,
    /// Show Details is held.
    pub details: bool,
    /// Where the crosshair meets the block, as the client said (the server checked it is near the block).
    pub hit: Option<[f64; 3]>,
}

/// A plugin's lines on the client (IBlockComponentProvider / IEntityComponentProvider.appendTooltip).
pub type ClientProvider = fn(&dyn ClientHost, &Target, &mut Vec<Line>);
/// A plugin's lines from the server's state for a block (IServerDataProvider<BlockAccessor> with its client half).
pub type ServerProvider = fn(&mut dyn ServerHost, &Request, &mut Vec<Line>);
/// The same for an entity (IServerDataProvider<EntityAccessor>).
pub type ServerEntityProvider = fn(&mut dyn ServerHost, &Request, EntityRef, &mut Vec<Line>);
/// IServerExtensionProvider<ProgressView.Data>: a machine's progress bars, how far along (0 to 1) and the text above
/// the bar.
pub type ProgressProvider = fn(&mut dyn ServerHost, &Request) -> Vec<(f32, Option<String>)>;
/// A plugin's name for a block target (IComponentProvider.getIcon and the title it replaces): its icon's item, title
/// and mod namespace. The first provider to answer wins.
pub type NameProvider = fn(&dyn ClientHost, &Target) -> Option<(String, String, String)>;
/// A recipe viewer's lookup (JEICompat): show the recipes (`uses` false) or the uses of an item. True when shown.
pub type RecipeLookup = fn(&mut dyn ClientHost, &str, bool) -> bool;

pub(crate) struct Registry {
    pub client: Vec<ClientProvider>,
    pub server: Vec<ServerProvider>,
    pub server_entity: Vec<ServerEntityProvider>,
    pub progress: Vec<ProgressProvider>,
    pub names: Vec<NameProvider>,
    /// key, label, default.
    pub options: Vec<(&'static str, &'static str, bool)>,
    /// Namespace or block id: items, energy, fluids left out.
    pub hidden: Vec<(&'static str, [bool; 3])>,
    pub mod_names: Vec<(&'static str, &'static str)>,
    pub lookup: Option<RecipeLookup>,
}

pub(crate) static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    client: Vec::new(),
    server: Vec::new(),
    server_entity: Vec::new(),
    progress: Vec::new(),
    names: Vec::new(),
    options: Vec::new(),
    hidden: Vec::new(),
    mod_names: Vec::new(),
    lookup: None,
});

pub(crate) fn registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let mut guard = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

fn add<T: Copy>(list: &mut Vec<T>, f: T, same: impl Fn(&T, &T) -> bool) {
    if !list.iter().any(|g| same(g, &f)) {
        list.push(f);
    }
}

/// Adds a client provider (once; calling again does nothing).
pub fn register_client(f: ClientProvider) {
    registry(|r| add(&mut r.client, f, |a, b| *a as usize == *b as usize));
}

/// Adds a server provider for blocks (once).
pub fn register_server(f: ServerProvider) {
    registry(|r| add(&mut r.server, f, |a, b| *a as usize == *b as usize));
}

/// Adds a server provider for entities (once).
pub fn register_server_entity(f: ServerEntityProvider) {
    registry(|r| add(&mut r.server_entity, f, |a, b| *a as usize == *b as usize));
}

/// `registerProgress`: adds a progress provider (once). A mod block with no provider still gets a bar from the ints
/// `progress` and `max_progress` its block entity saves.
pub fn register_progress(f: ProgressProvider) {
    registry(|r| add(&mut r.progress, f, |a, b| *a as usize == *b as usize));
}

/// Adds a name provider (once).
pub fn register_name(f: NameProvider) {
    registry(|r| add(&mut r.names, f, |a, b| *a as usize == *b as usize));
}

/// IWailaClientRegistration.addConfig: an on/off option in Plugin Settings under the mod's name, kept with Jade's
/// settings under `key` (once). Pick a key no other mod does: `plugin_<modid>.<name>` (`plugin_ae2.power_storage`).
/// The server reads it for the player who asked ([`plugin_on`]).
pub fn register_option(key: &'static str, label: &'static str, default_on: bool) {
    let added = registry(|r| {
        let new = !r.options.iter().any(|(k, _, _)| *k == key);
        if new {
            r.options.push((key, label, default_on));
        }
        new
    });
    if added {
        crate::opts::settings_changed();
    }
}

/// A plugin's option. In a server provider this answers for the player who asked (their request names the toggles
/// they have off); elsewhere it is this game's own setting.
pub fn plugin_on(host: &dyn Host, key: &str) -> bool {
    crate::opts::on(host, key)
}

/// While a server provider answers a block request: where the player's crosshair meets the block.
pub fn request_hit() -> Option<[f64; 3]> {
    crate::server::request_hit()
}

/// BodyProviderAdapter / the `hide` of Jade's universal providers: blocks whose id starts with `prefix` (a namespace
/// `ae2:` or one block id) show no ItemStorageProvider, EnergyStorageProvider or FluidStorageProvider lines for the
/// kinds marked true, for a mod that draws them itself.
pub fn hide_storage(prefix: &'static str, items: bool, energy: bool, fluids: bool) {
    registry(|r| {
        if !r.hidden.iter().any(|(p, _)| *p == prefix) {
            r.hidden.push((prefix, [items, energy, fluids]));
        }
    });
}

/// The name Jade shows under the box for a mod's namespace (ModIdentification), when the language has no
/// `jade.modName.<namespace>`: `register_mod_name("ironfurnaces", "Iron Furnaces")`.
pub fn register_mod_name(namespace: &'static str, name: &'static str) {
    registry(|r| {
        if !r.mod_names.iter().any(|(n, _)| *n == namespace) {
            r.mod_names.push((namespace, name));
        }
    });
}

/// A recipe viewer's lookup, for Jade's Show Recipes and Show Uses keys (JEICompat). The last one set wins.
pub fn set_recipe_lookup(f: RecipeLookup) {
    registry(|r| r.lookup = Some(f));
}

pub(crate) fn hidden_storage(block: &str) -> [bool; 3] {
    registry(|r| {
        let mut out = [false; 3];
        for (prefix, kinds) in &r.hidden {
            if block.starts_with(prefix) {
                for i in 0..3 {
                    out[i] |= kinds[i];
                }
            }
        }
        out
    })
}
