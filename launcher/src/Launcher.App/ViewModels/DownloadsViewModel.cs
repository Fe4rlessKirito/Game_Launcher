using CommunityToolkit.Mvvm.ComponentModel;

namespace Launcher.App.ViewModels;

public partial class DownloadsViewModel : ObservableObject
{
    public IReadOnlyList<DownloadTile> Jobs { get; } =
    [
        new("No active downloads", "Verified chunks and installation jobs will appear here.", 0)
    ];
}

public sealed record DownloadTile(string Title, string Detail, double Progress);
