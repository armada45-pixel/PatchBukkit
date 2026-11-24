use super::CALLBACK_CONTEXT;
use crate::proto::patchbukkit::common::Uuid as ProtoUuid;
use pumpkin::entity::player::Player;
use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::sync::{Arc, RwLock};

static PLAYER_HANDLE_CACHE: RwLock<Option<HashMap<uuid::Uuid, Arc<Player>>>> = RwLock::new(None);

/// This plugin is its own Rust binary with its own copy of `arc-swap`, so a swap made from code
/// compiled here cannot see Pumpkin's in-flight `load()` guards and may free a value Pumpkin is
/// still reading (seen as a SIGSEGV in `World::flush_block_updates`). Take a reference to each old
/// value before such a swap and pass it here: it is dropped 30 s later, after any reader is done.
// shortcut: one sleeping thread per swap; fine for rare swaps (world create/unload, spawn, teleports), use a reaper queue if they get frequent.
pub fn keep_alive_after_swap<T: Send + 'static>(old_values: T) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(30));
        drop(old_values);
    });
}

#[must_use]
pub fn get_string(str_ptr: *const c_char) -> String {
    unsafe { CStr::from_ptr(str_ptr).to_string_lossy().into_owned() }
}

pub fn cache_player(player: Arc<Player>) {
    let player_uuid = player.gameprofile.id;
    if let Ok(mut write_guard) = PLAYER_HANDLE_CACHE.write() {
        let cache = write_guard.get_or_insert_with(HashMap::new);
        cache.insert(player_uuid, player);
    }
}

pub fn remove_cached_player(player_uuid: &uuid::Uuid) {
    if let Ok(mut write_guard) = PLAYER_HANDLE_CACHE.write()
        && let Some(ref mut cache) = *write_guard
    {
        cache.remove(player_uuid);
    }
}

pub fn with_player_uuid<F, R>(player_uuid: uuid::Uuid, f: F) -> Option<R>
where
    F: FnOnce(Arc<Player>) -> R,
{
    let ctx = CALLBACK_CONTEXT.get()?;

    if let Some(player) = ctx.plugin_context.server.get_player_by_uuid(player_uuid) {
        return Some(f(player));
    }

    // Release the read guard before calling `f`: it may send packets or fire events,
    // which call `cache_player` (write lock) and would deadlock this thread.
    cached_player(player_uuid).map(f)
}

fn cached_player(player_uuid: uuid::Uuid) -> Option<Arc<Player>> {
    PLAYER_HANDLE_CACHE
        .read()
        .ok()?
        .as_ref()?
        .get(&player_uuid)
        .cloned()
}

pub fn with_player<F, R>(proto_uuid: Option<&ProtoUuid>, f: F) -> Option<R>
where
    F: FnOnce(Arc<Player>) -> R,
{
    let uuid_str = &proto_uuid?.value;
    let player_uuid = uuid::Uuid::parse_str(uuid_str).ok()?;
    with_player_uuid(player_uuid, f)
}
