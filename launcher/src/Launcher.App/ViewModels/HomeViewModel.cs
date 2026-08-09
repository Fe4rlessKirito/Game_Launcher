using CommunityToolkit.Mvvm.ComponentModel;

namespace Launcher.App.ViewModels;

public partial class HomeViewModel : ObservableObject
{
    private readonly string _greeting = "A quiet place for your games.";
    private readonly string _summary = "Your installed library is available even when the network is not. New builds are verified before they reach the install folder.";
    public string Greeting => _greeting;
    public string Summary => _summary;
    public IReadOnlyList<HomeActivity> RecentActivity { get; } =
    [
        new("Synthetic Game", "Ready to play", "NOW", "Accent"),
        new("Content-addressed cache", "Integrity verified", "TODAY", "Muted"),
        new("Launcher foundation", "Up to date", "TODAY", "Muted")
    ];
}

public sealed record HomeActivity(string Title, string Detail, string Time, string Tone);
