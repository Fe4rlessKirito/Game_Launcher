using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using Launcher.Core;
using Launcher.App.Runtime;

namespace Launcher.App.ViewModels;

public partial class LibraryViewModel : ObservableObject
{
    private IReadOnlyList<GameTile> _allGames = [];
    private IReadOnlyList<HomeGame> _allRecentlyPlayed = [];
    private IReadOnlyList<HomeGame> _allPlayNext = [];

    public ObservableCollection<HomeGame> RecentlyPlayed { get; } = [];
    public ObservableCollection<GameTile> Games { get; } = [];
    public ObservableCollection<HomeGame> PlayNext { get; } = [];

    public string GameCountDisplay => $"{Games.Count} ITEM{(Games.Count == 1 ? string.Empty : "S")}";

    public void ApplyRuntimeGames(IReadOnlyList<RuntimeGame> games)
    {
        _allGames = games
            .Select(game => new GameTile(game.Title, game.StatusText, game.DisplayVersion, game.Monogram, game.State, game.Id))
            .ToArray();
        _allRecentlyPlayed = games
            .Select(game => new HomeGame(game.Title, game.StatusText, game.Monogram, game.IsInstalled ? "Play" : "Explore"))
            .ToArray();
        _allPlayNext = games
            .Where(game => !game.IsInstalled || game.State == GameState.UpdateAvailable)
            .Take(4)
            .Select(game => new HomeGame(
                game.Title,
                game.State == GameState.UpdateAvailable ? "An update is ready" : "A verified build is waiting",
                game.Monogram,
                game.State == GameState.UpdateAvailable ? "Update" : "Install"))
            .ToArray();

        ApplySearch(string.Empty);
    }

    public void ApplySearch(string? query)
    {
        var filter = query?.Trim() ?? string.Empty;
        Games.ReplaceWith(_allGames.Where(game => Matches(game.Title, filter)));
        RecentlyPlayed.ReplaceWith(_allRecentlyPlayed.Where(game => Matches(game.Title, filter)));
        PlayNext.ReplaceWith(_allPlayNext.Where(game => Matches(game.Title, filter)));
        OnPropertyChanged(nameof(GameCountDisplay));
    }

    private static bool Matches(string title, string filter) =>
        filter.Length == 0 || title.Contains(filter, StringComparison.OrdinalIgnoreCase);
}

internal static class ObservableCollectionExtensions
{
    public static void ReplaceWith<T>(this ObservableCollection<T> collection, IEnumerable<T> items)
    {
        collection.Clear();
        foreach (var item in items)
        {
            collection.Add(item);
        }
    }
}

public sealed record GameTile(string Title, string Status, string Version, string Monogram, GameState State, string? GameId = null);
