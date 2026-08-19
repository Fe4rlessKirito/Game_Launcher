using System.Collections.ObjectModel;
using System.Globalization;
using System.Linq;
using Avalonia.Media;
using Avalonia.Media.Imaging;
using Avalonia.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Launcher.App.Runtime;
using Launcher.Core;

namespace Launcher.App.ViewModels;

public partial class ShellViewModel : ObservableObject
{
    private LauncherRuntime? _runtime;
    private RuntimeGame[] _runtimeGames = [];
    private readonly HomeViewModel _homePage = new();
    private readonly LibraryViewModel _libraryPage = new();
    private readonly StoreViewModel _storePage = new();
    private readonly CollectionsViewModel _collectionsPage;
    private readonly DownloadsViewModel _downloadsPage = new();
    private readonly SettingsViewModel _settingsPage = new();
    private readonly Stack<NavigationTarget> _backStack = new();
    private readonly Stack<NavigationTarget> _forwardStack = new();
    private readonly HashSet<string> _excludedGameIds = new(StringComparer.OrdinalIgnoreCase);
    private NavigationTarget _currentTarget = new("Library", null, true);

    [ObservableProperty] private object _currentPage;
    [ObservableProperty] private string _pageTitle = "Your library";
    [ObservableProperty] private string _pageKicker = "LIBRARY / OVERVIEW";
    [ObservableProperty] private string _currentDestination = "Library";
    [ObservableProperty] private string _searchQuery = string.Empty;
    [ObservableProperty] private bool _isSidebarVisible = true;
    [ObservableProperty] private bool _isPageHeaderVisible = true;
    [ObservableProperty] private bool _isAddingCategory;
    [ObservableProperty] private string _newCategoryName = string.Empty;
    [ObservableProperty] private string _categoryError = string.Empty;
    [ObservableProperty] private bool _isAddingGame;
    [ObservableProperty] private string _newGameName = string.Empty;
    [ObservableProperty] private string _gameError = string.Empty;
    [ObservableProperty] private bool _showOnlyReadyToPlay;
    [ObservableProperty] private string _connectionStatus = "Offline-ready";
    [ObservableProperty] private string _runtimeError = string.Empty;
    [ObservableProperty] private string _accountDisplayName = "Guest";

    public ObservableCollection<SidebarCategory> SidebarCategories { get; }
    public SettingsViewModel Settings => _settingsPage;
    public string AccountInitials => BuildMonogram(AccountDisplayName.TrimStart('@'));
    public bool IsGuest => string.Equals(AccountDisplayName, "Guest", StringComparison.OrdinalIgnoreCase);

    public ObservableCollection<SidebarCategory> VisibleSidebarCategories { get; } = new();
    public SidebarCategory? SelectedCollection { get; private set; }

    public IReadOnlyList<SidebarGame> SidebarGames => SidebarCategories.SelectMany(category => category.Games).ToArray();
    public string GamesFilterLabel => $"Games ({_runtimeGames.Length})";
    public string DownloadStatus { get; private set; } = "Downloads · All games up to date";
    public bool CanGoBack => _backStack.Count > 0;
    public bool CanGoForward => _forwardStack.Count > 0;
    public bool IsLibrarySelected => string.Equals(CurrentDestination, "Library", StringComparison.OrdinalIgnoreCase)
        || string.Equals(CurrentDestination, "Collections", StringComparison.OrdinalIgnoreCase);
    public bool IsStoreSelected => string.Equals(CurrentDestination, "Store", StringComparison.OrdinalIgnoreCase);
    public bool IsCommunitySelected => string.Equals(CurrentDestination, "Community", StringComparison.OrdinalIgnoreCase);
    public bool IsHomeSelected => string.Equals(CurrentDestination, "Home", StringComparison.OrdinalIgnoreCase)
        || string.Equals(CurrentDestination, "Library", StringComparison.OrdinalIgnoreCase);
    public bool HasSearchQuery => !string.IsNullOrWhiteSpace(SearchQuery);
    public string ReadyToPlayFilterTip => ShowOnlyReadyToPlay ? "Show all games" : "Show only ready to play games";

    public ShellViewModel(LauncherRuntime? runtime = null, bool seedDemoData = true)
    {
        SidebarCategories =
        [
            new SidebarCategory("FAVORITES", false, seedDemoData ? new[]
            {
                new SidebarGame("Synthetic Game", "SG", "Installed", 4),
                new SidebarGame("Build Playground", "BP", "Ready to install", 3),
                new SidebarGame("Asterfall", "AF", "Not installed", 2),
                new SidebarGame("Northstar", "NS", "Not installed", 1)
            } : [])
        ];
        _collectionsPage = new CollectionsViewModel(SidebarCategories);
        _currentPage = _libraryPage;
        foreach (var category in SidebarCategories)
        {
            category.ApplyFilter(SearchQuery);
        }

        RefreshVisibleSidebarCategories();
        if (runtime is not null) AttachRuntime(runtime);
    }

    public async Task InitializeRuntimeAsync(LauncherRuntime runtime, CancellationToken cancellationToken = default)
    {
        AttachRuntime(runtime);
        ConnectionStatus = "Connecting…";
        RuntimeError = string.Empty;
        try
        {
            var snapshot = await runtime.InitializeAsync(cancellationToken).ConfigureAwait(true);
            ApplyRuntimeSnapshot(snapshot);
        }
        catch (Exception error) when (error is IOException or HttpRequestException or InvalidDataException or TaskCanceledException)
        {
            ConnectionStatus = "Offline-ready";
            RuntimeError = error.Message;
        }
    }

    private void AttachRuntime(LauncherRuntime runtime)
    {
        if (ReferenceEquals(_runtime, runtime)) return;
        if (_runtime is not null)
        {
            _runtime.SnapshotChanged -= OnRuntimeSnapshotChanged;
            _runtime.ProgressChanged -= OnRuntimeProgressChanged;
        }
        _runtime = runtime;
        _runtime.SnapshotChanged += OnRuntimeSnapshotChanged;
        _runtime.ProgressChanged += OnRuntimeProgressChanged;
        _downloadsPage.AttachRuntime(runtime);
        _settingsPage.AttachRuntime(runtime);
    }

    private void OnRuntimeSnapshotChanged(LauncherRuntimeSnapshot snapshot)
    {
        Dispatcher.UIThread.Post(() => ApplyRuntimeSnapshot(snapshot));
    }

    private void OnRuntimeProgressChanged(DownloadProgress progress)
    {
        Dispatcher.UIThread.Post(() => _downloadsPage.ApplyProgress(progress));
    }

    public void SetRuntimeError(string message)
    {
        ConnectionStatus = "Offline-ready";
        RuntimeError = message;
    }

    private void ApplyRuntimeSnapshot(LauncherRuntimeSnapshot snapshot)
    {
        ConnectionStatus = snapshot.ConnectionStatus;
        RuntimeError = snapshot.Error ?? string.Empty;
        AccountDisplayName = snapshot.User?.Username is { Length: > 0 } username
            ? $"@{username}"
            : snapshot.User is not null ? "Account" : "Guest";
        _settingsPage.ApplyUser(snapshot.User);
        _settingsPage.ApplySteamSnapshot(snapshot.Steam ?? SteamLibrarySnapshot.Empty);
        _downloadsPage.ApplyRuntimeJobs(snapshot.DownloadJobs, snapshot.Games);
        _storePage.ApplyRuntimeGames(snapshot.Games);
        UpdateDownloadStatus(snapshot.DownloadJobs);

        _excludedGameIds.Clear();
        if (snapshot.ExcludedGameIds is not null)
        {
            _excludedGameIds.UnionWith(snapshot.ExcludedGameIds);
        }

        var visibleGames = snapshot.Games
            .Where(game => !_excludedGameIds.Contains(game.Id))
            .ToArray();
        _runtimeGames = visibleGames;
        _libraryPage.ApplyRuntimeGames(visibleGames);
        OnPropertyChanged(nameof(GamesFilterLabel));

        foreach (var category in SidebarCategories.Where(category => category.IsUserCreated))
        {
            foreach (var game in category.Games.Where(game => _excludedGameIds.Contains(game.OpenKey)).ToArray())
            {
                category.Games.Remove(game);
            }

            category.ApplyFilter(SearchQuery, readyToPlayOnly: ShowOnlyReadyToPlay);
        }

        var favorites = SidebarCategories.FirstOrDefault(category => !category.IsUserCreated);
        if (favorites is not null)
        {
            favorites.Games.Clear();
            foreach (var game in visibleGames.Where(game => game.IsSteamGame && game.SteamInstall?.IsFavorite == true))
            {
                favorites.Games.Add(new SidebarGame(game.Title, game.Monogram, game.StatusText, RecentActivityOrder(game), game.Id, game.BuildId, game.IconArtworkSource ?? game.ArtworkSource, game.IsSteamGame));
            }
            favorites.ApplyFilter(SearchQuery, readyToPlayOnly: ShowOnlyReadyToPlay);
        }

        OnPropertyChanged(nameof(SidebarGames));
        if (CurrentPage is GameDetailsViewModel details && (_currentTarget.GameId ?? _currentTarget.Title) is { } gameKey)
        {
            details.ApplyRuntimeGame(_runtime?.FindGame(gameKey));
        }
    }

    private void UpdateDownloadStatus(IReadOnlyList<PersistedDownloadJob> jobs)
    {
        var active = jobs.Count(job => job.State is not DownloadJobState.Ready
            and not DownloadJobState.Cancelled
            and not DownloadJobState.Failed);
        DownloadStatus = active == 0
            ? "Downloads · All games up to date"
            : $"Downloads · {active} active";
        OnPropertyChanged(nameof(DownloadStatus));
    }

    private static int RecentActivityOrder(RuntimeGame game) => game.Installed is null
        ? 0
        : Math.Clamp((int)(DateTimeOffset.UtcNow - game.Installed.InstalledAt).TotalMinutes, 0, int.MaxValue) * -1;

    partial void OnSearchQueryChanged(string value)
    {
        foreach (var category in SidebarCategories)
        {
            category.ApplyFilter(value);
        }

        _libraryPage.ApplySearch(value);

        OnPropertyChanged(nameof(HasSearchQuery));
    }

    partial void OnShowOnlyReadyToPlayChanged(bool value) => OnPropertyChanged(nameof(ReadyToPlayFilterTip));

    partial void OnAccountDisplayNameChanged(string value)
    {
        OnPropertyChanged(nameof(AccountInitials));
        OnPropertyChanged(nameof(IsGuest));
    }

    [RelayCommand]
    private void ClearSearch() => SearchQuery = string.Empty;

    [RelayCommand]
    private void SortByRecentActivity()
    {
        foreach (var category in SidebarCategories)
        {
            category.ApplyFilter(SearchQuery, sortByRecentActivity: true);
        }
    }

    [RelayCommand]
    private void ToggleReadyToPlay()
    {
        ShowOnlyReadyToPlay = !ShowOnlyReadyToPlay;
        foreach (var category in SidebarCategories)
        {
            category.ApplyFilter(SearchQuery, readyToPlayOnly: ShowOnlyReadyToPlay);
        }
    }

    [RelayCommand]
    private void Navigate(string? destination)
    {
        var target = CreateTarget(destination);
        if (target == _currentTarget)
        {
            if (target.Key is "Library" or "Collections")
            {
                ClearCollectionSelection();
            }

            return;
        }

        _backStack.Push(_currentTarget);
        _forwardStack.Clear();
        ApplyTarget(target);
        NotifyNavigationState();
    }

    [RelayCommand]
    private void GoBack()
    {
        if (!_backStack.TryPop(out var target))
        {
            return;
        }

        _forwardStack.Push(_currentTarget);
        ApplyTarget(target);
        NotifyNavigationState();
    }

    [RelayCommand]
    private void GoForward()
    {
        if (!_forwardStack.TryPop(out var target))
        {
            return;
        }

        _backStack.Push(_currentTarget);
        ApplyTarget(target);
        NotifyNavigationState();
    }

    [RelayCommand]
    private void OpenDetails(string? title)
    {
        if (string.IsNullOrWhiteSpace(title))
        {
            return;
        }

        var runtimeGame = _runtime?.FindGame(title);
        _backStack.Push(_currentTarget);
        _forwardStack.Clear();
        ApplyTarget(new NavigationTarget("Details", runtimeGame?.Title ?? title, IsSidebarVisible, runtimeGame?.Id));
        NotifyNavigationState();
    }

    [RelayCommand]
    private void BeginAddCategory()
    {
        CategoryError = string.Empty;
        NewCategoryName = string.Empty;
        IsAddingCategory = true;
    }

    [RelayCommand]
    private void CancelCategory()
    {
        CategoryError = string.Empty;
        NewCategoryName = string.Empty;
        IsAddingCategory = false;
    }

    [RelayCommand]
    private void CommitCategory()
    {
        var name = NewCategoryName.Trim();
        if (name.Length == 0)
        {
            CategoryError = "Enter a category name.";
            return;
        }

        if (SidebarCategories.Any(category => string.Equals(category.Name, name, StringComparison.OrdinalIgnoreCase)))
        {
            CategoryError = "That category already exists.";
            return;
        }

        SidebarCategories.Add(new SidebarCategory(name, true));
        RefreshVisibleSidebarCategories();
        CancelCategory();
    }

    [RelayCommand]
    private void SelectCollection(SidebarCategory? category)
    {
        if (category is null || !SidebarCategories.Contains(category))
        {
            return;
        }

        SelectedCollection = category;
        RefreshVisibleSidebarCategories();
        OnPropertyChanged(nameof(SelectedCollection));
    }

    private void ClearCollectionSelection()
    {
        if (SelectedCollection is null)
        {
            return;
        }

        SelectedCollection = null;
        RefreshVisibleSidebarCategories();
        OnPropertyChanged(nameof(SelectedCollection));
    }

    private void RefreshVisibleSidebarCategories()
    {
        VisibleSidebarCategories.Clear();
        if (SelectedCollection is not null)
        {
            VisibleSidebarCategories.Add(SelectedCollection);
            return;
        }

        foreach (var category in SidebarCategories)
        {
            VisibleSidebarCategories.Add(category);
        }
    }

    [RelayCommand]
    private static void ToggleCategory(SidebarCategory? category)
    {
        if (category is not null)
        {
            category.IsExpanded = !category.IsExpanded;
        }
    }

    [RelayCommand]
    private void RemoveCategory(SidebarCategory? category)
    {
        if (category is not null && category.IsUserCreated)
        {
            SidebarCategories.Remove(category);
            if (ReferenceEquals(SelectedCollection, category))
            {
                ClearCollectionSelection();
            }
            else
            {
                RefreshVisibleSidebarCategories();
            }
        }
    }

    [RelayCommand]
    private void BeginAddGame()
    {
        if (!IsSidebarVisible)
        {
            Navigate("Library");
        }

        GameError = string.Empty;
        NewGameName = string.Empty;
        IsAddingGame = true;
    }

    [RelayCommand]
    private void CancelAddGame()
    {
        GameError = string.Empty;
        NewGameName = string.Empty;
        IsAddingGame = false;
    }

    [RelayCommand]
    private void CommitAddGame()
    {
        var name = NewGameName.Trim();
        if (name.Length == 0)
        {
            GameError = "Enter a game name.";
            return;
        }

        if (SidebarGames.Any(game => string.Equals(game.Title, name, StringComparison.OrdinalIgnoreCase)))
        {
            GameError = "That game is already in your library.";
            return;
        }

        var favorites = SidebarCategories.FirstOrDefault(category => !category.IsUserCreated)
            ?? SidebarCategories.First();
        favorites.Games.Add(new SidebarGame(name, BuildMonogram(name), "Not installed"));
        favorites.ApplyFilter(SearchQuery);
        OnPropertyChanged(nameof(SidebarGames));
        OnPropertyChanged(nameof(GamesFilterLabel));
        CancelAddGame();
    }

    public void AddGameToCategory(SidebarGame game, SidebarCategory category)
    {
        if (category.IsUserCreated
            && !category.Games.Any(existing => string.Equals(existing.OpenKey, game.OpenKey, StringComparison.OrdinalIgnoreCase)))
        {
            category.Games.Add(game);
            category.ApplyFilter(SearchQuery);
            category.IsExpanded = true;
        }
    }

    public Task RemoveGameFromLibraryAsync(SidebarGame game) =>
        RemoveGameFromLibraryAsync(game?.OpenKey, game?.Title);

    public Task RemoveGameFromLibraryAsync(GameTile game) =>
        RemoveGameFromLibraryAsync(game?.OpenKey, game?.Title);

    private async Task RemoveGameFromLibraryAsync(string? gameId, string? gameTitle)
    {
        if (string.IsNullOrWhiteSpace(gameId))
        {
            return;
        }

        var wasExcluded = _excludedGameIds.Contains(gameId);
        var removedSidebarGames = SidebarCategories
            .Select(category => new
            {
                Category = category,
                Games = category.Games
                    .Where(candidate => string.Equals(candidate.OpenKey, gameId, StringComparison.OrdinalIgnoreCase))
                    .ToArray()
            })
            .Where(entry => entry.Games.Length > 0)
            .ToArray();
        _excludedGameIds.Add(gameId);
        RemoveGameFromLocalProjection(gameId);

        if (_runtime is null || wasExcluded)
        {
            return;
        }

        try
        {
            await _runtime.RemoveFromLibraryAsync(gameId).ConfigureAwait(true);
        }
        catch (Exception error) when (error is IOException or InvalidOperationException or OperationCanceledException or Microsoft.Data.Sqlite.SqliteException)
        {
            _excludedGameIds.Remove(gameId);
            foreach (var entry in removedSidebarGames)
            {
                foreach (var removedGame in entry.Games)
                {
                    if (!entry.Category.Games.Any(existing => string.Equals(existing.OpenKey, removedGame.OpenKey, StringComparison.OrdinalIgnoreCase)))
                    {
                        entry.Category.Games.Add(removedGame);
                    }
                }

                entry.Category.ApplyFilter(SearchQuery, readyToPlayOnly: ShowOnlyReadyToPlay);
            }

            if (_runtime is not null)
            {
                _runtimeGames = _runtime.Snapshot.Games
                    .Where(runtimeGame => !_excludedGameIds.Contains(runtimeGame.Id))
                    .ToArray();
            }
            _libraryPage.ApplyRuntimeGames(_runtimeGames);
            _libraryPage.ApplySearch(SearchQuery);
            OnPropertyChanged(nameof(SidebarGames));
            OnPropertyChanged(nameof(GamesFilterLabel));
            RuntimeError = $"Could not remove {gameTitle ?? gameId} from the library: {error.Message}";
        }
    }

    private void RemoveGameFromLocalProjection(string gameId)
    {
        foreach (var category in SidebarCategories)
        {
            foreach (var game in category.Games.Where(candidate => string.Equals(candidate.OpenKey, gameId, StringComparison.OrdinalIgnoreCase)).ToArray())
            {
                category.Games.Remove(game);
            }

            category.ApplyFilter(SearchQuery, readyToPlayOnly: ShowOnlyReadyToPlay);
        }

        _runtimeGames = _runtimeGames
            .Where(runtimeGame => !string.Equals(runtimeGame.Id, gameId, StringComparison.OrdinalIgnoreCase))
            .ToArray();
        _libraryPage.ApplyRuntimeGames(_runtimeGames);
        _libraryPage.ApplySearch(SearchQuery);
        OnPropertyChanged(nameof(SidebarGames));
        OnPropertyChanged(nameof(GamesFilterLabel));
    }

    private void ApplyTarget(NavigationTarget target)
    {
        _currentTarget = target;
        CurrentDestination = target.Key;
        if (target.Key is "Library" or "Collections")
        {
            ClearCollectionSelection();
        }
        IsSidebarVisible = target.Key switch
        {
            "Library" or "Collections" => true,
            "Details" => target.KeepSidebar,
            _ => false
        };
        IsPageHeaderVisible = !string.Equals(target.Key, "Downloads", StringComparison.OrdinalIgnoreCase)
            && !string.Equals(target.Key, "Settings", StringComparison.OrdinalIgnoreCase)
            && !string.Equals(target.Key, "Collections", StringComparison.OrdinalIgnoreCase);

        switch (target.Key)
        {
            case "Library":
                CurrentPage = _libraryPage;
                PageTitle = "Your library";
                PageKicker = "LIBRARY / OVERVIEW";
                break;
            case "Collections":
                CurrentPage = _collectionsPage;
                PageTitle = "Your collections";
                PageKicker = "LIBRARY / COLLECTIONS";
                break;
            case "Downloads":
                CurrentPage = _downloadsPage;
                PageTitle = "Downloads";
                PageKicker = "DOWNLOADS / ACTIVITY";
                break;
            case "Settings":
                CurrentPage = _settingsPage;
                PageTitle = "Settings";
                PageKicker = "SETTINGS / PREFERENCES";
                break;
            case "Store":
                CurrentPage = _storePage;
                PageTitle = "Store";
                PageKicker = "STORE / DISCOVER";
                break;
            case "Community":
                CurrentPage = new SectionPageViewModel("Community", "COMMUNITY / ACTIVITY", "Updates, friends, and shared game activity will live here.", "Community features are ready for account data.");
                PageTitle = "Community";
                PageKicker = "COMMUNITY / ACTIVITY";
                break;
            case "Help":
                CurrentPage = new SectionPageViewModel("Help", "HELP / SUPPORT", "Check launcher health, storage status, and recovery guidance.", "Support tools will connect to the backend diagnostics surface.");
                PageTitle = "Help";
                PageKicker = "HELP / SUPPORT";
                break;
            case "Details":
                var runtimeGame = _runtime?.FindGame(target.GameId ?? target.Title);
                CurrentPage = new GameDetailsViewModel(target.Title ?? runtimeGame?.Title ?? "Synthetic Game", _runtime, runtimeGame);
                PageTitle = target.Title ?? "Game details";
                PageKicker = "GAME DETAILS / BUILD INFORMATION";
                break;
            default:
                CurrentPage = _libraryPage;
                PageTitle = "Your library";
                PageKicker = "LIBRARY / OVERVIEW";
                break;
        }

        OnPropertyChanged(nameof(CurrentPage));
        OnPropertyChanged(nameof(PageTitle));
        OnPropertyChanged(nameof(PageKicker));
    }

    private void NotifyNavigationState()
    {
        OnPropertyChanged(nameof(CanGoBack));
        OnPropertyChanged(nameof(CanGoForward));
        OnPropertyChanged(nameof(IsLibrarySelected));
        OnPropertyChanged(nameof(IsStoreSelected));
        OnPropertyChanged(nameof(IsCommunitySelected));
        OnPropertyChanged(nameof(IsHomeSelected));
    }

    private static NavigationTarget CreateTarget(string? destination) => destination?.Trim().ToUpperInvariant() switch
    {
        "HOME" or "LIBRARY" or "GAMES" => new NavigationTarget("Library", null, true),
        "COLLECTIONS" => new NavigationTarget("Collections", null, true),
        "DOWNLOADS" => new NavigationTarget("Downloads", null),
        "SETTINGS" => new NavigationTarget("Settings", null),
        "STORE" => new NavigationTarget("Store", null),
        "COMMUNITY" or "FRIENDS" => new NavigationTarget("Community", null),
        "HELP" => new NavigationTarget("Help", null),
        _ => new NavigationTarget("Library", null)
    };

    private static string BuildMonogram(string title)
    {
        var words = title.Split(' ', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        var monogram = words.Length > 1
            ? string.Concat(words.Take(2).Select(word => char.ToUpperInvariant(word[0])))
            : new string(title.Where(char.IsLetterOrDigit).Take(2).Select(char.ToUpperInvariant).ToArray());
        return monogram.Length == 0 ? "G" : monogram;
    }

    private sealed record NavigationTarget(string Key, string? Title, bool KeepSidebar = false, string? GameId = null);
}

public sealed record SidebarGame(
    string Title,
    string Monogram,
    string Status,
    int RecentActivityOrder = 0,
    string? GameId = null,
    string? BuildId = null,
    string? ArtworkSource = null,
    bool IsSteamGame = false)
{
    public string OpenKey => GameId ?? Title;
    public bool HasArtwork => !string.IsNullOrWhiteSpace(ArtworkSource);
    public Bitmap? ArtworkImage => ArtworkLoader.LoadSidebarIcon(ArtworkSource);
    public bool HasArtworkImage => ArtworkImage is not null;
    public bool ShowMonogram => !HasArtworkImage;
    public bool ShowSteamBadge => IsSteamGame;

    private static readonly IBrush InstalledBrush = new SolidColorBrush(Color.Parse("#D6D7D8"));
    private static readonly IBrush UpdateBrush = new SolidColorBrush(Color.Parse("#1A9FFF"));
    private static readonly IBrush UnavailableBrush = new SolidColorBrush(Color.Parse("#6D7886"));

    public bool IsReadyToPlay =>
        string.Equals(Status, "Installed", StringComparison.OrdinalIgnoreCase)
        || Status.Contains("ready to play", StringComparison.OrdinalIgnoreCase)
        || Status.Contains("streamed", StringComparison.OrdinalIgnoreCase);

    public IBrush StatusBrush =>
        Status.Contains("update", StringComparison.OrdinalIgnoreCase)
            ? UpdateBrush
            : Status.Contains("not installed", StringComparison.OrdinalIgnoreCase)
              || Status.Contains("ready to install", StringComparison.OrdinalIgnoreCase)
                ? UnavailableBrush
                : InstalledBrush;
}

public partial class SidebarCategory : ObservableObject
{
    private string _filter = string.Empty;
    private bool _sortByRecentActivity;
    private bool _readyToPlayOnly;

    public SidebarCategory(string name, bool isUserCreated, IEnumerable<SidebarGame>? games = null)
    {
        Name = name;
        IsUserCreated = isUserCreated;
        Games = games is null ? new ObservableCollection<SidebarGame>() : new ObservableCollection<SidebarGame>(games);
        VisibleGames = new ObservableCollection<SidebarGame>();
        Games.CollectionChanged += (_, _) =>
        {
            ApplyFilter(_filter);
            OnPropertyChanged(nameof(GameCount));
        };
        ApplyFilter(string.Empty);
    }

    public string Name { get; }
    public bool IsUserCreated { get; }
    public ObservableCollection<SidebarGame> Games { get; }
    public ObservableCollection<SidebarGame> VisibleGames { get; }
    public int GameCount => Games.Count;
    public string GameCountDisplay => _readyToPlayOnly && VisibleGames.Count != Games.Count
        ? $"{VisibleGames.Count}/{Games.Count}"
        : Games.Count.ToString(CultureInfo.InvariantCulture);
    public bool IsEmpty => VisibleGames.Count == 0;
    public string ExpandGlyph => IsExpanded ? "−" : "+";

    [ObservableProperty]
    private bool _isExpanded = true;

    partial void OnIsExpandedChanged(bool value) => OnPropertyChanged(nameof(ExpandGlyph));

    public void ApplyFilter(string? filter, bool? sortByRecentActivity = null, bool? readyToPlayOnly = null)
    {
        _filter = filter?.Trim() ?? string.Empty;
        if (sortByRecentActivity.HasValue)
        {
            _sortByRecentActivity = sortByRecentActivity.Value;
        }

        if (readyToPlayOnly.HasValue)
        {
            _readyToPlayOnly = readyToPlayOnly.Value;
        }

        VisibleGames.Clear();
        var games = Games.Where(game =>
            (_filter.Length == 0 || game.Title.Contains(_filter, StringComparison.OrdinalIgnoreCase))
            && (!_readyToPlayOnly || game.IsReadyToPlay));
        if (_sortByRecentActivity)
        {
            games = games
                .OrderByDescending(game => game.RecentActivityOrder)
                .ThenBy(game => game.Title, StringComparer.OrdinalIgnoreCase);
        }

        foreach (var game in games)
        {
            VisibleGames.Add(game);
        }

        OnPropertyChanged(nameof(IsEmpty));
        OnPropertyChanged(nameof(GameCountDisplay));
    }
}

public sealed class SectionPageViewModel
{
    public SectionPageViewModel(string title, string kicker, string description, string detail)
    {
        Title = title;
        Kicker = kicker;
        Description = description;
        Detail = detail;
    }

    public string Title { get; }
    public string Kicker { get; }
    public string Description { get; }
    public string Detail { get; }
}
