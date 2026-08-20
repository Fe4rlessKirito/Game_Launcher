using Launcher.App.Runtime;
using Launcher.App.ViewModels;
using Launcher.Core;

namespace Launcher.App.Tests;

public sealed class OptionalStoreTests
{
    [Fact]
    public void OptionalStoresAreDisabledByDefault()
    {
        var snapshots = OptionalStoreDiscovery.Discover(new Dictionary<OptionalStoreProvider, bool>());

        Assert.Equal(Enum.GetValues<OptionalStoreProvider>().Length, snapshots.Count);
        Assert.All(snapshots, snapshot =>
        {
            Assert.False(snapshot.Enabled);
            Assert.Equal("Disabled", snapshot.StatusText);
            Assert.Empty(snapshot.Games);
        });
        Assert.False(new LauncherSettings().GogIntegrationEnabled);
        Assert.False(new LauncherSettings().UbisoftIntegrationEnabled);
        Assert.False(new LauncherSettings().EaIntegrationEnabled);
        Assert.False(new LauncherSettings().BattleNetIntegrationEnabled);
        Assert.False(new LauncherSettings().XboxIntegrationEnabled);
        Assert.False(new LauncherSettings().ItchIntegrationEnabled);
    }

    [Fact]
    public void DisabledProviderDoesNotInspectItsInstallRoot()
    {
        var root = CreateTempDirectory();
        try
        {
            var install = Directory.CreateDirectory(Path.Combine(root, "Example Game")).FullName;
            File.WriteAllText(Path.Combine(install, "ExampleGame.exe"), string.Empty);

            var snapshot = OptionalStoreDiscovery.Discover(OptionalStoreProvider.EaApp, false, [root]);

            Assert.False(snapshot.Enabled);
            Assert.False(snapshot.IsDetected);
            Assert.Empty(snapshot.InstallRoots);
            Assert.Empty(snapshot.Games);
        }
        finally
        {
            DeleteTempDirectory(root);
        }
    }

    [Fact]
    public void EnabledDirectoryProviderFindsAPlayableLocalInstall()
    {
        var root = CreateTempDirectory();
        try
        {
            var install = Directory.CreateDirectory(Path.Combine(root, "Example Game")).FullName;
            var executable = Path.Combine(install, "ExampleGame.exe");
            File.WriteAllText(executable, string.Empty);

            var snapshot = OptionalStoreDiscovery.Discover(OptionalStoreProvider.EaApp, true, [root]);

            var game = Assert.Single(snapshot.Games);
            Assert.True(snapshot.Enabled);
            Assert.True(snapshot.IsDetected);
            Assert.Equal("Example Game", game.Name);
            Assert.Equal(install, game.InstallRoot);
            Assert.Equal(executable, game.LaunchPath);
            Assert.Equal(OptionalStoreProvider.EaApp, game.Provider);
        }
        finally
        {
            DeleteTempDirectory(root);
        }
    }

    [Fact]
    public void EnabledGogProviderUsesItsManifestLaunchTarget()
    {
        var root = CreateTempDirectory();
        try
        {
            var install = Directory.CreateDirectory(Path.Combine(root, "GOG Demo")).FullName;
            var executable = Path.Combine(install, "gog-demo.exe");
            var manifest = Path.Combine(install, "goggame-123.info");
            File.WriteAllText(executable, string.Empty);
            File.WriteAllText(manifest, """
                {
                  "gameId": "123",
                  "name": "GOG Demo",
                  "installSize": 4096,
                  "playTasks": [
                    { "isPrimary": true, "path": "gog-demo.exe" }
                  ]
                }
                """);

            var snapshot = OptionalStoreDiscovery.Discover(OptionalStoreProvider.Gog, true, [root]);

            var game = Assert.Single(snapshot.Games);
            Assert.Equal("123", game.AppId);
            Assert.Equal("GOG Demo", game.Name);
            Assert.Equal(executable, game.LaunchPath);
            Assert.Equal(4096, game.SizeBytes);
            Assert.Equal(manifest, game.MetadataPath);
        }
        finally
        {
            DeleteTempDirectory(root);
        }
    }

    [Fact]
    public void OptionalStoreSettingsPersistAsOptInFlags()
    {
        var root = CreateTempDirectory();
        try
        {
            var settings = new SettingsViewModel(Path.Combine(root, "settings.json"))
            {
                GogIntegrationEnabled = true,
                UbisoftIntegrationEnabled = true,
                EaIntegrationEnabled = true,
                BattleNetIntegrationEnabled = true,
                XboxIntegrationEnabled = true,
                ItchIntegrationEnabled = true
            };

            settings.SaveCommand.Execute(null);
            var loaded = new SettingsViewModel(Path.Combine(root, "settings.json"));

            Assert.True(loaded.GogIntegrationEnabled);
            Assert.True(loaded.UbisoftIntegrationEnabled);
            Assert.True(loaded.EaIntegrationEnabled);
            Assert.True(loaded.BattleNetIntegrationEnabled);
            Assert.True(loaded.XboxIntegrationEnabled);
            Assert.True(loaded.ItchIntegrationEnabled);
        }
        finally
        {
            DeleteTempDirectory(root);
        }
    }

    private static string CreateTempDirectory()
    {
        var root = Path.Combine(Path.GetTempPath(), "vaultnode-optional-store-test", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);
        return root;
    }

    private static void DeleteTempDirectory(string root)
    {
        if (Directory.Exists(root)) Directory.Delete(root, recursive: true);
    }
}
