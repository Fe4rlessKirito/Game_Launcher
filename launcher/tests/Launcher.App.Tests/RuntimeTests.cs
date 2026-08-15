using System.Net;
using System.Net.Http;
using Launcher.App.Runtime;
using Launcher.App.ViewModels;
using Launcher.Core;
using Launcher.Networking;
using Microsoft.Data.Sqlite;

namespace Launcher.App.Tests;

public sealed class RuntimeTests
{
    [Theory]
    [InlineData("http://127.0.0.1:8080")]
    [InlineData("http://5.231.32.191")]
    [InlineData("https://5.231.32.191/")]
    public void SettingsMigrateLegacyApiEndpoints(string legacyEndpoint)
    {
        var directory = Path.Combine(Path.GetTempPath(), "vaultnode-settings-test", Guid.NewGuid().ToString("N"));
        var path = Path.Combine(directory, "settings.json");
        Directory.CreateDirectory(directory);
        try
        {
            File.WriteAllText(path, $$"""{"apiBaseUrl":"{{legacyEndpoint}}"}""");

            var settings = new SettingsViewModel(path);

            Assert.Equal("https://vaultnode.pp.ua", settings.ApiBaseUrl);
            Assert.Contains("\"apiBaseUrl\": \"https://vaultnode.pp.ua\"", File.ReadAllText(path));
        }
        finally
        {
            if (Directory.Exists(directory)) Directory.Delete(directory, recursive: true);
        }
    }

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

    [Fact]
    public async Task ApiClientLoadsEveryCatalogPage()
    {
        using var client = new HttpClient(new PagedCatalogHandler());
        var api = new LauncherApiClient(client, new Uri("http://launcher/"));

        var games = await api.GetGamesAsync();

        Assert.Equal(["Game A", "Game B"], games.Select(game => game.Title));
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

    private sealed class PagedCatalogHandler : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            var secondPage = request.RequestUri?.Query.Contains("cursor=1", StringComparison.Ordinal) == true;
            var body = secondPage
                ? "{\"items\":[{\"id\":\"game-b\",\"slug\":\"game-b\",\"title\":\"Game B\",\"description\":\"B\",\"hero_image_url\":null,\"cover_image_url\":null,\"latest_build\":null}],\"next_cursor\":null}"
                : "{\"items\":[{\"id\":\"game-a\",\"slug\":\"game-a\",\"title\":\"Game A\",\"description\":\"A\",\"hero_image_url\":null,\"cover_image_url\":null,\"latest_build\":null}],\"next_cursor\":\"1\"}";
            return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK) { Content = new StringContent(body) });
        }
    }
}
