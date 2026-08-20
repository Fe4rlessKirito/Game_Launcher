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
        var runtime = new LauncherRuntime(
            new LauncherSettings(ApiBaseUrl: "http://launcher/"),
            root,
            client,
            steamDiscovery: () => SteamLibrarySnapshot.Empty,
            epicDiscovery: () => EpicLibrarySnapshot.Empty);

        try
        {
            var snapshot = await runtime.InitializeAsync();

            Assert.True(snapshot.IsOnline);
            var game = Assert.Single(snapshot.Games);
            Assert.Equal("Game A", game.Title);
            Assert.Equal("build-a", game.BuildId);
            Assert.Equal(GameState.NotInstalled, game.State);
            Assert.Equal("Ready to install", game.StatusText);

            var removed = await runtime.RemoveFromLibraryAsync("game-a");
            Assert.Contains("game-a", removed.ExcludedGameIds!);

            var refreshed = await runtime.RefreshAsync();
            Assert.Contains("game-a", refreshed.ExcludedGameIds!);
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

    [Fact]
    public async Task CachedSteamOwnedGamesAreAvailableOfflineAndNotMarkedInstalled()
    {
        var root = Path.Combine(Path.GetTempPath(), "vaultnode-steam-owned-test", Guid.NewGuid().ToString("N"));
        using var client = new HttpClient(new EmptyCatalogHandler());
        var runtime = new LauncherRuntime(
            new LauncherSettings(
                ApiBaseUrl: "http://launcher/",
                SteamId64: "76561197960265729",
                SteamOwnedGames: [new SteamOwnedGame("730", "Counter-Strike 2", 120, HeaderUrl: "https://cdn.example/header.jpg")]),
            root,
            client,
            steamDiscovery: () => SteamLibrarySnapshot.Empty,
            epicDiscovery: () => EpicLibrarySnapshot.Empty);

        try
        {
            var snapshot = await runtime.InitializeAsync();

            var game = Assert.Single(snapshot.Games);
            Assert.Equal("steam:730", game.Id);
            Assert.True(game.IsSteamGame);
            Assert.False(game.IsInstalled);
            Assert.Equal("Available in Steam", game.StatusText);
            Assert.NotNull(game.SteamOwned);
            Assert.Equal("76561197960265729", snapshot.Steam?.ConnectedAccount?.SteamId64);
        }
        finally
        {
            await runtime.DisposeAsync();
            SqliteConnection.ClearAllPools();
            if (Directory.Exists(root)) Directory.Delete(root, recursive: true);
        }
    }

    [Fact]
    public async Task EpicInstalledGamesAreAvailableLocallyAndStayOutsideVaultnodeStore()
    {
        var root = Path.Combine(Path.GetTempPath(), "vaultnode-epic-runtime-test", Guid.NewGuid().ToString("N"));
        var installRoot = Path.Combine(root, "EpicGame");
        Directory.CreateDirectory(installRoot);
        using var client = new HttpClient(new EmptyCatalogHandler());
        var epicGame = new EpicGameInstall(
            "EpicDemo",
            "Epic Demo",
            installRoot,
            "EpicDemo.exe",
            2048,
            Path.Combine(root, "EpicDemo.item"));
        var runtime = new LauncherRuntime(
            new LauncherSettings(ApiBaseUrl: "http://launcher/"),
            root,
            client,
            steamDiscovery: () => SteamLibrarySnapshot.Empty,
            epicDiscovery: () => new EpicLibrarySnapshot([Path.Combine(root, "manifests")], [epicGame], null));

        try
        {
            var snapshot = await runtime.InitializeAsync();

            var game = Assert.Single(snapshot.Games);
            Assert.Equal("epic:EpicDemo", game.Id);
            Assert.True(game.IsEpicGame);
            Assert.True(game.IsExternalStoreGame);
            Assert.True(game.IsInstalled);
            Assert.Equal("Installed", game.StatusText);
            Assert.Equal("Epic Games", game.DisplayVersion);
            Assert.Same(epicGame, game.EpicInstall);
            Assert.NotNull(snapshot.Epic);
        }
        finally
        {
            await runtime.DisposeAsync();
            SqliteConnection.ClearAllPools();
            if (Directory.Exists(root)) Directory.Delete(root, recursive: true);
        }
    }

    [Fact]
    public async Task SteamLibraryResponseIsValidatedAndMapped()
    {
        using var client = new HttpClient(new SteamLibraryHandler());
        var api = new LauncherApiClient(client, new Uri("http://launcher/"));

        var response = await api.ConnectSteamAsync(new Dictionary<string, string>
        {
            ["openid.mode"] = "id_res",
            ["openid.claimed_id"] = "https://steamcommunity.com/openid/id/76561197960265729"
        });

        Assert.Equal("76561197960265729", response.SteamId64);
        Assert.Equal(1, response.GameCount);
        Assert.Equal("Counter-Strike 2", Assert.Single(response.Games).Name);
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

    private sealed class EmptyCatalogHandler : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) =>
            Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("{\"items\":[],\"next_cursor\":null}")
            });
    }

    private sealed class SteamLibraryHandler : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            Assert.Equal(HttpMethod.Post, request.Method);
            Assert.Equal("/api/v1/steam/connect", request.RequestUri?.AbsolutePath);
            return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent("{\"steam_id\":\"76561197960265729\",\"persona_name\":null,\"games\":[{\"app_id\":\"730\",\"name\":\"Counter-Strike 2\",\"playtime_minutes\":120,\"icon_url\":null,\"header_url\":\"https://cdn.example/header.jpg\"}],\"game_count\":1,\"refreshed_at\":\"2026-08-20T00:00:00Z\"}")
            });
        }
    }
}
