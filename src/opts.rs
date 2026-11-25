//! Jade's settings (WailaConfig and the plugin options), their names in the jar's language files, and the store: the
//! file `config/jade/jade.json` as Java keeps it, through Sunrise's config files service. Profiles are
//! `config/jade/profile-<n>.json`, the ignore lists `config/jade/hide-blocks.json` and `hide-entities.json`, and a
//! server's forced values `config/jade/server-plugin-overrides.json` (`/jadec generate_server_overrides`).
//!
//! A server answering a player reads that player's own values (`with_player`), which their client sends when they
//! change; the server's overrides win over them.
use mod_api::serde_json::{self, Value};
use mod_api::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

pub const ID: &str = "jade";

/// Bumped when a setting changes.
static EPOCH: AtomicU32 = AtomicU32::new(1);

pub fn settings_epoch() -> u32 {
    EPOCH.load(Ordering::Relaxed)
}

pub fn settings_changed() {
    EPOCH.fetch_add(1, Ordering::Relaxed);
}

pub(crate) struct Store {
    loaded: bool,
    values: BTreeMap<String, String>,
    overrides: BTreeMap<String, String>,
    hide_blocks: Vec<String>,
    hide_entities: Vec<String>,
}

static STORE: Mutex<Store> = Mutex::new(Store {
    loaded: false,
    values: BTreeMap::new(),
    overrides: BTreeMap::new(),
    hide_blocks: Vec::new(),
    hide_entities: Vec::new(),
});

fn store<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    let mut g = STORE.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut g)
}

fn path(host: &dyn Host, name: &str) -> std::path::PathBuf {
    host.config_dir().join("jade").join(name)
}

/// A config file's JSON object, or None.
/// A JSON object, wrapped: a type of another crate can't stand in a function's signature (each function is an
/// injection point, and the points file names types by their crate).
pub(crate) struct JsonObject(pub serde_json::Map<String, Value>);

/// A JSON value maybe there, wrapped for the same reason.
#[derive(Clone, Copy)]
pub(crate) struct JsonRef<'a>(pub Option<&'a Value>);

pub fn read_json(host: &dyn Host, name: &str) -> Option<JsonObject> {
    let bytes = match std::fs::read(path(host, name)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("jade: can't read config/jade/{name}: {e:?}");
            return None;
        }
    };
    match serde_json::from_slice::<Value>(&bytes).ok()? {
        Value::Object(m) => Some(JsonObject(m)),
        _ => None,
    }
}

fn write_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

pub fn write_json(host: &dyn Host, name: &str, map: &JsonObject) -> bool {
    let text = serde_json::to_string_pretty(&Value::Object(map.0.clone())).unwrap_or_default();
    match write_file(&path(host, name), text.as_bytes()) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("jade: can't write config/jade/{name}: {e:?}");
            false
        }
    }
}

fn as_string(v: JsonRef<'_>) -> Option<String> {
    match v.0? {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

const HIDE_BLOCKS: &[&str] = &[
    "minecraft:barrier",
    "minecraft:light",
    "minecraft:structure_void",
];
const HIDE_ENTITIES: &[&str] = &[
    "minecraft:area_effect_cloud",
    "minecraft:firework_rocket",
    "minecraft:interaction",
    "minecraft:text_display",
    "minecraft:lightning_bolt",
];

/// Reads the files once (and again after `reload`): a missing jade.json or ignore list is written with its defaults.
fn ensure(host: &dyn Host) {
    if store(|s| s.loaded) {
        return;
    }
    let values: BTreeMap<String, String> = read_json(host, "jade.json")
        .map(|m| {
            m.0.iter()
                .filter_map(|(k, v)| Some((k.clone(), as_string(JsonRef(Some(v)))?)))
                .collect()
        })
        .unwrap_or_default();
    let overrides: BTreeMap<String, String> = read_json(host, "server-plugin-overrides.json")
        .map(|m| {
            m.0.iter()
                .filter_map(|(k, v)| Some((k.clone(), as_string(JsonRef(Some(v)))?)))
                .collect()
        })
        .unwrap_or_default();
    let list = |name: &str, defaults: &[&str]| -> Vec<String> {
        match read_json(host, name) {
            Some(m) => {
                m.0.get("values")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            }
            None => {
                let mut m = serde_json::Map::new();
                m.insert("__comment".to_owned(), Value::String("This is an ignore list for the target of Jade. You can add registry ids to the \"values\" list.".to_owned()));
                m.insert(
                    "values".to_owned(),
                    Value::Array(
                        defaults
                            .iter()
                            .map(|d| Value::String((*d).to_owned()))
                            .collect(),
                    ),
                );
                write_json(host, name, &JsonObject(m));
                defaults.iter().map(|d| (*d).to_owned()).collect()
            }
        }
    };
    let hide_blocks = list("hide-blocks.json", HIDE_BLOCKS);
    let hide_entities = list("hide-entities.json", HIDE_ENTITIES);
    store(|s| {
        s.values = values;
        s.overrides = overrides;
        s.hide_blocks = hide_blocks;
        s.hide_entities = hide_entities;
        s.loaded = true;
    });
    settings_changed();
}

/// Reload Plugins: the files are read again when next asked.
pub fn reload() {
    store(|s| s.loaded = false);
    settings_changed();
}

fn save(host: &dyn Host) {
    let map: serde_json::Map<String, Value> = store(|s| {
        s.values
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect()
    });
    write_json(host, "jade.json", &JsonObject(map));
}

/// The ignore lists: blocks, then entities (`#tag` entries for blocks).
pub fn ignore_lists(host: &dyn Host) -> (Vec<String>, Vec<String>) {
    ensure(host);
    store(|s| (s.hide_blocks.clone(), s.hide_entities.clone()))
}

thread_local! {
    /// While the server answers a player: that player's values.
    static PLAYER: RefCell<Option<HashMap<String, String>>> = const { RefCell::new(None) };
}

/// Each player's values as their client sent them (`set_player_values`).
static PLAYERS: Mutex<Option<HashMap<u128, HashMap<String, String>>>> = Mutex::new(None);

/// A client's values: only known keys, short values, and at most this many.
pub fn set_player_values(player: u128, text: &str) {
    let known = |k: &str| {
        option(k).is_some() || crate::api::registry(|r| r.options.iter().any(|(o, _, _)| *o == k))
    };
    let map: HashMap<String, String> = text
        .split('\u{1f}')
        .filter_map(|kv| kv.split_once('='))
        .filter(|(k, v)| v.len() <= 48 && known(k))
        .take(512)
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    let mut g = PLAYERS.lock().unwrap_or_else(|e| e.into_inner());
    let all = g.get_or_insert_with(HashMap::new);
    if all.len() > 1024 && !all.contains_key(&player) {
        all.clear();
    }
    all.insert(player, map);
}

pub fn forget_player(player: u128) {
    if let Some(all) = PLAYERS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        all.remove(&player);
    }
}

/// `f` with `player`'s values as `opt` and `on` read them.
pub fn with_player<R>(player: u128, f: impl FnOnce() -> R) -> R {
    let values = PLAYERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|all| all.get(&player).cloned())
        .unwrap_or_default();
    PLAYER.with_borrow_mut(|p| *p = Some(values));
    let out = f();
    PLAYER.with_borrow_mut(|p| *p = None);
    out
}

/// What this game's client sends the server: every option's value (`k=v` apart by `\u{1f}`).
pub fn values_for_server(host: &dyn Host) -> String {
    let mut out = Vec::new();
    for o in OPTIONS {
        out.push(format!("{}={}", o.key, opt(host, o.key)));
    }
    for (k, _, _) in crate::api::registry(|r| r.options.clone()) {
        out.push(format!("{k}={}", opt(host, k)));
    }
    out.join("\u{1f}")
}

fn default_of(key: &str) -> Option<String> {
    if let Some(o) = option(key) {
        return Some(o.default.to_owned());
    }
    crate::api::registry(|r| {
        r.options
            .iter()
            .find(|(k, _, _)| *k == key)
            .map(|(_, _, d)| d.to_string())
    })
}

/// A setting's value now: the server's override, else (answering a player) that player's, else this game's, else
/// its default.
pub fn opt(host: &dyn Host, key: &str) -> String {
    ensure(host);
    if let Some(v) = store(|s| s.overrides.get(key).cloned()) {
        return v;
    }
    if let Some(v) = PLAYER.with_borrow(|p| p.as_ref().map(|m| m.get(key).cloned())) {
        return v.or_else(|| default_of(key)).unwrap_or_default();
    }
    store(|s| s.values.get(key).cloned())
        .or_else(|| default_of(key))
        .unwrap_or_default()
}

/// What this game keeps for `key`, if anything (not its default).
pub fn stored(host: &dyn Host, key: &str) -> Option<String> {
    ensure(host);
    store(|s| s.values.get(key).cloned())
}

pub fn on(host: &dyn Host, key: &str) -> bool {
    opt(host, key) != "false"
}

pub fn num(host: &dyn Host, key: &str) -> f64 {
    opt(host, key).parse().unwrap_or(0.0)
}

/// Keeps a value and writes the file.
pub fn set(host: &dyn Host, key: &str, value: &str) {
    ensure(host);
    store(|s| s.values.insert(key.to_owned(), value.to_owned()));
    save(host);
    settings_changed();
}

/// What a slider or a dragged overlay says while it moves: read back at once, no file written. `commit` keeps it.
pub fn set_live(host: &dyn Host, key: &str, value: String) {
    ensure(host);
    store(|s| s.values.insert(key.to_owned(), value));
    settings_changed();
}

pub fn commit(host: &dyn Host) {
    save(host);
    settings_changed();
}

/// Jade.getProfilePreset: what a profile starts as besides the defaults, by its number.
fn profile_preset(profile: &str) -> &'static [(&'static str, &'static str)] {
    match profile {
        // jade.profile_preset.accessibility.
        "1" => &[
            ("accessibility_plugin", "true"),
            ("always_list_mode", "true"),
            ("animation", "false"),
            ("overlay_alpha", "100"),
            ("block_face", "true"),
            ("mod_name", "false"),
        ],
        // jade.profile_preset.minimalism.
        "2" => &[
            ("display_mode", "lite"),
            ("boss_bar_overlap", "hide_tip"),
            ("overlay_alpha", "0"),
            ("overlay_theme", "dark"),
            ("theme_style", "slim"),
            ("display_item", "inline"),
            ("breaking_progress", "false"),
            ("harvest_tool", "false"),
            ("item_tooltip", "false"),
        ],
        _ => &[],
    }
}

fn profile_keys() -> Vec<String> {
    let mut keys: Vec<String> = OPTIONS
        .iter()
        .filter(|o| o.key != "profile" && o.key != "enable_profiles")
        .map(|o| o.key.to_owned())
        .collect();
    keys.extend(crate::api::registry(|r| {
        r.options
            .iter()
            .map(|(k, _, _)| (*k).to_owned())
            .collect::<Vec<_>>()
    }));
    keys
}

/// Jade.saveProfile: this game's settings, kept as profile `n`'s (`config/jade/profile-<n>.json`).
pub fn save_profile(host: &dyn Host, n: &str) {
    let map: serde_json::Map<String, Value> = profile_keys()
        .into_iter()
        .map(|k| {
            let v = opt(host, &k);
            (k, Value::String(v))
        })
        .collect();
    write_json(host, &format!("profile-{n}.json"), &JsonObject(map));
}

/// Profiles: leaving one keeps its settings, entering one brings its own back; one never used starts from the
/// defaults and its preset.
fn switch_profile(host: &dyn Host, from: &str, to: &str) {
    save_profile(host, from);
    let saved = read_json(host, &format!("profile-{to}.json"));
    store(|s| {
        for k in profile_keys() {
            s.values.remove(&k);
        }
        match &saved {
            Some(m) => {
                for (k, v) in &m.0 {
                    if let Some(v) = as_string(JsonRef(Some(v))) {
                        s.values.insert(k.clone(), v);
                    }
                }
            }
            None => {
                for (k, v) in profile_preset(to) {
                    s.values.insert((*k).to_owned(), (*v).to_owned());
                }
            }
        }
    });
}

/// `/jadec use_profile` and the profile keys (Jade.useProfile).
pub fn use_profile(host: &dyn Host, n: &str) {
    let from = opt(host, "profile");
    if on(host, "enable_profiles") {
        switch_profile(host, &from, n);
    }
    set(host, "profile", n);
}

/// One step of an option's cycle; `profile` moves between profiles.
pub fn set_cycled(host: &dyn Host, key: &str, value: &str) {
    if key == "profile" && on(host, "enable_profiles") {
        let from = opt(host, "profile");
        switch_profile(host, &from, value);
    }
    set(host, key, value);
}

/// Jade.resetConfig: every option back to its default.
pub fn reset(host: &dyn Host) {
    store(|s| {
        for k in profile_keys() {
            s.values.remove(&k);
        }
        s.values.remove("profile");
        s.values.remove("enable_profiles");
    });
    save(host);
    settings_changed();
}

/// `/jadec generate_server_overrides [pattern]`: the matching options' values now, as the server's forced ones.
pub fn generate_overrides(host: &dyn Host, pattern: &str) -> Option<usize> {
    let wanted: serde_json::Map<String, Value> = OPTIONS
        .iter()
        .filter(|o| pattern.is_empty() || o.key.contains(pattern))
        .map(|o| (o.key.to_owned(), Value::String(opt(host, o.key))))
        .collect();
    if wanted.is_empty() {
        return None;
    }
    let n = wanted.len();
    write_json(host, "server-plugin-overrides.json", &JsonObject(wanted));
    reload();
    Some(n)
}

/// `/jadec handlers`: the options listed in `config/jade/handlers.md`.
pub fn dump_handlers(host: &dyn Host) -> bool {
    let text: Vec<String> = OPTIONS
        .iter()
        .map(|o| format!("- `{}`: {} (default {})", o.key, o.label, o.default))
        .collect();
    write_file(&path(host, "handlers.md"), text.join("\n").as_bytes()).is_ok()
}

/// The options a plugin added: key, label and default, in the order they came.
pub fn plugin_options() -> Vec<(&'static str, &'static str, bool)> {
    crate::api::registry(|r| r.options.clone())
}

/// The language's text for a key (client), else `fallback`.
pub fn tr(h: &dyn ClientHost, key: &str, fallback: &str) -> String {
    h.translate(key).unwrap_or_else(|| fallback.to_owned())
}

pub fn trf(h: &dyn ClientHost, key: &str, fallback: &str, args: &[&str]) -> String {
    fill(&tr(h, key, fallback), args)
}

/// An option's title in the game's language.
pub fn option_label(h: &dyn ClientHost, o: &Opt) -> String {
    match option_lang_key(o.key) {
        Some(key) => tr(h, &key, o.label),
        None => o.label.to_owned(),
    }
}

/// An option's value as its button says it (`Custom` for a dragged position).
pub fn value_label(h: &dyn ClientHost, o: &Opt) -> String {
    match o.key {
        "overlay_theme" => return crate::theme::main_label(h),
        "theme_style" => return crate::theme::style_label(h),
        _ => {}
    }
    let value = opt(h, o.key);
    if value.starts_with("custom:") {
        return "Custom".to_owned();
    }
    let english = o
        .values
        .iter()
        .find(|(v, _)| *v == value)
        .map_or(value.as_str(), |(_, l)| *l);
    match value_lang_key(o.key, &value) {
        Some(key) => tr(h, &key, english),
        None => english.to_owned(),
    }
}

/// What a value of an option is called in the jar's language files (`OptionsList.choices`: the option's key and the
/// enum's name).
fn value_lang_key(option: &str, value: &str) -> Option<String> {
    let enum_name = match (option, value) {
        (_, "true") => return Some("options.on".to_owned()),
        (_, "false") => return Some("options.off".to_owned()),
        ("reach_distance", "0") => return Some("options.off".to_owned()),
        ("display_mode", "hold") => "hold_key",
        (
            "display_mode" | "display_fluids" | "display_item" | "tts_mode" | "perspective_mode",
            v,
        ) => v,
        ("boss_bar_overlap", "none") => "no_operation",
        ("boss_bar_overlap", "hide_bar") => "hide_boss_bar",
        ("boss_bar_overlap", "hide_tip") => "hide_tooltip",
        ("boss_bar_overlap", "push") => "push_down",
        ("registry_name", "advanced") => "advanced_tooltips",
        ("pet_armor_mode", "damageable") => {
            return Some("config.jade.plugin_minecraft.pet_armor_show_damageable".to_owned());
        }
        ("pet_armor_mode", "all") => {
            return Some("config.jade.plugin_minecraft.pet_armor_show_all".to_owned());
        }
        ("energy_style" | "fluid_style", "bar") => "progress_bar",
        ("energy_style" | "fluid_style", "plain") => "plain_text",
        ("energy_style" | "fluid_style", "icon") => "icon",
        ("profile", "1") => return Some("jade.profile_preset.accessibility".to_owned()),
        ("profile", "2") => return Some("jade.profile_preset.minimalism".to_owned()),
        _ => return None,
    };
    Some(format!("{}_{enum_name}", option_lang_key(option)?))
}

/// One of Jade's settings: the file it is kept in, its label, its default and its values with their labels.
pub struct Opt {
    pub key: &'static str,
    pub label: &'static str,
    pub default: &'static str,
    pub values: &'static [(&'static str, &'static str)],
}

/// One of Jade's own settings, by its key.
pub fn option(key: &str) -> Option<&'static Opt> {
    OPTIONS.iter().find(|o| o.key == key)
}

const ONOFF: &[(&str, &str)] = &[("true", "On"), ("false", "Off")];
const fn on_by_default(key: &'static str, label: &'static str) -> Opt {
    Opt {
        key,
        label,
        default: "true",
        values: ONOFF,
    }
}
const fn off_by_default(key: &'static str, label: &'static str) -> Opt {
    Opt {
        key,
        label,
        default: "false",
        values: ONOFF,
    }
}

/// WailaConfig and the plugin options, in the order of Jade's settings screen.
pub const OPTIONS: &[Opt] = &[
    on_by_default("display_tooltip", "Enabled"),
    Opt {
        key: "display_mode",
        label: "Display Mode",
        default: "toggle",
        values: &[("toggle", "Toggle"), ("hold", "Hold"), ("lite", "Lite")],
    },
    on_by_default("display_blocks", "Display Blocks"),
    on_by_default("display_entities", "Display Entities"),
    on_by_default("display_bosses", "Display Bosses"),
    Opt {
        key: "display_fluids",
        label: "Display Fluids",
        default: "none",
        values: &[("none", "None"), ("any", "Any"), ("fallback", "Fallback")],
    },
    on_by_default("hide_from_tab_list", "Hide from Player List"),
    on_by_default("hide_from_guis", "Hide from GUIs"),
    Opt {
        key: "boss_bar_overlap",
        label: "Boss Bar Overlap",
        default: "push",
        values: &[
            ("none", "No Operation"),
            ("hide_bar", "Hide Boss Bar"),
            ("hide_tip", "Hide Tooltip"),
            ("push", "Push Down Tooltip"),
        ],
    },
    Opt {
        key: "display_item",
        label: "Icon Position",
        default: "top",
        values: &[
            ("hide", "Hide"),
            ("top", "Top"),
            ("centered", "Centered"),
            ("inline", "In-line"),
        ],
    },
    Opt {
        key: "tts_mode",
        label: "Text to Speech Mode",
        default: "press",
        values: &[("press", "Press"), ("toggle", "Toggle")],
    },
    Opt {
        key: "perspective_mode",
        label: "Perspective Mode",
        default: "camera",
        values: &[("camera", "From Camera"), ("eye", "From Eye")],
    },
    Opt {
        key: "reach_distance",
        label: "Extended Reach",
        default: "0",
        values: &[
            ("0", "Off"),
            ("5", "5"),
            ("10", "10"),
            ("15", "15"),
            ("20", "20"),
        ],
    },
    on_by_default("item_mod_name", "Mod Name on Item Tooltip"),
    Opt {
        key: "overlay_x",
        label: "Overlay Position X",
        default: "center",
        values: &[("left", "Left"), ("center", "Center"), ("right", "Right")],
    },
    Opt {
        key: "position",
        label: "Overlay Position Y",
        default: "0",
        values: &[
            ("0", "Top"),
            ("1", "Upper"),
            ("2", "Middle"),
            ("3", "Bottom"),
        ],
    },
    Opt {
        key: "scale",
        label: "Display Scale",
        default: "1",
        values: &[
            ("0.5", "0.5"),
            ("0.75", "0.75"),
            ("1", "1"),
            ("1.25", "1.25"),
            ("1.5", "1.5"),
        ],
    },
    Opt {
        key: "text_background_opacity",
        label: "Text Background Opacity",
        default: "0",
        values: &[
            ("0", "0%"),
            ("25", "25%"),
            ("50", "50%"),
            ("75", "75%"),
            ("100", "100%"),
        ],
    },
    off_by_default("flip_main_hand", "Flip Position with Main Hand"),
    Opt {
        key: "overlay_alpha",
        label: "Background Opacity",
        default: "70",
        values: &[
            ("0", "0%"),
            ("25", "25%"),
            ("50", "50%"),
            ("70", "70%"),
            ("85", "85%"),
            ("100", "100%"),
        ],
    },
    Opt {
        key: "overlay_theme",
        label: "Theme",
        default: "dark",
        values: &[
            ("dark", "Dark"),
            ("waila", "Classic"),
            ("create", "Create"),
            ("top", "One Probe"),
        ],
    },
    Opt {
        key: "theme_style",
        label: "Theme Style",
        default: "default",
        values: &[("default", "Default"), ("slim", "Slim")],
    },
    on_by_default("animation", "Overlay Animation"),
    off_by_default("narrate_keys", "Narrate Key Binds"),
    off_by_default("enable_profiles", "Profiles Enabled"),
    Opt {
        key: "profile",
        label: "Profile",
        default: "0",
        values: &[
            ("0", "#0"),
            ("1", "#1 Accessibility Mode"),
            ("2", "#2 Minimalism"),
            ("3", "#3"),
        ],
    },
    off_by_default("debug_mode", "Debug Mode"),
    // WailaConfig.General.builtinCamouflage (no screen in Jade; in the file there).
    on_by_default("builtin_camouflage", "Built-in Camouflage"),
    // Core Features.
    on_by_default("object_name", "Target Name"),
    on_by_default("mod_name", "Mod Name"),
    off_by_default("mod_name_smaller", "Mod Name: Smaller"),
    on_by_default("mod_name_translated", "Translate Mod Name"),
    off_by_default("distance", "Distance"),
    off_by_default("coordinates", "Coordinates"),
    off_by_default("coordinates_rel", "Relative Coordinates"),
    off_by_default("block_face", "Block Face"),
    Opt {
        key: "registry_name",
        label: "Registry Name",
        default: "false",
        values: &[
            ("false", "Off"),
            ("true", "On"),
            ("advanced", "Advanced Tooltips"),
        ],
    },
    off_by_default("registry_special", "Special IDs"),
    off_by_default("block_states", "Block States"),
    off_by_default("block_properties", "Block Properties"),
    off_by_default("loot_table", "Loot Table"),
    // Minecraft - Entity.
    on_by_default("entity_health", "Entity Health"),
    Opt {
        key: "health_max",
        label: "Max Health for Render",
        default: "40",
        values: &[("20", "20"), ("40", "40"), ("60", "60"), ("100", "100")],
    },
    Opt {
        key: "health_per_line",
        label: "Icons per Line",
        default: "10",
        values: &[("5", "5"), ("10", "10"), ("15", "15"), ("20", "20")],
    },
    off_by_default("health_fractions", "Show Fractions"),
    on_by_default("entity_armor", "Entity Armor"),
    Opt {
        key: "armor_max",
        label: "Max Armors for Render",
        default: "20",
        values: &[("10", "10"), ("20", "20"), ("40", "40")],
    },
    on_by_default("entity_stats", "Horse Stats"),
    on_by_default("potion_effects", "Entity Effects"),
    Opt {
        key: "effects_limit",
        label: "Effects Limit",
        default: "5",
        values: &[
            ("1", "1"),
            ("3", "3"),
            ("5", "5"),
            ("10", "10"),
            ("99", "No limit"),
        ],
    },
    on_by_default("item_frame", "Item Frame"),
    on_by_default("container_entity", "Container Entity"),
    on_by_default("mob_growth", "Mob Growth"),
    on_by_default("mob_breeding", "Mob Breeding"),
    on_by_default("armor_stand", "Armor Stand"),
    on_by_default("painting", "Painting"),
    on_by_default("next_entity_drop", "Next Entity Drop"),
    on_by_default("animal_owner", "Animal Owner"),
    on_by_default("sulfur_cube", "Sulfur Cube"),
    on_by_default("villager_profession", "Villager Profession"),
    off_by_default("villager_restock", "Villager Restock"),
    on_by_default("zombie_villager", "Zombie Villager Conversion"),
    on_by_default("pet_armor", "Pet Armor"),
    Opt {
        key: "pet_armor_mode",
        label: "Pet Armor Shows",
        default: "damageable",
        values: &[("damageable", "Show Damageable"), ("all", "Show All")],
    },
    on_by_default("item_tooltip", "Item Description"),
    // Minecraft - Block.
    on_by_default("breaking_progress", "Breaking Progress"),
    on_by_default("harvest_tool", "Harvest Tool"),
    off_by_default("harvest_new_line", "Harvest Tool: Start in New Line"),
    on_by_default("harvest_effective", "Harvest Tool: Effective Tool"),
    off_by_default("harvest_unbreakable", "Harvest Tool: Show \"Unbreakable\""),
    off_by_default("harvest_creative", "Harvest Tool: Show in Creative Mode"),
    on_by_default("brewing_stand", "Brewing Stand"),
    on_by_default("tnt_stability", "TNT Stability"),
    on_by_default("beehive", "Beehive"),
    on_by_default("note_block", "Note Block"),
    on_by_default("command_block", "Command Block"),
    on_by_default("enchantment_power", "Enchantment Power"),
    on_by_default("total_enchantment_power", "Total Enchantment Power"),
    on_by_default("player_head", "Player Head"),
    on_by_default("item_storage", "Item Storage"),
    Opt {
        key: "storage_detailed",
        label: "Item Storage: Detailed Amount",
        default: "54",
        values: &[("27", "27"), ("54", "54"), ("108", "108")],
    },
    Opt {
        key: "storage_normal",
        label: "Item Storage: Normal Amount",
        default: "9",
        values: &[
            ("0", "0"),
            ("9", "9"),
            ("18", "18"),
            ("27", "27"),
            ("54", "54"),
        ],
    },
    Opt {
        key: "storage_names",
        label: "Item Storage: Show Name Threshold",
        default: "5",
        values: &[("0", "0"), ("2", "2"), ("5", "5"), ("9", "9")],
    },
    Opt {
        key: "storage_per_line",
        label: "Item Storage: Items per Line",
        default: "9",
        values: &[("5", "5"), ("9", "9"), ("12", "12")],
    },
    off_by_default("storage_sort", "Item Storage: Sort by Count"),
    on_by_default("energy_storage", "Energy Storage"),
    off_by_default("energy_detailed", "Energy Storage: Detailed-Only"),
    Opt {
        key: "energy_style",
        label: "Energy Storage: Display Style",
        default: "bar",
        values: &[
            ("bar", "Progress Bar"),
            ("plain", "Plain Text"),
            ("icon", "Icon"),
        ],
    },
    on_by_default("fluid_storage", "Fluid Storage"),
    off_by_default("fluid_detailed", "Fluid Storage: Detailed-Only"),
    Opt {
        key: "fluid_style",
        label: "Fluid Storage: Display Style",
        default: "bar",
        values: &[
            ("bar", "Progress Bar"),
            ("plain", "Plain Text"),
            ("icon", "Icon"),
        ],
    },
    on_by_default("lectern", "Lectern"),
    on_by_default("furnace", "Furnace Contents"),
    on_by_default("mob_spawner", "Mob Spawner"),
    on_by_default("crop_progress", "Crop Progress"),
    on_by_default("redstone", "Redstone Info"),
    on_by_default("jukebox", "Jukebox Record"),
    on_by_default("shelf", "Shelf"),
    on_by_default("waxed", "Waxed State"),
    // Accessibility.
    // AccessorClientHandler: without the Accessibility Plugin none of the access providers run.
    off_by_default("accessibility_plugin", "Accessibility Plugin"),
    off_by_default("always_list_mode", "Always Use List Mode"),
    on_by_default("access_sign", "Sign Content"),
    on_by_default("access_block", "Block Details"),
    on_by_default("access_entity", "Entity Details"),
    on_by_default("access_variant", "Entity Variant"),
    on_by_default("access_held", "Entity Held Item"),
    on_by_default("access_npc", "NPC Description"),
];

/// A language file's pattern with its arguments: `%s`, `%d`, `%1$s` and `%%` (Component.translatable, String.format),
/// `{0}`, `{1,choice,...}` and `{1,select,...}` (JadeClient.formatString's MessageFormat; `select` always takes `other`, as
/// this port has no JadeLanguages name classes). What an argument says isn't read again.
pub fn fill(pattern: &str, args: &[&str]) -> String {
    let arg = |n: usize| args.get(n).copied().unwrap_or("");
    let c: Vec<char> = pattern.chars().collect();
    let (mut out, mut i, mut next) = (String::new(), 0, 0);
    while i < c.len() {
        if c[i] == '%' {
            let digits = c[i + 1..].iter().take_while(|d| d.is_ascii_digit()).count();
            let numbered = digits > 0 && c.get(i + 1 + digits) == Some(&'$');
            let at = if numbered { i + 2 + digits } else { i + 1 };
            match c.get(at) {
                Some('%') if !numbered => out.push('%'),
                Some('s' | 'd') if numbered => {
                    let n: usize = c[i + 1..i + 1 + digits]
                        .iter()
                        .collect::<String>()
                        .parse()
                        .unwrap_or(1);
                    out.push_str(arg(n.saturating_sub(1)));
                }
                Some('s' | 'd') => {
                    out.push_str(arg(next));
                    next += 1;
                }
                _ => {
                    out.push('%');
                    i += 1;
                    continue;
                }
            }
            i = at + 1;
        } else if c[i] == '{' {
            let mut depth = 0;
            let close = c[i..].iter().position(|ch| {
                depth += match ch {
                    '{' => 1,
                    '}' => -1,
                    _ => 0,
                };
                depth == 0
            });
            let Some(close) = close else {
                out.push('{');
                i += 1;
                continue;
            };
            let body: String = c[i + 1..i + close].iter().collect();
            let mut parts = body.splitn(3, ',');
            let number = parts.next().and_then(|n| n.trim().parse::<usize>().ok());
            match (number, parts.next().map(str::trim), parts.next()) {
                (Some(n), None, _) => out.push_str(arg(n)),
                (Some(n), Some("choice"), Some(choices)) => {
                    out.push_str(choice(choices, arg(n).parse::<f64>().unwrap_or(0.0)))
                }
                (Some(_), Some("select"), Some(cases)) => out.push_str(
                    cases
                        .rsplit_once("other {")
                        .map_or("", |(_, rest)| rest.strip_suffix('}').unwrap_or(rest)),
                ),
                _ => {
                    out.push('{');
                    out.push_str(&body);
                    out.push('}');
                }
            }
            i += close + 1;
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

/// ChoiceFormat: the last choice whose limit the number reaches (`1#`, `2≤`, `1.0<`), else the first.
fn choice(choices: &str, number: f64) -> &str {
    let mut chosen = "";
    for (i, part) in choices.split('|').enumerate() {
        let Some((at, mark)) = part
            .char_indices()
            .find(|(_, m)| matches!(m, '#' | '≤' | '<'))
        else {
            continue;
        };
        let limit: f64 = part[..at].trim().parse().unwrap_or(0.0);
        if i == 0 || number > limit || (mark != '<' && number == limit) {
            chosen = &part[at + mark.len_utf8()..];
        }
    }
    chosen
}

/// An option's key in the jar's language files (`OptionsList.Entry.makeKey`, `plugin_<id>` for a provider's). None for the
/// few this port added, which have no such key.
pub fn option_lang_key(key: &str) -> Option<String> {
    let minecraft = |path: &str| Some(format!("config.jade.plugin_minecraft.{path}"));
    let core = |path: &str| Some(format!("config.jade.plugin_jade.{path}"));
    match key {
        "display_tooltip"
        | "display_mode"
        | "display_blocks"
        | "display_entities"
        | "display_bosses"
        | "display_fluids"
        | "hide_from_tab_list"
        | "hide_from_guis"
        | "boss_bar_overlap"
        | "display_item"
        | "tts_mode"
        | "perspective_mode"
        | "reach_distance"
        | "item_mod_name"
        | "text_background_opacity"
        | "flip_main_hand"
        | "overlay_alpha"
        | "overlay_theme"
        | "theme_style"
        | "animation"
        | "narrate_keys"
        | "enable_profiles"
        | "debug_mode"
        | "accessibility_plugin"
        | "always_list_mode" => Some(format!("config.jade.{key}")),
        "scale" => Some("config.jade.overlay_scale".to_owned()),
        "object_name" | "mod_name" | "mod_name_smaller" | "distance" | "coordinates"
        | "block_face" | "registry_name" | "block_states" | "block_properties" | "loot_table" => {
            core(key)
        }
        "mod_name_translated" => core("mod_name.translated"),
        "coordinates_rel" => core("coordinates.rel"),
        "registry_special" => core("registry_name.special"),
        "entity_health"
        | "entity_armor"
        | "potion_effects"
        | "item_frame"
        | "container_entity"
        | "mob_growth"
        | "mob_breeding"
        | "armor_stand"
        | "painting"
        | "next_entity_drop"
        | "animal_owner"
        | "sulfur_cube"
        | "villager_profession"
        | "villager_restock"
        | "zombie_villager"
        | "pet_armor"
        | "item_tooltip"
        | "breaking_progress"
        | "harvest_tool"
        | "brewing_stand"
        | "tnt_stability"
        | "beehive"
        | "note_block"
        | "command_block"
        | "enchantment_power"
        | "total_enchantment_power"
        | "player_head"
        | "item_storage"
        | "energy_storage"
        | "fluid_storage"
        | "lectern"
        | "furnace"
        | "mob_spawner"
        | "crop_progress"
        | "redstone"
        | "jukebox"
        | "shelf"
        | "waxed" => minecraft(key),
        "health_max" => minecraft("entity_health.max_for_render"),
        "health_per_line" => minecraft("entity_health.icons_per_line"),
        "health_fractions" => minecraft("entity_health.show_fractions"),
        "armor_max" => minecraft("entity_armor.max_for_render"),
        "entity_stats" => minecraft("horse_stats"),
        "effects_limit" => minecraft("potion_effects.limit"),
        "harvest_new_line" => minecraft("harvest_tool.new_line"),
        "harvest_effective" => minecraft("harvest_tool.effective_tool"),
        "harvest_unbreakable" => minecraft("harvest_tool.show_unbreakable"),
        "harvest_creative" => minecraft("harvest_tool.creative"),
        "storage_detailed" => minecraft("item_storage.detailed_amount"),
        "storage_normal" => minecraft("item_storage.normal_amount"),
        "storage_names" => minecraft("item_storage.show_name_amount"),
        "storage_per_line" => minecraft("item_storage.items_per_line"),
        "storage_sort" => minecraft("item_storage.sort"),
        "energy_detailed" => minecraft("energy_storage.detailed"),
        "energy_style" => minecraft("energy_storage.style"),
        "fluid_detailed" => minecraft("fluid_storage.detailed"),
        "fluid_style" => minecraft("fluid_storage.style"),
        "access_sign" => Some("config.jade.plugin_jade_access.sign".to_owned()),
        "access_block" => Some("config.jade.plugin_jade_access.block".to_owned()),
        "access_entity" => Some("config.jade.plugin_jade_access.entity".to_owned()),
        "access_variant" => Some("config.jade.plugin_jade_access.entity_variant".to_owned()),
        "access_held" => Some("config.jade.plugin_jade_access.held_item".to_owned()),
        "access_npc" => Some("config.jade.plugin_jade_access.npc_description".to_owned()),
        _ => None,
    }
}

/// Text without its `§` codes, for measuring.
pub fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut skip = 0u8;
    for c in s.chars() {
        if skip > 0 {
            // `§#RRGGBB` skips the six digits too.
            skip = if skip == 1 && c == '#' { 6 } else { skip - 1 };
        } else if c == '§' {
            skip = 1;
        } else {
            out.push(c);
        }
    }
    out
}

/// DisplayHelper.dfCommas: at most two decimals.
pub fn trim2(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A whole number with thousands commas.
pub fn commas(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// DisplayHelper.humanReadableNumber: "1.5kFE".
pub fn human(number: i64, unit: &str) -> String {
    if number == 0 {
        return format!("0{unit}");
    }
    let mut v = number.unsigned_abs() as f64;
    let exp = ((v.log10() as usize) / 3).min(7);
    v /= 1000f64.powi(exp as i32);
    let sign = if number < 0 { "-" } else { "" };
    let prefix = if exp == 0 {
        String::new()
    } else {
        "kMGTPEZ"[exp - 1..exp].to_owned()
    };
    format!("{sign}{}{prefix}{unit}", trim2(v))
}

/// IThemeHelper.seconds as a translatable marker (server text): "12s", "2m 5s".
pub fn seconds_marker(ticks: i64) -> String {
    use crate::api::tr;
    let mut s = (ticks as f32 / 20.0).floor() as i64;
    if s >= 3600 {
        let h = s / 3600;
        s %= 3600;
        let m = s / 60;
        if m == 0 {
            return tr("jade.hours", "{0}h", &[&h.to_string()]);
        }
        return tr(
            "jade.hours_minutes",
            "{0}h {1}m",
            &[&h.to_string(), &m.to_string()],
        );
    }
    if s >= 60 {
        let m = s / 60;
        s %= 60;
        if s == 0 {
            return tr("jade.minutes", "{0}m", &[&m.to_string()]);
        }
        return tr(
            "jade.minutes_seconds",
            "{0}m {1}s",
            &[&m.to_string(), &s.to_string()],
        );
    }
    tr("jade.seconds", "{0}s", &[&s.to_string()])
}

/// StringUtil.formatTickDuration: m:ss (h:mm:ss past an hour).
pub fn tick_duration(ticks: i64) -> String {
    let s = ticks / 20;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}
