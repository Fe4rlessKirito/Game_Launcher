using Launcher.App.ViewModels;

namespace Launcher.App.Tests;

public class ViewModelTests
{
    [Fact]
    public void ShellStartsOnHome()
    {
        var shell = new ShellViewModel();
        Assert.IsType<HomeViewModel>(shell.CurrentPage);
        Assert.Equal("Good evening", shell.PageTitle);
    }

    [Fact]
    public void ShellNavigationCoversLibraryDownloadsSettingsAndDetails()
    {
        var shell = new ShellViewModel();

        shell.NavigateCommand.Execute("Library");
        Assert.IsType<LibraryViewModel>(shell.CurrentPage);
        shell.NavigateCommand.Execute("Downloads");
        Assert.IsType<DownloadsViewModel>(shell.CurrentPage);
        shell.NavigateCommand.Execute("Settings");
        Assert.IsType<SettingsViewModel>(shell.CurrentPage);
        shell.OpenDetailsCommand.Execute("Synthetic Game");
        Assert.IsType<GameDetailsViewModel>(shell.CurrentPage);
        Assert.Equal("Synthetic Game", shell.PageTitle);
    }
}
