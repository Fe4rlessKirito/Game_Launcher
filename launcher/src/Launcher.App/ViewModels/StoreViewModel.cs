using System.Collections.ObjectModel;
using Avalonia.Media.Imaging;
using CommunityToolkit.Mvvm.ComponentModel;
using Launcher.App.Runtime;

namespace Launcher.App.ViewModels;

public partial class StoreViewModel : ObservableObject
{
    private IReadOnlyList<StoreGame> _allGames = [];

    public ObservableCollection<StoreGame> Games { get; } = [];

    [ObservableProperty]
    private string _searchQuery = string.Empty;

    public string GameCountDisplay => $"{Games.Count} GAME{(Games.Count == 1 ? string.Empty : "S")}";
    public bool IsEmpty => Games.Count == 0;

    public void ApplyRuntimeGames(IReadOnlyList<RuntimeGame> games)
    {
        _allGames = games
            .Where(game => !game.IsExternalStoreGame)
            .Select(game => new StoreGame(
                game.Id,
                game.Title,
                game.Description,
                game.DisplayVersion,
                game.SizeDisplay,
                game.Monogram,
                game.StatusText,
                game.ArtworkSource))
            .OrderBy(game => game.Title, StringComparer.OrdinalIgnoreCase)
            .ToArray();
        ApplySearch();
    }

    partial void OnSearchQueryChanged(string value) => ApplySearch();

    private void ApplySearch()
    {
        var query = SearchQuery.Trim();
        Games.Clear();
        foreach (var game in _allGames.Where(game => query.Length == 0
                     || game.Title.Contains(query, StringComparison.OrdinalIgnoreCase)
                     || game.Description.Contains(query, StringComparison.OrdinalIgnoreCase)))
        {
            Games.Add(game);
        }

        OnPropertyChanged(nameof(GameCountDisplay));
        OnPropertyChanged(nameof(IsEmpty));
    }
}

public sealed record StoreGame(
    string Id,
    string Title,
    string Description,
    string Version,
    string Size,
    string Monogram,
    string Status,
    string? ArtworkSource = null,
    bool IsSteamGame = false)
{
    public bool HasArtwork => !string.IsNullOrWhiteSpace(ArtworkSource);
    public Bitmap? ArtworkImage => ArtworkLoader.Load(ArtworkSource);
    public bool HasArtworkImage => ArtworkImage is not null;
    public bool ShowMonogram => !HasArtwork;
    public bool ShowSteamBadge => IsSteamGame;
}
