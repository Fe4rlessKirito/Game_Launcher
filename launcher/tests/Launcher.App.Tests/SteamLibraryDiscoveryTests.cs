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

    [Fact]
    public void PrefersMostRecentSteamAccountForFavorites()
    {
        var root = CreateTempDirectory();
        try
        {
            var steamApps = Path.Combine(root, "steamapps");
            Directory.CreateDirectory(Path.Combine(steamApps, "common", "Recent Game"));
            Directory.CreateDirectory(Path.Combine(steamApps, "common", "Auto Login Game"));
            File.WriteAllText(
                Path.Combine(steamApps, "appmanifest_30.acf"),
                "\"AppState\"\n{\n  \"appid\" \"30\"\n  \"name\" \"Recent Game\"\n  \"installdir\" \"Recent Game\"\n}");
            File.WriteAllText(
                Path.Combine(steamApps, "appmanifest_40.acf"),
                "\"AppState\"\n{\n  \"appid\" \"40\"\n  \"name\" \"Auto Login Game\"\n  \"installdir\" \"Auto Login Game\"\n}");

            WriteFavoriteConfig(root, "123", "40");
            WriteFavoriteConfig(root, "456", "30");
            Directory.CreateDirectory(Path.Combine(root, "config"));
            File.WriteAllText(
                Path.Combine(root, "config", "loginusers.vdf"),
                "\"users\"\n{\n"
                + "  \"76561197960265851\"\n  {\n    \"MostRecent\" \"0\"\n    \"AllowAutoLogin\" \"1\"\n  }\n"
                + "  \"76561197960266184\"\n  {\n    \"MostRecent\" \"1\"\n    \"AllowAutoLogin\" \"0\"\n  }\n}");

            var snapshot = SteamLibraryDiscovery.Discover(root);

            Assert.False(snapshot.Games.Single(game => game.AppId == "40").IsFavorite);
            Assert.True(snapshot.Games.Single(game => game.AppId == "30").IsFavorite);
        }
        finally
        {
            Directory.Delete(root, recursive: true);
        }
    }

    [Fact]
    public void ReadsLegacyLibraryFolderPathsAndRejectsInvalidManifests()
    {
        var root = CreateTempDirectory();
        var secondLibrary = CreateTempDirectory();
        try
        {
            var steamApps = Path.Combine(root, "steamapps");
            Directory.CreateDirectory(steamApps);
            var installRoot = Path.Combine(secondLibrary, "steamapps", "common", "Valid Game");
            Directory.CreateDirectory(installRoot);
            File.WriteAllText(
                Path.Combine(secondLibrary, "steamapps", "appmanifest_50.acf"),
                "\"AppState\"\n{\n  \"appid\" \"50\"\n  \"name\" \"Valid Game\"\n  \"installdir\" \"Valid Game\"\n}");
            File.WriteAllText(
                Path.Combine(steamApps, "appmanifest_invalid.acf"),
                "\"AppState\"\n{\n  \"appid\" \"not-an-app-id\"\n  \"name\" \"Invalid\"\n  \"installdir\" \"Invalid\"\n}");
            Directory.CreateDirectory(Path.Combine(steamApps, "common"));
            File.WriteAllText(
                Path.Combine(steamApps, "appmanifest_60.acf"),
                "\"AppState\"\n{\n  \"appid\" \"60\"\n  \"name\" \"Common Root\"\n  \"installdir\" \".\"\n}");
            File.WriteAllText(
                Path.Combine(steamApps, "libraryfolders.vdf"),
                "\"libraryfolders\"\n{\n"
                + "  \"0\" \"" + root.Replace("\\", "\\\\", StringComparison.Ordinal) + "\"\n"
                + "  \"1\" \"" + secondLibrary.Replace("\\", "\\\\", StringComparison.Ordinal) + "\"\n}");

            var snapshot = SteamLibraryDiscovery.Discover(root);

            var game = Assert.Single(snapshot.Games);
            Assert.Equal("50", game.AppId);
            Assert.Equal(Path.GetFullPath(installRoot), game.InstallRoot);
        }
        finally
        {
            Directory.Delete(root, recursive: true);
            Directory.Delete(secondLibrary, recursive: true);
        }
    }

    [Fact]
    public void FindsSteamExecutableFromAnExplicitRootAndValidatesAppIds()
    {
        if (!OperatingSystem.IsWindows()) return;

        var root = CreateTempDirectory();
        try
        {
            var executable = Path.Combine(root, "steam.exe");
            File.WriteAllBytes(executable, []);

            Assert.Equal(Path.GetFullPath(executable), SteamLibraryDiscovery.FindSteamExecutable(root));
            Assert.True(SteamLibraryDiscovery.IsValidAppId("480"));
            Assert.False(SteamLibraryDiscovery.IsValidAppId("0"));
            Assert.False(SteamLibraryDiscovery.IsValidAppId("not-an-app-id"));
        }
        finally
        {
            Directory.Delete(root, recursive: true);
        }
    }

    private static void WriteFavoriteConfig(string steamRoot, string accountId, string appId)
    {
        var configRoot = Path.Combine(steamRoot, "userdata", accountId, "7", "remote");
        Directory.CreateDirectory(configRoot);
        File.WriteAllText(
            Path.Combine(configRoot, "sharedconfig.vdf"),
            "\"UserRoamingConfigStore\"\n{\n  \"Software\"\n  {\n    \"Valve\"\n    {\n      \"Steam\"\n      {\n        \"apps\"\n        {\n          \"" + appId + "\"\n          {\n            \"tags\"\n            {\n              \"0\" \"favorite\"\n            }\n          }\n        }\n      }\n    }\n  }\n}");
    }

    private static string CreateTempDirectory()
    {
        var path = Path.Combine(Path.GetTempPath(), "vaultnode-steam-test-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(path);
        return path;
    }
}
