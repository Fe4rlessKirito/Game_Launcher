using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

namespace Launcher.App.ViewModels;

public partial class ShellViewModel : ObservableObject
{
    [ObservableProperty] private object _currentPage;
    [ObservableProperty] private string _pageTitle = "Good evening";
    [ObservableProperty] private string _pageKicker = "HOME / OVERVIEW";
    [ObservableProperty] private string _connectionStatus = "Offline-ready";

    public ShellViewModel()
    {
        _currentPage = new HomeViewModel();
    }

    [RelayCommand]
    private void Navigate(string destination)
    {
        object page;
        switch (destination)
        {
            case "Library": page = new LibraryViewModel(); PageTitle = "Your library"; PageKicker = "LIBRARY / COLLECTION"; break;
            case "Downloads": page = new DownloadsViewModel(); PageTitle = "Downloads"; PageKicker = "DOWNLOADS / ACTIVITY"; break;
            case "Settings": page = new SettingsViewModel(); PageTitle = "Settings"; PageKicker = "SETTINGS / PREFERENCES"; break;
            default: page = new HomeViewModel(); PageTitle = "Good evening"; PageKicker = "HOME / OVERVIEW"; break;
        }
        CurrentPage = page;
        OnPropertyChanged(nameof(CurrentPage));
        OnPropertyChanged(nameof(PageTitle));
        OnPropertyChanged(nameof(PageKicker));
    }

    [RelayCommand]
    private void OpenDetails(string? title)
    {
        CurrentPage = new GameDetailsViewModel(title ?? "Synthetic Game");
        PageTitle = title ?? "Synthetic Game";
        PageKicker = "GAME DETAILS / BUILD INFORMATION";
        OnPropertyChanged(nameof(CurrentPage));
        OnPropertyChanged(nameof(PageTitle));
        OnPropertyChanged(nameof(PageKicker));
    }
}
