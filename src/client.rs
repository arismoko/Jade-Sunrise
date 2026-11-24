//! The client's ticks: Jade's keys (JadeClient, through Sunrise's key mappings), the requests for the server's lines,
//! the settings the server answers this player by, what comes back, and `/jadec` (JadeClientCommand).
use crate::opts::{self, ID, on, opt, tr};
use crate::overlay::{self, EntityData, NET_REQUEST, Seen, with};
use mod_api::*;
use std::time::Instant;

/// JadeKeys: name, label and default key, registered as Sunrise's key mappings (Controls, category Jade).
pub const KEYS: [[&str; 3]; 11] = [
    ["config", "Open Config", "key.keyboard.keypad.0"],
    ["show_overlay", "Show Overlay", "key.keyboard.keypad.1"],
    ["toggle_liquid", "Toggle Fluid", "key.keyboard.keypad.2"],
    ["show_recipes", "Show Recipes", "key.keyboard.keypad.3"],
    ["show_uses", "Show Uses", "key.keyboard.keypad.4"],
    ["narrate", "Narrate Target", "key.keyboard.keypad.5"],
    ["show_details", "Show Details", "key.keyboard.left.shift"],
    ["profile.0", "Use Profile #0", "key.keyboard.unknown"],
    ["profile.1", "Use Profile #1", "key.keyboard.unknown"],
    ["profile.2", "Use Profile #2", "key.keyboard.unknown"],
    ["profile.3", "Use Profile #3", "key.keyboard.unknown"],
];

pub fn register_keys(r: &mut dyn Registrar) {
    for [name, _, key] in KEYS {
        let mut k = KeyRegistration::new(&name.replace('.', "_"), key);
        k.label = format!("key.jade.{name}");
        k.category = "key.category.jade.main".to_owned();
        r.register_key_mapping(k);
    }
}

/// The key a mapping is bound to now (Controls), as the game names it.
pub fn bound(h: &dyn ClientHost, name: &str) -> String {
    crate::keys::binding(h, &name.replace('.', "_")).map(|(k, _)| k).unwrap_or_else(|| KEYS.iter().find(|k| k[0] == name).map_or("key.keyboard.unknown", |k| k[2]).to_owned())
}

/// The key's name as Controls shows it.
pub fn key_label(h: &dyn ClientHost, name: &str) -> String {
    let key = bound(h, name);
    h.translate(&key).unwrap_or_else(|| key.rsplit('.').next().unwrap_or(&key).to_uppercase())
}

fn down(h: &dyn ClientHost, name: &str) -> bool {
    let key = bound(h, name);
    key != "key.keyboard.unknown" && h.is_key_down(&key)
}

/// Jade's keys, once a tick: the toggles act on a press, Show Details while held.
fn keys(h: &dyn ClientHost) {
    // A key mapping doesn't fire under a screen (the config screens' keys are for typing and binding).
    let playing = h.playing();
    let profiles: &[&'static str] = if on(h, "enable_profiles") { &["profile.0", "profile.1", "profile.2", "profile.3"] } else { &[] };
    let pressed: Vec<&'static str> = ["config", "show_overlay", "toggle_liquid", "show_recipes", "show_uses", "narrate"].into_iter().chain(profiles.iter().copied()).filter(|n| playing && down(h, n)).collect();
    let toggle = opt(h, "display_mode") != "hold";
    let tts_toggle = opt(h, "tts_mode") == "toggle";
    let hold = down(h, "show_overlay");
    let details = down(h, "show_details");
    let narrate_keys = on(h, "narrate_keys");
    with(|c| {
        for name in &pressed {
            if c.keys_down.contains(name) {
                continue;
            }
            match *name {
                "show_overlay" if toggle => {
                    c.hidden = !c.hidden;
                    c.say = Some(if c.hidden { tr(h, "narration.jade.key.show_overlay.off", "Overlay hidden") } else { tr(h, "narration.jade.key.show_overlay.on", "Overlay shown") });
                    if c.hidden {
                        c.hint = Some(0);
                    }
                }
                "toggle_liquid" => {
                    c.liquids = !c.liquids;
                    c.say = Some(if c.liquids { tr(h, "narration.jade.key.toggle_liquid.on", "Fluid shown") } else { tr(h, "narration.jade.key.toggle_liquid.off", "Fluid hidden") });
                }
                "narrate" if tts_toggle => {
                    c.tts_on = !c.tts_on;
                    if c.tts_on {
                        c.hint = Some(1);
                    }
                }
                "profile.0" => c.use_profile = Some(0),
                "profile.1" => c.use_profile = Some(1),
                "profile.2" => c.use_profile = Some(2),
                "profile.3" => c.use_profile = Some(3),
                "narrate" => c.narrate = true,
                "config" => c.open_config = true,
                "show_recipes" => c.lookup = Some(false),
                "show_uses" => c.lookup = Some(true),
                _ => {}
            }
        }
        // DisplayMode.HOLD_KEY: shown only while the key is held.
        if !toggle {
            c.hidden = !hold;
        }
        c.keys_down = pressed;
        c.details = details;
        if !narrate_keys {
            c.say = None;
        }
    });
}

/// `client_tick_post`: keys, hints, the config screen, profiles, recipe lookups, and the server requests.
pub fn tick(h: &mut dyn ClientHost) {
    if !h.on(ID) {
        return;
    }
    keys(h);
    // Narrate Key Binds (the key's new state) and Narrate Target (the box), said by the game's narrator. Java Jade
    // speaks the box through text2speech whatever the Narrator option is; the game's narrator says nothing while the
    // option is Off or Chat only (`NarrateError::Off`).
    for text in [with(|c| c.say.take()), with(|c| c.speak.take())].into_iter().flatten() {
        let _ = h.narrate(&Text::literal(text), Narration::Now);
    }
    if let Some(n) = with(|c| c.use_profile.take()) {
        opts::use_profile(h, &n.to_string());
        let _ = h.chat_line(Text::literal(format!("Jade profile #{n}")));
    }
    // Jade.history()'s hints, once each: toast.jade.toggle_hint and toast.jade.tts_hint, in chat.
    if let Some(which) = with(|c| c.hint.take()) {
        let (flag, key_name, first, second) = if which == 0 {
            ("hint_overlay_toggle", "show_overlay", tr(h, "toast.jade.toggle_hint.1", "Jade is now closed"), tr(h, "toast.jade.toggle_hint.2", "Press %s again to reopen"))
        } else {
            ("hint_narrator_toggle", "narrate", tr(h, "toast.jade.tts_hint.1", "Jade's narrator is now open"), tr(h, "toast.jade.tts_hint.2", "Press %s again to disable"))
        };
        if opt(h, flag) != "shown" {
            opts::set(h, flag, "shown");
            let label = key_label(h, key_name);
            let _ = h.chat_line(Text::literal(first));
            let _ = h.chat_line(Text::literal(second.replace("%s", &label)));
        }
    }
    if with(|c| std::mem::take(&mut c.open_config)) {
        crate::config_ui::open(h);
    }
    crate::config_ui::tick(h);
    // JEICompat: the recipe viewer's recipes or uses of the looked-at item.
    if let Some(uses) = with(|c| c.lookup.take())
        && let Some(item) = target_item(h)
        && let Some(f) = crate::api::registry(|r| r.lookup)
    {
        f(h, &item, uses);
    }
    sync_settings(h);
    request(h);
}

/// The item of what is looked at (the box's icon).
fn target_item(h: &dyn ClientHost) -> Option<String> {
    let seen = with(|c| c.seen.clone())?;
    overlay::build(h, &seen)?.icon
}

/// The values the server answers this player by, sent when they change.
fn sync_settings(h: &mut dyn ClientHost) {
    let epoch = opts::settings_epoch();
    if with(|c| c.sent_epoch) == epoch {
        return;
    }
    let msg = format!("s {}", opts::values_for_server(h));
    if h.send_payload_to_server(&PayloadId(NET_REQUEST.into()), PayloadBytes(msg.into_bytes())).is_ok() {
        with(|c| c.sent_epoch = epoch);
    }
}

/// What the server is asked about now: the block's lines (and its pick of a nearer special entity), the entity's,
/// or its pick with nothing else in view. Again a few times a second while it stays, at once when it changes.
fn request(h: &mut dyn ClientHost) {
    if !with(|c| c.active) {
        return;
    }
    let seen = with(|c| c.seen.clone()).unwrap_or(Seen::Entity { id: u32::MAX, point: [0.0; 3], dist: f64::INFINITY });
    let details = u8::from(with(|c| c.details));
    let extra = opts::num(h, "reach_distance").clamp(0.0, 20.0);
    let (key, msg) = match &seen {
        Seen::Block { pos, block, real, state, hit, dist, face } => {
            // Not for what passes for another block without its block entity (builtInOverrides).
            if block != real && !real.contains("chest") {
                return;
            }
            let slot = overlay::shelf_slot(*pos, real, state, *hit, *face);
            let key = format!("b {} {} {} {details} {slot}", pos[0], pos[1], pos[2]);
            let h4 = hit.map(|v| (v * 4096.0).round() as i64);
            (key.clone(), format!("{key} {} {} {} {extra} {dist}", h4[0], h4[1], h4[2]))
        }
        Seen::Entity { id, .. } if *id != u32::MAX && !with(|c| c.entity_data.as_ref().is_some_and(|d| d.id == *id && d.picked)) => {
            let key = format!("e {id} {details}");
            (key.clone(), key)
        }
        _ => {
            let key = format!("n {extra} {details}");
            (key.clone(), key)
        }
    };
    let due = with(|c| c.asked.as_ref().is_none_or(|(k, at)| *k != key || at.elapsed().as_millis() >= 250));
    if due && h.send_payload_to_server(&PayloadId(NET_REQUEST.into()), PayloadBytes(msg.into_bytes())).is_ok() {
        with(|c| c.asked = Some((key, Instant::now())));
    }
}

/// `net_from_server` on `jade:data`: a block's or an entity's lines, a pick, or `/jade show|hide`.
pub fn from_server(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    // ShowOverlayPacket.
    if let Some(rest) = text.strip_prefix("o ") {
        with(|c| c.hidden = rest.trim() != "1");
        return;
    }
    // ServerHandshakePacket.shearableBlocks.
    if let Some(rest) = text.strip_prefix("h ") {
        crate::facts::shears_received(rest);
        return;
    }
    if text.starts_with('N') {
        with(|c| {
            if c.entity_data.as_ref().is_some_and(|d| d.picked) {
                c.entity_data = None;
            }
        });
        return;
    }
    let mut it = text.split('\n');
    let head = it.next().unwrap_or("");
    if let Some(rest) = head.strip_prefix("E ") {
        let w: Vec<&str> = rest.split(' ').collect();
        let [id, picked, dist, kind] = w.as_slice() else { return };
        let (Ok(id), Ok(dist)) = (id.parse::<u32>(), dist.parse::<f64>()) else { return };
        let name = it.next().unwrap_or("").to_owned();
        let icon = it.next().unwrap_or("").to_owned();
        let lines: Vec<String> = it.take(256).map(str::to_owned).collect();
        with(|c| c.entity_data = Some(EntityData { id, kind: (*kind).to_owned(), name, icon, lines, picked: *picked == "1", dist, at: Instant::now() }));
        return;
    }
    if let Some(rest) = head.strip_prefix("B ") {
        let v: Vec<i32> = rest.split(' ').filter_map(|s| s.parse().ok()).collect();
        let [x, y, z] = v.as_slice() else { return };
        let lines: Vec<String> = it.take(256).map(str::to_owned).collect();
        with(|c| {
            c.block_data = Some(([*x, *y, *z], lines, Instant::now()));
            if c.entity_data.as_ref().is_some_and(|d| d.picked) {
                c.entity_data = None;
            }
        });
    }
}

/// The world went: what was asked and answered goes with it.
pub fn logged_out() {
    with(|c| {
        c.sent_epoch = 0;
        c.block_data = None;
        c.entity_data = None;
        c.asked = None;
        c.pinned = None;
        c.hidden = false;
    });
}

/// JadeClientCommand's usages, for completion and the client's own dispatch.
pub fn register_client_commands(e: &mut RegisterClientCommandsEvent<'_>) {
    for usage in ["jadec config", "jadec pin", "jadec handlers", "jadec use_profile <profile:integer>", "jadec generate_server_overrides", "jadec generate_server_overrides <pattern:text>"] {
        e.commands.push(Command::new(usage));
    }
}

/// `/jadec ...` on the player's own client.
pub fn command(h: &mut dyn ClientHost, line: &str) -> Flow {
    let mut words = line.trim_start_matches('/').split_whitespace();
    if words.next() != Some("jadec") {
        return Flow::Continue;
    }
    let args: Vec<&str> = words.collect();
    let say = |h: &mut dyn ClientHost, t: String| {
        let _ = h.chat_line(Text::literal(t));
    };
    match args.as_slice() {
        ["config"] => with(|c| c.open_config = true),
        ["use_profile", n @ ("0" | "1" | "2" | "3")] => {
            opts::use_profile(h, n);
            say(h, format!("Jade profile #{n}"));
        }
        ["pin"] => with(|c| c.pin_requested = true),
        ["handlers"] => {
            let ok = opts::dump_handlers(h);
            let t = if ok { tr(h, "command.jade.dump.success", "Dumped successfully") } else { "Couldn't write config/jade/handlers.md".to_owned() };
            say(h, t);
        }
        ["generate_server_overrides", pattern @ ..] => {
            let t = match opts::generate_overrides(h, &pattern.join(" ")) {
                Some(_) => opts::trf(h, "command.jade.generate_server_overrides.success", "Generated override file: %s", &["config/jade/server-plugin-overrides.json"]),
                None => tr(h, "command.jade.generate_server_overrides.no_match", "No config options match the pattern"),
            };
            say(h, t);
        }
        _ => say(h, "/jadec config | use_profile <0-3> | pin | handlers | generate_server_overrides [pattern]".to_owned()),
    }
    Flow::Cancel
}
