use std::{
    cell::Cell,
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex, OnceLock},
};

use anyhow::Result;
use jni::{
    Env,
    objects::{Global, JClass},
};
use prost::Message;
use pumpkin::{entity::player::Player, server::Server};

use crate::{
    events::handler::JvmEventPayload,
    java::native_callbacks::fast_path::try_dispatch_packet_upcall,
    proto::patchbukkit::events::{FireEventResponse, event::Data},
};

/// Cached `(entity_id, is_op)` per player UUID so `VirtualChannelManager.registerPlayer`
/// is only invoked across JNI when a player connects or their entity ID / op status changes.
static REGISTERED_PLAYERS: LazyLock<Mutex<HashMap<uuid::Uuid, (i32, bool)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

thread_local! {
    /// Re-entrant-safe scratch buffer for Protobuf serialization of outgoing events.
    static ENCODE_BUF: Cell<Vec<u8>> = const { Cell::new(Vec::new()) };
}

/// `PatchBukkitEventFactory`, resolved once at JVM start. Looking it up by name on every event fails
/// when the event fires inside a Java upcall, where JNI uses the JDK's own class loader.
static EVENT_FACTORY: OnceLock<Global<JClass<'static>>> = OnceLock::new();
/// `VirtualChannelManager`, cached for the same reason.
static CHANNEL_MANAGER: OnceLock<Global<JClass<'static>>> = OnceLock::new();

/// Entity ID of each player's session that has left. Events still in flight for it (a teleport finishing
/// after the quit) must not register the player in Paper again, or a ghost stays online there.
// shortcut: one entry per player who ever left (removed on rejoin), fine for a small server; prune by age if it grows.
static LEFT_SESSIONS: LazyLock<Mutex<HashMap<uuid::Uuid, i32>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub struct EventManager;

impl Default for EventManager {
    fn default() -> Self {
        Self::new()
    }
}

impl EventManager {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub fn fire_event(
        &mut self,
        env: &mut Env,
        payload: JvmEventPayload,
        plugin_name: String,
    ) -> Result<FireEventResponse> {
        Self::fire_event_static(env, payload, plugin_name)
    }

    /// Resolves and caches the event factory class; call on a thread where `FindClass` sees PatchBukkit.
    pub fn cache_classes(env: &mut Env) -> Result<()> {
        let class = env.find_class(jni::jni_str!(
            "org/patchbukkit/events/PatchBukkitEventFactory"
        ))?;
        let _ = EVENT_FACTORY.set(env.new_global_ref(class)?);
        let class = env.find_class(jni::jni_str!("org/patchbukkit/network/VirtualChannelManager"))?;
        let _ = CHANNEL_MANAGER.set(env.new_global_ref(class)?);
        Ok(())
    }

    pub fn fire_event_static(
        env: &mut Env,
        payload: JvmEventPayload,
        plugin_name: String,
    ) -> Result<FireEventResponse> {
        if let Some(ref player) = payload.context.player
            && let Err(e) = Self::ensure_player_registered(env, player, &payload.context.server)
        {
            if env.exception_check() {
                env.exception_describe();
                env.exception_clear();
            }
            tracing::warn!("Failed to ensure player registered before event: {e}");
        }

        let is_player_leave = matches!(payload.event.data, Some(Data::PlayerLeave(_)));
        let leave_player_uuid = if is_player_leave {
            payload.context.player.as_ref().map(|p| p.gameprofile.id)
        } else {
            None
        };
        if let Some(uuid) = leave_player_uuid
            && let Ok(mut map) = REGISTERED_PLAYERS.lock()
        {
            map.remove(&uuid);
            if let Some(player) = payload.context.player.as_ref() {
                LEFT_SESSIONS
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(uuid, player.living_entity.entity.entity_id);
            }
            // A bridged teleport whose Pumpkin event never fired must not hide a later one to the same spot.
            crate::java::native_callbacks::fast_path::BRIDGED_TELEPORTS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|(id, _)| *id != uuid);
        }

        // Panama FFM upcall fast-path for high-frequency network packets (`PacketReceived` / `PacketSent`).
        // Bypasses Protobuf serialization, JNI `JByteArray` allocation, and Java Protobuf decoding.
        match payload.event.data {
            Some(Data::PacketReceived(ref pkt)) => {
                if let Some(uuid) = payload
                    .context
                    .player
                    .as_ref()
                    .map(|p| p.gameprofile.id)
                    .or_else(|| {
                        pkt.player_uuid
                            .as_ref()
                            .and_then(|u| uuid::Uuid::parse_str(&u.value).ok())
                    })
                    && try_dispatch_packet_upcall(uuid, pkt.packet_id, &pkt.payload, false)
                {
                    if env.exception_check() {
                        env.exception_describe();
                        env.exception_clear();
                    }
                    return Ok(FireEventResponse {
                        cancelled: false,
                        data: None,
                    });
                }
            }
            Some(Data::PacketSent(ref pkt)) => {
                if let Some(uuid) = payload
                    .context
                    .player
                    .as_ref()
                    .map(|p| p.gameprofile.id)
                    .or_else(|| {
                        pkt.player_uuid
                            .as_ref()
                            .and_then(|u| uuid::Uuid::parse_str(&u.value).ok())
                    })
                    && try_dispatch_packet_upcall(uuid, pkt.packet_id, &pkt.payload, true)
                {
                    if env.exception_check() {
                        env.exception_describe();
                        env.exception_clear();
                    }
                    return Ok(FireEventResponse {
                        cancelled: false,
                        data: None,
                    });
                }
            }
            _ => {}
        }

        let mut encode_buf = ENCODE_BUF.with(Cell::take);
        encode_buf.clear();
        payload.event.encode(&mut encode_buf)?;
        let j_bytes_res = env.byte_array_from_slice(&encode_buf);
        ENCODE_BUF.with(|c| c.set(encode_buf));
        let j_bytes = j_bytes_res?;
        let plugin_name_jstr = env.new_string(&plugin_name)?;

        let factory = EVENT_FACTORY
            .get()
            .ok_or_else(|| anyhow::anyhow!("PatchBukkitEventFactory is not cached yet"))?;
        let result = match env.call_static_method(
            factory,
            jni::jni_str!("fireEventFromBytes"),
            jni::jni_sig!("([BLjava/lang/String;)[B"),
            &[(&j_bytes).into(), (&plugin_name_jstr).into()],
        ) {
            Ok(res) => res,
            Err(e) => {
                if env.exception_check() {
                    env.exception_describe();
                    env.exception_clear();
                }
                return Err(e.into());
            }
        };

        let obj = result.l()?;
        if let Some(uuid) = leave_player_uuid {
            crate::java::native_callbacks::utils::remove_cached_player(&uuid);
        }
        if obj.is_null() {
            return Ok(FireEventResponse {
                cancelled: false,
                data: Some(payload.event),
            });
        }

        let j_byte_array = jni::objects::JByteArray::cast_local(env, obj)?;
        let resp_vec = env.convert_byte_array(&j_byte_array)?;
        let response = FireEventResponse::decode(resp_vec.as_slice())?;
        Ok(response)
    }

    pub fn ensure_player_registered_fast(
        jvm: &jni::JavaVM,
        player: &Arc<Player>,
        server: &Arc<Server>,
    ) {
        let player_permission_level = player.permission_lvl.load();
        let is_op = player_permission_level > pumpkin_util::permission::PermissionLvl::Zero
            || server
                .data
                .operator_config
                .try_read()
                .is_ok_and(|ops| ops.get_entry(&player.gameprofile.id).is_some());
        let entity_id = player.living_entity.entity.entity_id;
        let uuid = player.gameprofile.id;

        {
            let map = REGISTERED_PLAYERS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if map.get(&uuid) == Some(&(entity_id, is_op)) {
                return;
            }
        }

        let _ = jvm.attach_current_thread(|env| -> Result<()> {
            Self::ensure_player_registered(env, player, server)
        });
    }

    fn ensure_player_registered(
        env: &mut Env,
        player: &Arc<Player>,
        server: &Arc<Server>,
    ) -> Result<()> {
        // A closed session never comes back in Paper: an event or packet still in flight after the quit
        // (AuthMe gives op back on quit, which changes `is_op`) would register it again as a ghost.
        if player.client.closed() {
            return Ok(());
        }
        let player_permission_level = player.permission_lvl.load();
        let is_op = player_permission_level > pumpkin_util::permission::PermissionLvl::Zero
            || server
                .data
                .operator_config
                .try_read()
                .is_ok_and(|ops| ops.get_entry(&player.gameprofile.id).is_some());
        let entity_id = player.living_entity.entity.entity_id;
        let uuid = player.gameprofile.id;

        {
            let mut map = REGISTERED_PLAYERS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if map.get(&uuid) == Some(&(entity_id, is_op)) {
                return Ok(());
            }
            {
                let mut left = LEFT_SESSIONS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if left.get(&uuid) == Some(&entity_id) {
                    return Ok(());
                }
                left.remove(&uuid);
            }
            // Insert before calling JNI so any re-entrant event for the same player
            // during channel injection does not recursively call `register_player_raw`.
            map.insert(uuid, (entity_id, is_op));
        }

        if let Err(e) = Self::register_player_raw(env, player, entity_id, is_op) {
            if let Ok(mut map) = REGISTERED_PLAYERS.lock() {
                map.remove(&uuid);
            }
            return Err(e);
        }
        Ok(())
    }

    pub fn register_player(
        env: &mut Env,
        player: &Arc<Player>,
        server: &Arc<Server>,
    ) -> Result<()> {
        let player_permission_level = player.permission_lvl.load();
        let is_op = player_permission_level > pumpkin_util::permission::PermissionLvl::Zero
            || server
                .data
                .operator_config
                .try_read()
                .is_ok_and(|ops| ops.get_entry(&player.gameprofile.id).is_some());
        let entity_id = player.living_entity.entity.entity_id;
        Self::register_player_raw(env, player, entity_id, is_op)
    }

    fn register_player_raw(
        env: &mut Env,
        player: &Arc<Player>,
        entity_id: i32,
        is_op: bool,
    ) -> Result<()> {
        let uuid_jstr = env.new_string(player.gameprofile.id.to_string())?;
        let name_jstr = env.new_string(&player.gameprofile.name)?;

        let manager = CHANNEL_MANAGER
            .get()
            .ok_or_else(|| anyhow::anyhow!("VirtualChannelManager is not cached yet"))?;
        if let Err(e) = env.call_static_method(
            manager,
            jni::jni_str!("registerPlayer"),
            jni::jni_sig!("(Ljava/lang/String;Ljava/lang/String;ZI)V"),
            &[
                (&uuid_jstr).into(),
                (&name_jstr).into(),
                jni::objects::JValue::Bool(is_op),
                jni::objects::JValue::Int(entity_id),
            ],
        ) {
            if env.exception_check() {
                env.exception_describe();
                env.exception_clear();
            }
            return Err(e.into());
        }

        Ok(())
    }
}
