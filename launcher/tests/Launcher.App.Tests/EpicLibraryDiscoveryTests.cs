using System.Text.Json;
using Launcher.App.Runtime;

namespace Launcher.App.Tests;

public sealed class EpicLibraryDiscoveryTests
{
    [Fact]
    public void DiscoversInstalledGamesFromEpicItemManifest()
    {
        var root = CreateTempDirectory();
        try
        {
            var installRoot = Path.Combine(root, "Hades II");
            Directory.CreateDirectory(Path.Combine(installRoot, "Binaries", "Win64"));
            var manifest = Path.Combine(root, "hades.item");
            WriteManifest(
                manifest,
                appName: "Hades2",
                displayName: "Hades II",
                installRoot,
                launchExecutable: @"Binaries\Win64\Hades2.exe",
                installSize: "123456");

            var snapshot = EpicLibraryDiscovery.Discover(root);

            var game = Assert.Single(snapshot.Games);
            Assert.Equal("Hades2", game.AppName);
            Assert.Equal("Hades II", game.Name);
            Assert.Equal(Path.GetFullPath(installRoot), game.InstallRoot);
            Assert.Equal(@"Binaries\Win64\Hades2.exe", game.LaunchExecutable);
            Assert.Equal(123456, game.SizeBytes);
            Assert.Equal(Path.GetFullPath(manifest), game.ManifestPath);
            Assert.Contains(Path.GetFullPath(root), snapshot.ManifestRoots);
            Assert.Null(snapshot.Error);
        }
        finally
        {
            Directory.Delete(root, recursive: true);
        }
    }

    [Fact]
    public void RejectsManifestTraversalAndMissingInstallDirectories()
    {
        var root = CreateTempDirectory();
        try
        {
            var installRoot = Path.Combine(root, "Valid");
            Directory.CreateDirectory(installRoot);
            WriteManifest(
                Path.Combine(root, "traversal.item"),
                appName: "Traversal",
                displayName: "Traversal",
                installRoot,
                launchExecutable: @"..\outside.exe",
                installSize: 100);
            WriteManifest(
                Path.Combine(root, "missing.item"),
                appName: "Missing",
                displayName: "Missing",
                Path.Combine(root, "DoesNotExist"),
                launchExecutable: "Missing.exe",
                installSize: 100);

            var snapshot = EpicLibraryDiscovery.Discover(root);

            Assert.Empty(snapshot.Games);
        }
        finally
        {
            Directory.Delete(root, recursive: true);
        }
    }

    [Fact]
    public void DeduplicatesAppNamesAndValidatesEpicIds()
    {
        var root = CreateTempDirectory();
        try
        {
            var installRoot = Path.Combine(root, "Game");
            Directory.CreateDirectory(installRoot);
            WriteManifest(Path.Combine(root, "first.item"), "DemoGame", "First name", installRoot, "Game.exe", 10);
            WriteManifest(Path.Combine(root, "second.item"), "demogame", "Second name", installRoot, "Game.exe", 20);

            var snapshot = EpicLibraryDiscovery.Discover(root);

            Assert.Single(snapshot.Games);
            Assert.True(EpicLibraryDiscovery.IsValidAppName("DemoGame_2.0"));
            Assert.False(EpicLibraryDiscovery.IsValidAppName("Demo/Game"));
            Assert.False(EpicLibraryDiscovery.IsValidAppName(""));
        }
        finally
        {
            Directory.Delete(root, recursive: true);
        }
    }

    private static void WriteManifest(
        string path,
        string appName,
        string displayName,
        string installRoot,
        string launchExecutable,
        object installSize)
    {
        var escapedInstallRoot = installRoot.Replace("\\", "\\\\", StringComparison.Ordinal);
        var escapedExecutable = launchExecutable.Replace("\\", "\\\\", StringComparison.Ordinal);
        File.WriteAllText(
            path,
            $$"""{"AppName":"{{appName}}","DisplayName":"{{displayName}}","InstallLocation":"{{escapedInstallRoot}}","LaunchExecutable":"{{escapedExecutable}}","InstallSize":{{JsonSerializer.Serialize(installSize)}}}""");
    }

    private static string CreateTempDirectory()
    {
        var path = Path.Combine(Path.GetTempPath(), "vaultnode-epic-test-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(path);
        return path;
    }
}
