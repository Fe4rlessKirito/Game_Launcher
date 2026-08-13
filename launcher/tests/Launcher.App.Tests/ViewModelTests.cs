using Launcher.App.ViewModels;
using Launcher.App.Runtime;
using Launcher.Core;

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
        Assert.Equal(2, shell.VisibleSidebarCategories.Count);
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
}
