using Launcher.Downloads;
using Launcher.Installation;
using Launcher.Manifests;
using Launcher.Security;
using Launcher.Storage;
using Microsoft.Data.Sqlite;

namespace Launcher.Installation.Tests;

public class InstallerIntegrationTests
{
    [Fact]
    public async Task InstallsAndVerifiesACompressedChunkTransactionally()
    {
        var root = Path.Combine(Path.GetTempPath(), "launcher-install-test-" + Guid.NewGuid().ToString("N"));
        var cacheRoot = Path.Combine(root, "cache");
        var installRoot = Path.Combine(root, "install");
        var databasePath = Path.Combine(root, "launcher.db");
        Directory.CreateDirectory(root);
        try
        {
            var raw = System.Text.Encoding.UTF8.GetBytes("synthetic launcher installation bytes");
            var rawHash = Hashing.ComputeHash(raw);
            using var compressor = new ZstdSharp.Compressor(3);
            var encoded = compressor.Wrap(raw).ToArray();
            var encodedHash = Hashing.ComputeHash(encoded);
            var cache = new ChunkCache(cacheRoot, 1024 * 1024);
            await cache.InitializeAsync();
            await cache.PutAsync(encodedHash, encoded);
            var manifest = new Manifest(
                1,
                "manifest-test",
                "synthetic-game",
                "build-test",
                "1.0.0",
                DateTimeOffset.UtcNow,
                ChunkingConfig.Default,
                EncodingConfig.Default,
                [new FileRecipe("Game/SyntheticGame.exe", raw.Length, rawHash, [new ChunkReference(rawHash, raw.Length, encodedHash, encoded.Length, $"chunks/encoded/{encodedHash}.bin")])],
                new LaunchProfile("Game/SyntheticGame.exe", "Game", [], new Dictionary<string, string>()));
            var state = new LocalStateStore(databasePath);
            await state.InitializeAsync();
            var installer = new Installer(cache, state);
            await installer.InstallAsync(manifest, installRoot);

            Assert.Equal(raw, await File.ReadAllBytesAsync(Path.Combine(installRoot, "Game", "SyntheticGame.exe")));
            Assert.Empty(await Installer.VerifyAsync(manifest, installRoot));
            Assert.Single(await state.GetInstalledGamesAsync());
        }
        finally
        {
            SqliteConnection.ClearAllPools();
            if (Directory.Exists(root)) Directory.Delete(root, true);
        }
    }
}
