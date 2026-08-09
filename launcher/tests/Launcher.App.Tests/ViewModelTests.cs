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
}
