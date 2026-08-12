using System.Net;
using System.Net.Http;
using Launcher.App.Runtime;
using Launcher.Core;
using Microsoft.Data.Sqlite;

namespace Launcher.App.Tests;

public sealed class RuntimeTests
{
    [Fact]
    public async Task RuntimeHydratesCatalogAndDerivesInstallState()
    {
        var root = Path.Combine(Path.GetTempPath(), "vaultnode-runtime-test", Guid.NewGuid().ToString("N"));
        using var client = new HttpClient(new CatalogHandler()) { BaseAddress = new Uri("http://launcher/") };
        var runtime = new LauncherRuntime(new LauncherSettings(ApiBaseUrl: "http://launcher/"), root, client);

        try
        {
            var snapshot = await runtime.InitializeAsync();

            Assert.True(snapshot.IsOnline);
            var game = Assert.Single(snapshot.Games);
            Assert.Equal("Game A", game.Title);
            Assert.Equal("build-a", game.BuildId);
            Assert.Equal(GameState.NotInstalled, game.State);
            Assert.Equal("Ready to install", game.StatusText);
        }
        finally
        {
            await runtime.DisposeAsync();
            SqliteConnection.ClearAllPools();
            if (Directory.Exists(root)) Directory.Delete(root, recursive: true);
        }
    }

    private sealed class CatalogHandler : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            if (request.RequestUri?.AbsolutePath == "/api/v1/games")
            {
                return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
                {
                    Content = new StringContent("{\"items\":[{\"id\":\"game-a\",\"slug\":\"game-a\",\"title\":\"Game A\",\"description\":\"A test game\",\"hero_image_url\":null,\"cover_image_url\":null,\"latest_build\":{\"id\":\"build-a\",\"game_id\":\"game-a\",\"display_version\":\"1.0.0\",\"size_bytes\":3,\"published_at\":null}}],\"next_cursor\":null}")
                });
            }

            return Task.FromResult(new HttpResponseMessage(HttpStatusCode.NotFound));
        }
    }
}
