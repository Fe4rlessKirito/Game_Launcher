using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using Launcher.Core;
using Launcher.App.Runtime;

namespace Launcher.App.ViewModels;

public partial class LibraryViewModel : ObservableObject
{
    public ObservableCollection<HomeGame> RecentlyPlayed { get; } = [];
    public ObservableCollection<GameTile> Games { get; } = [];
    public ObservableCollection<HomeGame> PlayNext { get; } = [];

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
