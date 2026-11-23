package org.patchbukkit.bootstrap;

import org.bukkit.World;
import org.junit.jupiter.api.Test;

import static org.junit.jupiter.api.Assertions.assertEquals;

class WorldNameTest {

    @Test
    void dimensionsSharingTheOverworldNameGetPaperNames() {
        assertEquals("world_nether", PumpkinDedicatedServer.bukkitWorldName("world", "world", World.Environment.NETHER));
        assertEquals("world_the_end", PumpkinDedicatedServer.bukkitWorldName("world", "world", World.Environment.THE_END));
        assertEquals("world_nether", PumpkinDedicatedServer.bukkitWorldName("", "world", World.Environment.NETHER));
    }

    @Test
    void distinctNamesAreKept() {
        assertEquals("hell", PumpkinDedicatedServer.bukkitWorldName("hell", "world", World.Environment.NETHER));
        assertEquals("lobby", PumpkinDedicatedServer.bukkitWorldName("lobby", "world", World.Environment.NORMAL));
    }
}
