//! Rebinding Jade's own key mappings from its config screen, as Controls does: `ModKeyMappingRef::set_binding` / `reset_binding`
//! (they also save `sunrise-mods.txt`). The screen's handlers read the host only, so a change is queued there and
//! made at the next client tick, where the host can be written.

use mod_api::*;
use std::sync::Mutex;

enum Op {
    Set(String, String, u32),
    Reset(String),
}

static QUEUE: Mutex<Vec<Op>> = Mutex::new(Vec::new());

fn push(op: Op) {
    if let Ok(mut q) = QUEUE.lock() {
        q.push(op);
    }
}

/// Binds Jade's key mapping `name` (its registration id) to `key` with GLFW `modifiers` (shift 1, control 2, alt 4).
pub(crate) fn set_key(name: &str, key: &str, modifiers: u32) {
    push(Op::Set(name.to_owned(), key.to_owned(), modifiers));
}

/// Puts Jade's key mapping `name` back to its default.
pub(crate) fn reset_key(name: &str) {
    push(Op::Reset(name.to_owned()));
}

/// The queued changes, in order.
pub(crate) fn tick(host: &mut dyn ClientHost) {
    let ops = match QUEUE.lock() {
        Ok(mut q) if !q.is_empty() => std::mem::take(&mut *q),
        _ => return,
    };
    for op in ops {
        let name = match &op {
            Op::Set(name, ..) | Op::Reset(name) => name.clone(),
        };
        // A handle gone stale (the client's roster was replaced) is resolved again once.
        let mut result = Err(ModKeyRefusal::Stale);
        for _ in 0..2 {
            result = mapping(&*host, &name).and_then(|m| match &op {
                Op::Set(_, key, modifiers) => {
                    m.set_binding(host, key, ClientModifiers::new(*modifiers))
                }
                Op::Reset(_) => m.reset_binding(host),
            });
            if result != Err(ModKeyRefusal::Stale) {
                break;
            }
            forget(&name);
        }
        if let Err(refusal) = result {
            eprintln!("[jade] key {name}: {refusal}");
        }
    }
}

/// The handles of Jade's key mappings by registration id, resolved on first use.
static HANDLES: Mutex<Vec<(String, ModKeyMappingRef)>> = Mutex::new(Vec::new());

/// Jade's key mapping `name` (its registration id).
fn mapping(host: &dyn ClientHost, name: &str) -> Result<ModKeyMappingRef, ModKeyRefusal> {
    let mut handles = HANDLES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, m)) = handles.iter().find(|(n, _)| n == name) {
        return Ok(*m);
    }
    let m = host.mod_key_mapping(crate::opts::ID, name)?;
    handles.push((name.to_owned(), m));
    Ok(m)
}

fn forget(name: &str) {
    HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(n, _)| n != name);
}

/// The key and modifiers Jade's mapping `name` is bound to now, as Controls has it.
pub(crate) fn binding(host: &dyn ClientHost, name: &str) -> Option<(String, ClientModifiers)> {
    for _ in 0..2 {
        match mapping(host, name)
            .and_then(|m| m.binding(host).map(|(k, mods)| (k.to_owned(), mods)))
        {
            Ok(b) => return Some(b),
            Err(ModKeyRefusal::Stale) => forget(name),
            Err(_) => return None,
        }
    }
    None
}

static PLAYER_LIST: Mutex<Option<VanillaKeyMappingRef>> = Mutex::new(None);

/// Whether vanilla's Player List key mapping (`key.playerlist`) is currently held down.
pub(crate) fn player_list_down(host: &dyn ClientHost) -> bool {
    for _ in 0..2 {
        let m = {
            let mut slot = PLAYER_LIST.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(m) = *slot {
                m
            } else {
                let Ok(m) = host.vanilla_key_mapping("key.playerlist") else {
                    return false;
                };
                *slot = Some(m);
                m
            }
        };
        match m.is_down(host) {
            Ok(down) => return down,
            Err(MappingError::Stale) => {
                *PLAYER_LIST.lock().unwrap_or_else(|e| e.into_inner()) = None
            }
            Err(_) => return false,
        }
    }
    false
}
