using Avalonia.Media.Imaging;
using CommunityToolkit.Mvvm.ComponentModel;

namespace Launcher.App.ViewModels;

public partial class HomeViewModel : ObservableObject
{
    private readonly string _greeting = "A quiet place for your games.";
    private readonly string _summary = "Your installed library is available even when the network is not. New builds are verified before they reach the install folder.";
    public string Greeting => _greeting;
    public string Summary => _summary;
    public IReadOnlyList<FeaturedGame> FeaturedGames { get; } =
    [
        new("Synthetic Game", "A small verified build, ready when you are.", "PLAYABLE NOW", "SG")
    ];

    public IReadOnlyList<HomeGame> RecentlyPlayed { get; } =
    [
        new("Synthetic Game", "Played recently", "SG", "28 min"),
        new("Build Playground", "Ready to install", "BP", "New"),
        new("Asterfall", "Not installed", "AF", "Explore"),
        new("Northstar", "Not installed", "NS", "Explore")
    ];

    public IReadOnlyList<HomeGame> PlayNext { get; } =
    [
        new("Build Playground", "A verified build is waiting", "BP", "Install"),
        new("Asterfall", "Add it to your library", "AF", "Explore")
    ];
    public IReadOnlyList<HomeActivity> RecentActivity { get; } =
    [
        new("Synthetic Game", "Ready to play", "NOW", "Accent"),
        new("Content-addressed cache", "Integrity verified", "TODAY", "Muted"),
        new("Launcher foundation", "Up to date", "TODAY", "Muted")
    ];
}

public sealed record FeaturedGame(string Title, string Description, string Badge, string Monogram);
public sealed record HomeGame(
    string Title,
    string Subtitle,
    string Monogram,
    string Action,
    string? ArtworkSource = null,
    string? GameId = null,
    bool IsSteamGame = false)
{
    public string OpenKey => GameId ?? Title;
    public bool HasArtwork => !string.IsNullOrWhiteSpace(ArtworkSource);
    public Bitmap? ArtworkImage => ArtworkLoader.Load(ArtworkSource);
    public bool HasArtworkImage => ArtworkImage is not null;
    public bool ShowMonogram => !HasArtwork;
    public bool ShowSteamBadge => IsSteamGame;
}
public sealed record HomeActivity(string Title, string Detail, string Time, string Tone);
