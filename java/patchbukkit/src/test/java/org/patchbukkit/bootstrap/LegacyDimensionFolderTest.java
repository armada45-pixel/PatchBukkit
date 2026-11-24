package org.patchbukkit.bootstrap;

import static org.junit.jupiter.api.Assertions.*;

import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import org.bukkit.World;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;

class LegacyDimensionFolderTest {

    @Test
    void netherFolderGetsSelfLinkSoPumpkinFindsItsRegions(@TempDir Path dir) throws Exception {
        Files.createDirectories(dir.resolve("region"));
        ServerBridge.linkLegacyDimensionFolder(dir, World.Environment.NETHER);
        assertTrue(Files.isSymbolicLink(dir.resolve("DIM-1")));
        assertTrue(Files.isDirectory(dir.resolve("DIM-1/region")));
        // Running again keeps the existing link.
        ServerBridge.linkLegacyDimensionFolder(dir, World.Environment.NETHER);
    }

    @Test
    void overworldAndEmptyFoldersAreLeftAlone(@TempDir Path dir) throws Exception {
        ServerBridge.linkLegacyDimensionFolder(dir, World.Environment.THE_END);
        assertFalse(Files.exists(dir.resolve("DIM1"), LinkOption.NOFOLLOW_LINKS));
        Files.createDirectories(dir.resolve("region"));
        ServerBridge.linkLegacyDimensionFolder(dir, World.Environment.NORMAL);
        assertEquals(1, Files.list(dir).count());
    }
}
