using System.Net;
using System.Security.Cryptography;
using System.Text;
using Launcher.App.Runtime;

namespace Launcher.App.Tests;

public sealed class LauncherUpdateServiceTests
{
    [Theory]
    [InlineData("v0.2.0", "0.1.0", true)]
    [InlineData("0.1.0", "v0.1.0", false)]
    [InlineData("0.1.0", "0.1.0-rc.1", true)]
    [InlineData("0.1.0-rc.2", "0.1.0-rc.10", false)]
    [InlineData("0.1.0-rc.10", "0.1.0-rc.2", true)]
    public void VersionComparisonFollowsReleaseOrdering(string candidate, string current, bool expected)
    {
        Assert.Equal(expected, LauncherUpdateService.IsNewerVersion(candidate, current));
    }

    [Fact]
    public async Task ReleaseMetadataAndInstallerChecksumAreVerified()
    {
        var payload = Encoding.UTF8.GetBytes("verified installer fixture");
        var digest = Convert.ToHexString(SHA256.HashData(payload));
        var releaseJson = $$"""
            {
              "tag_name": "v0.2.0",
              "html_url": "https://github.com/Fe4rlessKirito/Game_Launcher/releases/tag/v0.2.0",
              "draft": false,
              "prerelease": false,
              "assets": [
                {
                  "name": "Vaultnode-Setup.exe",
                  "browser_download_url": "https://github.com/Fe4rlessKirito/Game_Launcher/releases/download/v0.2.0/Vaultnode-Setup.exe",
                  "digest": "sha256:{{digest}}",
                  "size": {{payload.Length}}
                }
              ]
            }
            """;
        using var client = new HttpClient(new ReleaseHandler(releaseJson, payload));
        var service = new LauncherUpdateService(client, "0.1.0");

        var update = await service.CheckAsync();
        Assert.NotNull(update);
        Assert.Equal("v0.2.0", update!.Version);
        Assert.Equal(digest, update.Sha256);

        var installerPath = await service.DownloadInstallerAsync(update);
        try
        {
            Assert.Equal(payload, await File.ReadAllBytesAsync(installerPath));
        }
        finally
        {
            if (File.Exists(installerPath)) File.Delete(installerPath);
        }
    }

    private sealed class ReleaseHandler(string releaseJson, byte[] installerBytes) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            if (request.RequestUri?.AbsolutePath.Contains("/releases/latest", StringComparison.Ordinal) == true)
            {
                return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
                {
                    Content = new StringContent(releaseJson, Encoding.UTF8, "application/json"),
                });
            }

            return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new ByteArrayContent(installerBytes),
            });
        }
    }
}
