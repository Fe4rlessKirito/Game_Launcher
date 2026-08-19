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

    [Fact]
    public void ReadsSteamFavoriteTagFromUserConfig()
    {
        var root = CreateTempDirectory();
        try
        {
            var steamApps = Path.Combine(root, "steamapps");
            Directory.CreateDirectory(Path.Combine(steamApps, "common", "Favorite Game"));
            Directory.CreateDirectory(Path.Combine(steamApps, "common", "Other Game"));
            File.WriteAllText(
                Path.Combine(steamApps, "appmanifest_10.acf"),
                "\"AppState\"\n{\n  \"appid\" \"10\"\n  \"name\" \"Favorite Game\"\n  \"installdir\" \"Favorite Game\"\n}");
            File.WriteAllText(
                Path.Combine(steamApps, "appmanifest_20.acf"),
                "\"AppState\"\n{\n  \"appid\" \"20\"\n  \"name\" \"Other Game\"\n  \"installdir\" \"Other Game\"\n}");

            var configRoot = Path.Combine(root, "userdata", "123", "7", "remote");
            Directory.CreateDirectory(configRoot);
            File.WriteAllText(
                Path.Combine(configRoot, "sharedconfig.vdf"),
                "\"UserRoamingConfigStore\"\n{\n  \"Software\"\n  {\n    \"Valve\"\n    {\n      \"Steam\"\n      {\n        \"apps\"\n        {\n          \"10\"\n          {\n            \"tags\"\n            {\n              \"0\" \"favorite\"\n            }\n          }\n          \"20\"\n          {\n            \"tags\"\n            {\n              \"0\" \"RPG\"\n            }\n          }\n        }\n      }\n    }\n  }\n}");

            var otherConfigRoot = Path.Combine(root, "userdata", "456", "7", "remote");
            Directory.CreateDirectory(otherConfigRoot);
            File.WriteAllText(
                Path.Combine(otherConfigRoot, "sharedconfig.vdf"),
                "\"UserRoamingConfigStore\"\n{\n  \"Software\"\n  {\n    \"Valve\"\n    {\n      \"Steam\"\n      {\n        \"apps\"\n        {\n          \"20\"\n          {\n            \"tags\"\n            {\n              \"0\" \"favorite\"\n            }\n          }\n        }\n      }\n    }\n  }\n}");
            Directory.CreateDirectory(Path.Combine(root, "config"));
            File.WriteAllText(
                Path.Combine(root, "config", "loginusers.vdf"),
                "\"users\"\n{\n  \"76561197960265851\"\n  {\n    \"AutoLogin\" \"1\"\n  }\n  \"76561197960266184\"\n  {\n    \"AutoLogin\" \"0\"\n  }\n}");

            var snapshot = SteamLibraryDiscovery.Discover(root);

            Assert.True(snapshot.Games.Single(game => game.AppId == "10").IsFavorite);
            Assert.False(snapshot.Games.Single(game => game.AppId == "20").IsFavorite);
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
