using CommunityToolkit.Mvvm.ComponentModel;

namespace Launcher.App.ViewModels;

public partial class SettingsViewModel : ObservableObject
{
    [ObservableProperty] private bool _launchOnStartup;
    [ObservableProperty] private bool _minimizeOnClose = true;
    [ObservableProperty] private bool _reducedMotion;
    [ObservableProperty] private int _concurrentDownloads = 4;
    public string DownloadDirectory { get; set; } = "C:\\Games\\Launcher\\Downloads";
    public string InstallDirectory { get; set; } = "C:\\Games";
}
