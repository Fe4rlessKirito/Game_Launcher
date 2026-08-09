using CommunityToolkit.Mvvm.ComponentModel;

namespace Launcher.App.ViewModels;

public partial class GameDetailsViewModel(string title) : ObservableObject
{
    public string Title { get; } = title;
    private readonly string _description = "A verified local build with content-addressed updates, repairable files, and an offline-ready launch profile.";
    private readonly string _version = "1.0.0";
    private readonly string _installSize = "90 B";
    private readonly string _installLocation = "C:\\Games\\Synthetic Game";
    private readonly string _status = "Installed and launchable";
    public string Description => _description;
    public string Version => _version;
    public string InstallSize => _installSize;
    public string InstallLocation => _installLocation;
    public string Status => _status;
}
