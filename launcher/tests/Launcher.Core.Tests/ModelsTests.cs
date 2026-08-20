using Launcher.Core;

namespace Launcher.Core.Tests;

public class ModelsTests
{
    [Fact]
    public void DownloadProgressComputesBoundedFraction() => Assert.Equal(0.5, new DownloadProgress("job", DownloadJobState.Downloading, 50, 100, 0, null).Fraction);

    [Fact]
    public async Task SettingsStoreUsesAnAtomicJsonFile()
    {
        var path = Path.Combine(Path.GetTempPath(), "launcher-settings-" + Guid.NewGuid().ToString("N"), "settings.json");
        try
        {
            var store = new JsonSettingsStore(path);
            await store.SaveAsync(new LauncherSettings(
                LaunchOnStartup: true,
                ConcurrentDownloads: 8,
                ApiBaseUrl: "https://staging.example.invalid/",
                TrustedManifestKeysPem: new Dictionary<string, string>
                {
                    ["staging-2026-01"] = "-----BEGIN PUBLIC KEY-----\nredacted\n-----END PUBLIC KEY-----"
                },
                RequireTrustedManifestKeys: true));
            var settings = await store.LoadAsync();
            Assert.True(settings.LaunchOnStartup);
            Assert.Equal(8, settings.ConcurrentDownloads);
            Assert.Equal("https://staging.example.invalid/", settings.ApiBaseUrl);
            Assert.Contains("staging-2026-01", settings.TrustedManifestKeysPem!.Keys);
            Assert.True(settings.RequireTrustedManifestKeys);

            await store.SaveAsync(settings with
            {
                BackgroundImagePath = "C:\\Users\\Public\\Pictures\\vaultnode.png",
                AutomaticUpdatesEnabled = false,
                InterfaceTransparency = 0.35,
                BackgroundImageOpacity = 0.80,
            });
            var presentationSettings = await store.LoadAsync();
            Assert.Equal("C:\\Users\\Public\\Pictures\\vaultnode.png", presentationSettings.BackgroundImagePath);
            Assert.False(presentationSettings.AutomaticUpdatesEnabled);
            Assert.Equal(0.35, presentationSettings.InterfaceTransparency);
            Assert.Equal(0.80, presentationSettings.BackgroundImageOpacity);
        }
        finally
        {
            var directory = Path.GetDirectoryName(path);
            if (directory is not null && Directory.Exists(directory)) Directory.Delete(directory, true);
        }
    }
}
