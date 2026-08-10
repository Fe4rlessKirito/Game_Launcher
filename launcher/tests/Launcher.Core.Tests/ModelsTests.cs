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
                }));
            var settings = await store.LoadAsync();
            Assert.True(settings.LaunchOnStartup);
            Assert.Equal(8, settings.ConcurrentDownloads);
            Assert.Equal("https://staging.example.invalid/", settings.ApiBaseUrl);
            Assert.Contains("staging-2026-01", settings.TrustedManifestKeysPem!.Keys);
        }
        finally
        {
            var directory = Path.GetDirectoryName(path);
            if (directory is not null && Directory.Exists(directory)) Directory.Delete(directory, true);
        }
    }
}
