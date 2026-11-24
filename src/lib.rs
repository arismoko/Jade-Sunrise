//! Jade (Snownee, CC BY-NC-SA 4.0; Jade-mc26.3-Fabric-26.3.5) for Sunrise's mod API: the look-at
//! overlay for blocks and entities, its providers, themes, config screens and profiles, its keys, `/jade` and `/jadec`,
//! and the `jade:max_position_deviation` rule. Written against Hooks only (no mixins, no `[[inject]]`), so it builds
//! against Sunrise's prebuilt SDK.
//!
//! Where things run:
//! - the client finds what the player looks at, builds the box from Jade's client providers, the other mods' (see
//!   `api.rs`) and the server's lines, and draws it in the HUD layer `jade:overlay` (`overlay.rs`, `theme.rs`);
//! - it asks the server a few times a second on `jade:request`; the server checks the request and answers on
//!   `jade:data` with what only it knows: block entity contents through the `ITEMS`, `ENERGY` and `FLUIDS`
//!   capabilities, everything about entities (`server.rs`, `entities.rs`);
//! - settings live in `config/jade/` as Java's (`opts.rs`); the config screens are the mod's own screen
//!   (`config_ui.rs`).
mod commands_menu;
mod api;
mod client;
mod config_ui;
mod entities;
mod facts;
mod gfx;
mod keys;
mod opts;
mod overlay;
mod server;
mod theme;

pub use api::*;
use mod_api::*;

#[derive(Default)]
pub(crate) struct Mod;

#[mod_api::sunrise_mod]
impl Hooks for Mod {
    fn register(&self, r: &mut dyn Registrar, _cx: &RegistrationContext) {
        // `/<ns>` commands menu: the client's side of its channel.
        commands_menu::register_payload(r);
        client::register_keys(r);
        server::register_rules(r);
        // `jade:request` (client to server) and `jade:data` (server to client). Optional, as Jade's: a client without
        // Jade joins a Jade server, and Jade's client-side lines work on a server without it.
        let mut handlers = r.payload_handlers();
        let mut payloads = handlers.registrar("1").optional();
        payloads.play_to_server(PayloadId(overlay::NET_REQUEST.into()), std::sync::Arc::new(FromClient));
        payloads.play_to_client(PayloadId(overlay::NET_DATA.into()), Some(std::sync::Arc::new(FromServer)));
    }

    fn register_gui_layers(&self, _cx: &RegistrationContext, event: &mut RegisterGuiLayersEvent<'_>) {
        if !event.layers.register(
            overlay::LAYER.to_owned(),
            GuiLayerPlacement::Below("minecraft:chat".to_owned()),
            Box::new(|host, draw| overlay::draw_layer(host, draw)),
        ) {
            eprintln!("[jade] HUD layer {} not registered", overlay::LAYER);
        }
    }

    fn gui_layer_pre(&self, host: &dyn ClientHost, event: &mut GuiLayerPreEvent<'_>) -> Flow {
        // Boss Bar Overlap's Hide Boss Bar: no bars while the box shows.
        if event.layer == "minecraft:boss_overlay" && host.on(opts::ID) && overlay::with(|c| c.showing) && opts::opt(host, "boss_bar_overlap") == "hide_bar" {
            return Flow::Cancel;
        }
        Flow::Continue
    }

    fn customize_boss_event_progress(&self, _host: &dyn ClientHost, _event: &mut CustomizeBossEventProgressEvent<'_>) -> Flow {
        overlay::with(|c| c.bars_now += 1);
        Flow::Continue
    }

    fn client_tick_post(&self, host: &mut dyn ClientHost, _event: &mut ClientTickPostEvent) {
        keys::tick(host);
        client::tick(host);
    }

    fn register_commands(&self, _host: &mut dyn ServerHost, event: &mut RegisterCommandsEvent<'_>) {
        server::register_commands(event);
    }

    fn register_client_commands(&self, _host: &mut dyn ClientHost, event: &mut RegisterClientCommandsEvent<'_>) {
        client::register_client_commands(event);
    }

    fn command(&self, host: &mut dyn ClientHost, event: &mut CommandEvent<'_>) -> Flow {
        client::command(host, event.command)
    }

    fn item_tooltip(&self, host: &dyn ClientHost, event: &mut ItemTooltipEvent<'_>) {
        overlay::item_tooltip(host, event);
    }

    fn add_client_reload_listeners(&self, _context: &dyn ClientReloadListenerContext, event: &mut AddClientReloadListenersEvent<'_>) {
        theme::register(event.listeners);
    }

    fn client_player_logging_out(&self, _host: &mut dyn ClientHost, _event: &mut ClientPlayerLoggingOutEvent) {
        client::logged_out();
        facts::forget_client();
    }

    fn loot_table_load(&self, _context: &dyn DataLoadContext, event: &mut LootTableLoadEvent<'_>) {
        // LootTableMineableCollector: read only, the table is left as it came.
        facts::loot_table_loaded(event.id, event.table);
    }

    fn player_logged_out(&self, _host: &mut dyn ServerHost, event: &mut PlayerLoggedOutEvent<'_>) {
        opts::forget_player(event.player);
    }

    fn mob_effect_added(&self, host: &mut dyn ServerHost, event: &mut MobEffectAddedEvent) {
        entities::effect_added(host, event.entity, event.effect, event.amplifier);
    }

    fn screen_render_post(&self, host: &dyn ClientHost, event: &mut ScreenRenderPostEvent<'_>) {
        config_ui::draw(host, event.draw, event.screen);
    }

    fn screen_mouse_button_pressed_pre(&self, host: &dyn ClientHost, event: &mut ScreenMouseButtonPressedPreEvent<'_>) -> Flow {
        if config_ui::pressed(host, event.screen, event.button, event.modifiers) { Flow::Cancel } else { Flow::Continue }
    }

    fn screen_mouse_button_released_pre(&self, host: &dyn ClientHost, event: &mut ScreenMouseButtonReleasedPreEvent<'_>) -> Flow {
        if config_ui::released(host, event.screen) { Flow::Cancel } else { Flow::Continue }
    }

    fn screen_mouse_scrolled_pre(&self, host: &dyn ClientHost, event: &mut ScreenMouseScrolledPreEvent<'_>) -> Flow {
        if config_ui::scrolled(host, event.screen, event.scroll, event.modifiers) { Flow::Cancel } else { Flow::Continue }
    }

    fn screen_key_pressed_pre(&self, host: &dyn ClientHost, event: &mut ScreenKeyPressedPreEvent<'_>) -> Flow {
        if config_ui::key(host, event.screen, event.key, event.repeat, event.modifiers) { Flow::Cancel } else { Flow::Continue }
    }

    fn screen_char_typed_pre(&self, host: &dyn ClientHost, event: &mut ScreenCharTypedPreEvent<'_>) -> Flow {
        if config_ui::typed(host, event.screen, event.c) { Flow::Cancel } else { Flow::Continue }
    }
}

/// `jade:request` on the server: a client asks about what it looks at, or sends its options.
struct FromClient;

impl IPayloadHandler for FromClient {
    fn handle(&self, payload: PayloadBytes, cx: &mut dyn IPayloadContext) -> Result<(), PayloadHandleError> {
        let player = cx.player().map_err(|e| PayloadHandleError::Failed(format!("{e:?}")))?.uuid();
        let host = cx.server_host().map_err(|e| PayloadHandleError::Failed(format!("{e:?}")))?;
        if host.on(opts::ID) {
            server::from_client(host, player, &payload.0);
        }
        Ok(())
    }
}

/// `jade:data` on the client: the server's answer.
struct FromServer;

impl IPayloadHandler for FromServer {
    fn handle(&self, payload: PayloadBytes, _cx: &mut dyn IPayloadContext) -> Result<(), PayloadHandleError> {
        client::from_server(&payload.0);
        Ok(())
    }
}

