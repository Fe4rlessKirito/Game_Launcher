using Launcher.App.Runtime;

namespace Launcher.App.Tests;

public sealed class SteamLibraryDiscoveryTests
{
    [Fact]
    public void DiscoversInstalledGamesFromSteamMetadata()
    {
        var root = CreateTempDirectory();
        try
        {
            var steamApps = Path.Combine(root, "steamapps");
            var installRoot = Path.Combine(steamApps, "common", "Spacewar");
            Directory.CreateDirectory(installRoot);
            var logoPath = Path.Combine(root, "appcache", "librarycache", "480", "logo.png");
            Directory.CreateDirectory(Path.GetDirectoryName(logoPath)!);
            File.WriteAllBytes(logoPath, []);

            var escapedRoot = root.Replace("\\", "\\\\", StringComparison.Ordinal);
            File.WriteAllText(
                Path.Combine(steamApps, "libraryfolders.vdf"),
                "\"libraryfolders\"\n{\n  \"0\"\n  {\n    \"path\" \"" + escapedRoot + "\"\n  }\n}");
            File.WriteAllText(
                Path.Combine(steamApps, "appmanifest_480.acf"),
                "\"AppState\"\n{\n  \"appid\" \"480\"\n  \"name\" \"Spacewar\"\n  \"installdir\" \"Spacewar\"\n  \"SizeOnDisk\" \"123456\"\n}");

            var snapshot = SteamLibraryDiscovery.Discover(root);

            var game = Assert.Single(snapshot.Games);
            Assert.Equal("480", game.AppId);
            Assert.Equal("Spacewar", game.Name);
            Assert.Equal(Path.GetFullPath(installRoot), game.InstallRoot);
            Assert.Equal(123456, game.SizeBytes);
            Assert.Equal(Path.GetFullPath(logoPath), game.IconArtworkPath);
            Assert.Contains(Path.GetFullPath(root), snapshot.LibraryRoots);
            Assert.Null(snapshot.Error);
        }
        finally
        {
            Directory.Delete(root, recursive: true);
        }
    }

    [Fact]
    public void IgnoresManifestPathTraversal()
    {
        var root = CreateTempDirectory();
        try
        {
            var steamApps = Path.Combine(root, "steamapps");
            Directory.CreateDirectory(Path.Combine(steamApps, "common"));
            File.WriteAllText(
                Path.Combine(steamApps, "appmanifest_999.acf"),
                "\"AppState\"\n{\n  \"appid\" \"999\"\n  \"name\" \"Outside\"\n  \"installdir\" \"..\\\\outside\"\n}");

            var snapshot = SteamLibraryDiscovery.Discover(root);

            Assert.Empty(snapshot.Games);
        }
        finally
        {
            Directory.Delete(root, recursive: true);
        }
    }

    private static string CreateTempDirectory()
    {
        var path = Path.Combine(Path.GetTempPath(), "vaultnode-steam-test-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(path);
        return path;
    }
}
