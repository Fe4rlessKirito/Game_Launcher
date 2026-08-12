using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using Launcher.Core;
using Launcher.App.Runtime;

namespace Launcher.App.ViewModels;

public partial class LibraryViewModel : ObservableObject
{
    public ObservableCollection<HomeGame> RecentlyPlayed { get; } =
    [
        new("Synthetic Game", "Played recently", "SG", "28 min"),
        new("Build Playground", "Ready to install", "BP", "New"),
        new("Asterfall", "Not installed", "AF", "Explore"),
        new("Northstar", "Not installed", "NS", "Explore")
    ];

    public ObservableCollection<GameTile> Games { get; } =
    [
        new("Synthetic Game", "Installed", "1.0.0", "SG", GameState.Launchable),
        new("Build Playground", "Ready to install", "0.4.2", "BP", GameState.NotInstalled)
    ];

    public ObservableCollection<HomeGame> PlayNext { get; } =
    [
        new("Build Playground", "A verified build is waiting", "BP", "Install"),
        new("Asterfall", "Add it to your library", "AF", "Explore")
    ];

    public string GameCountDisplay => $"{Games.Count} ITEM{(Games.Count == 1 ? string.Empty : "S")}";

    public void ApplyRuntimeGames(IReadOnlyList<RuntimeGame> games)
    {
        Games.Clear();
        RecentlyPlayed.Clear();
        PlayNext.Clear();

        foreach (var game in games)
        {
            Games.Add(new GameTile(game.Title, game.StatusText, game.DisplayVersion, game.Monogram, game.State, game.Id));
            RecentlyPlayed.Add(new HomeGame(game.Title, game.StatusText, game.Monogram, game.IsInstalled ? "Play" : "Explore"));
        }

        foreach (var game in games.Where(game => !game.IsInstalled || game.State == GameState.UpdateAvailable).Take(4))
        {
            PlayNext.Add(new HomeGame(game.Title, game.State == GameState.UpdateAvailable ? "An update is ready" : "A verified build is waiting", game.Monogram, game.State == GameState.UpdateAvailable ? "Update" : "Install"));
        }

        OnPropertyChanged(nameof(GameCountDisplay));
    }
}

public sealed record GameTile(string Title, string Status, string Version, string Monogram, GameState State, string? GameId = null);
