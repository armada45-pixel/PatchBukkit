//! World UUIDs as Bukkit sees them.
//!
//! Pumpkin gives every world a new random UUID on each start, but plugins store world UUIDs
//! (EssentialsX homes and spawns, for example). Bukkit therefore sees the UUID Paper keeps in
//! `<dimension>/data/paper/metadata.dat`, written there when a world has none, so it stays the
//! same across restarts and matches the world's UUID on Paper.

use std::{
    collections::HashMap,
    fs::{self, File},
    path::Path,
    sync::{LazyLock, RwLock},
};

use pumpkin::world::World;
use pumpkin_nbt::{
    compound::NbtCompound,
    nbt_compress::{read_gzip_compound_tag, write_gzip_compound_tag},
    tag::NbtTag,
};
use uuid::Uuid;

/// Pumpkin world UUID -> Bukkit world UUID.
static BUKKIT_IDS: LazyLock<RwLock<HashMap<Uuid, Uuid>>> = LazyLock::new(RwLock::default);

/// The UUID Bukkit uses for `world`.
pub fn bukkit_uuid(world: &World) -> Uuid {
    if let Some(id) = BUKKIT_IDS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&world.uuid)
    {
        return *id;
    }
    let file = world
        .level
        .level_folder
        .dim_folder
        .join("data/paper/metadata.dat");
    let data_version = world.level_info.load().data_version;
    let id = load_or_create(&file, data_version).unwrap_or_else(|e| {
        tracing::warn!(
            "Cannot read or write {}: {e}; this world's UUID will change on restart",
            file.display()
        );
        world.uuid
    });
    *BUKKIT_IDS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(world.uuid)
        .or_insert(id)
}

/// Whether `id` names `world`, as a Bukkit UUID or as Pumpkin's own.
pub fn is_world(world: &World, id: Uuid) -> bool {
    world.uuid == id || bukkit_uuid(world) == id
}

fn load_or_create(file: &Path, data_version: i32) -> anyhow::Result<Uuid> {
    if file.exists() {
        let root = read_gzip_compound_tag(File::open(file)?)?;
        return root
            .get_compound("data")
            .and_then(|d| d.get_int_array("uuid"))
            .and_then(uuid_from_ints)
            .ok_or_else(|| anyhow::anyhow!("no data.uuid int array"));
    }
    let id = Uuid::new_v4();
    let mut data = NbtCompound::new();
    data.put("uuid", NbtTag::IntArray(uuid_to_ints(id).to_vec()));
    let mut root = NbtCompound::new();
    root.put_int("DataVersion", data_version);
    root.put_compound("data", data);
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    write_gzip_compound_tag(root, File::create(file)?)?;
    Ok(id)
}

/// Minecraft's `UUIDUtil` int-array form: most significant int first.
fn uuid_to_ints(id: Uuid) -> [i32; 4] {
    let (msb, lsb) = id.as_u64_pair();
    [
        (msb >> 32) as i32,
        msb as i32,
        (lsb >> 32) as i32,
        lsb as i32,
    ]
}

fn uuid_from_ints(ints: &[i32]) -> Option<Uuid> {
    let [a, b, c, d] = *ints else { return None };
    let msb = (u64::from(a as u32) << 32) | u64::from(b as u32);
    let lsb = (u64::from(c as u32) << 32) | u64::from(d as u32);
    Some(Uuid::from_u64_pair(msb, lsb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_array_round_trip_matches_minecraft() {
        // From a Paper 26.3 overworld metadata.dat.
        let ints = [-880_202_094, -134_985_934, -1_615_284_261, 1_710_263_848];
        let id = uuid_from_ints(&ints).unwrap();
        assert_eq!(id.to_string(), "cb892e92-f7f4-4732-9fb8-b7db65f08e28");
        assert_eq!(uuid_to_ints(id), ints);
        assert_eq!(uuid_from_ints(&ints[..3]), None);
    }

    #[test]
    fn creates_then_reads_the_same_uuid() {
        let dir = std::env::temp_dir().join(format!("pb-world-ids-{}", Uuid::new_v4()));
        let file = dir.join("data/paper/metadata.dat");
        let id = load_or_create(&file, 5023).unwrap();
        assert_eq!(load_or_create(&file, 5023).unwrap(), id);
        let _ = fs::remove_dir_all(dir);
    }
}
