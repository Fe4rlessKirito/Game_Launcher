using Launcher.App.ViewModels;
using Launcher.App.Runtime;
using Launcher.Core;
using Microsoft.Data.Sqlite;

namespace Launcher.App.Tests;

public class ViewModelTests
{
    [Fact]
    public void ShellStartsOnLibraryOverview()
    {
        var shell = new ShellViewModel();
        Assert.IsType<LibraryViewModel>(shell.CurrentPage);
        Assert.Equal("Your library", shell.PageTitle);
        Assert.Equal("Library", shell.CurrentDestination);
        Assert.True(shell.IsSidebarVisible);
    }

    [Fact]
    public void ShellNavigationCoversLibraryDownloadsSettingsAndDetails()
    {
        var shell = new ShellViewModel();

        shell.NavigateCommand.Execute("Store");
        shell.NavigateCommand.Execute("Home");
        Assert.IsType<LibraryViewModel>(shell.CurrentPage);

        shell.NavigateCommand.Execute("Library");
        Assert.IsType<LibraryViewModel>(shell.CurrentPage);
        shell.NavigateCommand.Execute("Collections");
        Assert.IsType<CollectionsViewModel>(shell.CurrentPage);
        Assert.True(shell.IsSidebarVisible);
        Assert.True(shell.IsLibrarySelected);
        shell.NavigateCommand.Execute("Downloads");
        Assert.IsType<DownloadsViewModel>(shell.CurrentPage);
        shell.NavigateCommand.Execute("Settings");
        Assert.IsType<SettingsViewModel>(shell.CurrentPage);
        shell.OpenDetailsCommand.Execute("Synthetic Game");
        Assert.IsType<GameDetailsViewModel>(shell.CurrentPage);
        Assert.Equal("Synthetic Game", shell.PageTitle);
    }

    [Fact]
    public void SelectingACollectionOnlyFiltersTheSidebar()
    {
        var shell = new ShellViewModel();
        var favorites = shell.SidebarCategories[0];
        shell.NewCategoryName = "Co-op";
        shell.CommitCategoryCommand.Execute(null);

        shell.NavigateCommand.Execute("Collections");
        shell.SelectCollectionCommand.Execute(favorites);

        Assert.IsType<CollectionsViewModel>(shell.CurrentPage);
        Assert.Same(favorites, shell.SelectedCollection);
        Assert.Single(shell.VisibleSidebarCategories);
        Assert.Same(favorites, shell.VisibleSidebarCategories[0]);

        shell.NavigateCommand.Execute("Library");
        Assert.Null(shell.SelectedCollection);
        Assert.Equal(3, shell.VisibleSidebarCategories.Count);
    }

    [Fact]
    public void RecentActivitySortOnlyReordersSidebar()
    {
        var shell = new ShellViewModel();
        var favorites = shell.SidebarCategories[0];
        favorites.Games.Move(0, 3);

        Assert.Equal("Build Playground", favorites.VisibleGames[0].Title);

        shell.SortByRecentActivityCommand.Execute(null);

        Assert.Equal("Synthetic Game", favorites.VisibleGames[0].Title);
        Assert.IsType<LibraryViewModel>(shell.CurrentPage);
        Assert.Equal("Your library", shell.PageTitle);
    }

    [Fact]
    public void ReadyToPlayFilterOnlyChangesSidebar()
    {
        var shell = new ShellViewModel();
        var favorites = shell.SidebarCategories[0];

        shell.ToggleReadyToPlayCommand.Execute(null);

        Assert.Single(favorites.VisibleGames);
        Assert.Equal("Synthetic Game", favorites.VisibleGames[0].Title);
        Assert.Equal("1/4", favorites.GameCountDisplay);
        Assert.IsType<LibraryViewModel>(shell.CurrentPage);
        Assert.Equal("Your library", shell.PageTitle);

        shell.ToggleReadyToPlayCommand.Execute(null);
        Assert.Equal(4, favorites.VisibleGames.Count);
    }

    [Fact]
    public void LibrarySearchFiltersMainLibraryAndSidebarTogether()
    {
        var shell = new ShellViewModel();
        var library = Assert.IsType<LibraryViewModel>(shell.CurrentPage);
        library.ApplyRuntimeGames(
        [
            new RuntimeGame("synthetic", "synthetic", "Synthetic Game", "", "SG", "build-synthetic", "1.0.0", 90, GameState.Launchable, "", null, null),
            new RuntimeGame("build", "build", "Build Playground", "", "BP", "build-playground", "1.0.0", 90, GameState.NotInstalled, "", null, null)
        ]);

        shell.SearchQuery = "build";

        Assert.Single(library.Games);
        Assert.Equal("Build Playground", library.Games[0].Title);
        Assert.Single(library.RecentlyPlayed);
        Assert.Equal("Build Playground", library.RecentlyPlayed[0].Title);
        Assert.Single(shell.SidebarCategories[0].VisibleGames);
        Assert.Equal("Build Playground", shell.SidebarCategories[0].VisibleGames[0].Title);

        shell.ClearSearchCommand.Execute(null);

        Assert.Equal(2, library.Games.Count);
        Assert.Equal(4, shell.SidebarCategories[0].VisibleGames.Count);
    }

    [Fact]
    public async Task FavoritesFollowSteamStateAndGamesCanBeRemovedFromLibrary()
    {
        var root = Path.Combine(Path.GetTempPath(), "vaultnode-shell-test", Guid.NewGuid().ToString("N"));
        using var client = new HttpClient(new EmptyCatalogHandler());
        var runtime = new LauncherRuntime(
            new LauncherSettings(ApiBaseUrl: "http://launcher/"),
            root,
            client,
            steamDiscovery: () => new SteamLibrarySnapshot(
                [],
                [
                    new SteamGameInstall("10", "Favorite Game", "Favorite Game", "C:\\Games\\Favorite Game", "C:\\Games", 10, IsFavorite: true),
                    new SteamGameInstall("20", "Other Game", "Other Game", "C:\\Games\\Other Game", "C:\\Games", 20)
                ],
                null));
        var shell = new ShellViewModel(runtime, seedDemoData: false);

        try
        {
            await shell.InitializeRuntimeAsync(runtime);

            var favorites = shell.SidebarCategories[0];
            var uncategorized = shell.SidebarCategories[1];
            var library = Assert.IsType<LibraryViewModel>(shell.CurrentPage);
            Assert.Equal(["Favorite Game"], favorites.Games.Select(game => game.Title));
            Assert.Equal(["Other Game"], uncategorized.Games.Select(game => game.Title));
            Assert.Equal(["Favorite Game", "Other Game"], library.Games.Select(game => game.Title));

            await shell.RemoveGameFromLibraryAsync(favorites.Games[0]);

            Assert.Empty(favorites.Games);
            Assert.Equal(["Other Game"], uncategorized.Games.Select(game => game.Title));
            Assert.Equal(["Other Game"], library.Games.Select(game => game.Title));
            Assert.Contains("steam:10", runtime.Snapshot.ExcludedGameIds!);
        }
        finally
        {
            await runtime.DisposeAsync();
            SqliteConnection.ClearAllPools();
            if (Directory.Exists(root)) Directory.Delete(root, recursive: true);
        }
    }

    [Fact]
    public void UncategorizedGamesCanBeMovedIntoAUserCollection()
    {
        var shell = new ShellViewModel(seedDemoData: false);
        var uncategorized = shell.SidebarCategories[1];
        var game = new SidebarGame("Asterfall", "AF", "Not installed", GameId: "asterfall");
        uncategorized.Games.Add(game);

        shell.NewCategoryName = "Co-op";
        shell.CommitCategoryCommand.Execute(null);
        var collection = shell.SidebarCategories.Single(category => category.Name == "Co-op");

        Assert.True(shell.CanMoveGameToCategory(game.OpenKey, collection));
        Assert.True(shell.MoveGameToCategory(game.OpenKey, collection));
        Assert.Empty(uncategorized.Games);
        Assert.Contains(game, collection.Games);
    }

    private sealed class EmptyCatalogHandler : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) =>
            Task.FromResult(new HttpResponseMessage(System.Net.HttpStatusCode.OK)
            {
                Content = new StringContent("{\"items\":[],\"next_cursor\":null}")
            });
    }
}
