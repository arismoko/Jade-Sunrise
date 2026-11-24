//! `/<ns>`: this mod's commands menu. The shared guide mod (GuideME) draws it from
//! `assets/<ns>/commands/` as it draws the W guide: a tree of the commands, a page for each with its usage, what it
//! does and a 3D scene where one helps.
//!
//! The server answers the command, so a client never picks what it is shown: the executor reads the player's
//! permission level from the command's source and sends it on `<ns>:commands_menu`; the client opens the menu with
//! only the commands of that level or less. Every command listed is still checked by the server when it is run.
//! Without GuideME the client says so in chat.
//!
//! Shared file: `tools/commands-menu/commands_menu.rs`, copied into each mod with its own `NS`.

use mod_api::*;
use std::sync::Arc;

/// The mod's namespace: the command's name and the menu's folder.
pub(crate) const NS: &str = "jade";

fn channel() -> PayloadId {
    PayloadId(format!("{NS}:commands_menu"))
}

/// The bare `/<ns>` usage: everyone's (level 0), answered on the server.
pub(crate) fn command() -> CommandRegistration {
    Command::new(NS).executes(|host, source, _| {
        // Vanilla's message for a player-only command run by another source (CommandSourceStack.getPlayerOrException).
        let fail = || Box::new(Text::translate("permissions.requires.player", Vec::new()));
        let CommandSource::Player { player, permission, .. } = source else { return Err(fail()) };
        let level = number(*permission);
        player.send_payload(host, &channel(), PayloadBytes(vec![level])).map_err(|_| fail())?;
        Ok(1)
    })
}

/// Registers the client's side of the channel (call from `register`, with the mod's payload registrar).
pub(crate) fn register_payload(r: &mut dyn Registrar) {
    r.payload_handlers().registrar("1").play_to_client(channel(), Some(Arc::new(Open)));
}

fn number(level: PermissionLevel) -> u8 {
    match level {
        PermissionLevel::All => 0,
        PermissionLevel::Moderators => 1,
        PermissionLevel::Gamemasters => 2,
        PermissionLevel::Admins => 3,
        _ => 4,
    }
}

/// The client's side: the level the server sent opens the menu cut to it.
struct Open;

impl IPayloadHandler for Open {
    fn handle(&self, payload: PayloadBytes, cx: &mut dyn IPayloadContext) -> Result<(), PayloadHandleError> {
        #[cfg_attr(not(feature = "guideme"), allow(unused_variables))]
        let level = payload.0.first().copied().unwrap_or(0).min(4);
        let Ok(host) = cx.client_host() else { return Ok(()) };
        #[cfg_attr(not(feature = "guideme"), allow(unused_mut))]
        let mut opened = false;
        mod_api::if_installed!(host, guideme, {
            opened = guideme::api::open_commands(NS, level);
        });
        if !opened {
            // GuideME's own lang files aren't there without it, so the line is in English.
            let _ = host.chat_line(Text::literal(format!("Install GuideME to open the /{NS} commands menu.")));
        }
        Ok(())
    }
}
