using System.Collections.ObjectModel;
using System.Globalization;
using System.Linq;
using Avalonia.Media;
using Avalonia.Threading;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Launcher.App.Runtime;
using Launcher.Core;

namespace Launcher.App.ViewModels;

public partial class ShellViewModel : ObservableObject
{
    private LauncherRuntime? _runtime;
    private IReadOnlyList<RuntimeGame> _runtimeGames = [];
    private readonly HomeViewModel _homePage = new();
    private readonly LibraryViewModel _libraryPage = new();
    private readonly CollectionsViewModel _collectionsPage;
    private readonly DownloadsViewModel _downloadsPage = new();
    private readonly SettingsViewModel _settingsPage = new();
    private readonly Stack<NavigationTarget> _backStack = new();
    private readonly Stack<NavigationTarget> _forwardStack = new();
    private NavigationTarget _currentTarget = new("Library", null);

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

    public ObservableCollection<SidebarCategory> SidebarCategories { get; }

    public ObservableCollection<SidebarCategory> VisibleSidebarCategories { get; } = new();
    public SidebarCategory? SelectedCollection { get; private set; }

    public IReadOnlyList<SidebarGame> SidebarGames => SidebarCategories.SelectMany(category => category.Games).ToArray();
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
        _downloadsPage.ApplyRuntimeJobs(snapshot.DownloadJobs, snapshot.Games);
        UpdateDownloadStatus(snapshot.DownloadJobs);
        if (snapshot.Games.Count == 0) return;

        _runtimeGames = snapshot.Games;
        _libraryPage.ApplyRuntimeGames(snapshot.Games);

        var favorites = SidebarCategories.FirstOrDefault(category => !category.IsUserCreated);
        if (favorites is not null)
        {
            favorites.Games.Clear();
            foreach (var game in snapshot.Games)
            {
                favorites.Games.Add(new SidebarGame(game.Title, game.Monogram, game.StatusText, RecentActivityOrder(game), game.Id, game.BuildId));
            }
            favorites.ApplyFilter(SearchQuery, readyToPlayOnly: ShowOnlyReadyToPlay);
        }

        OnPropertyChanged(nameof(SidebarGames));
        if (CurrentPage is GameDetailsViewModel details && _currentTarget.Title is not null)
        {
            details.ApplyRuntimeGame(_runtime?.FindGame(_currentTarget.Title));
        }
    }

    private void UpdateDownloadStatus(IReadOnlyList<PersistedDownloadJob> jobs)
    {
        var active = jobs.Count(job => job.State is not DownloadJobState.Ready and not DownloadJobState.Cancelled);
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

        OnPropertyChanged(nameof(HasSearchQuery));
    }

    partial void OnShowOnlyReadyToPlayChanged(bool value) => OnPropertyChanged(nameof(ReadyToPlayFilterTip));

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

        _backStack.Push(_currentTarget);
        _forwardStack.Clear();
        ApplyTarget(new NavigationTarget("Details", title));
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
        CancelAddGame();
    }

    public void AddGameToCategory(SidebarGame game, SidebarCategory category)
    {
        if (category.IsUserCreated && !category.Games.Contains(game))
        {
            category.Games.Add(game);
            category.ApplyFilter(SearchQuery);
            category.IsExpanded = true;
        }
    }

    private void ApplyTarget(NavigationTarget target)
    {
        _currentTarget = target;
        CurrentDestination = target.Key;
        if (target.Key is "Library" or "Collections")
        {
            ClearCollectionSelection();
        }
        IsSidebarVisible = string.Equals(target.Key, "Library", StringComparison.OrdinalIgnoreCase)
            || string.Equals(target.Key, "Collections", StringComparison.OrdinalIgnoreCase);
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
                CurrentPage = new SectionPageViewModel("Store", "STORE / DISCOVER", "Find verified games and builds for your library.", "The store surface is ready for catalog data.");
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
                var runtimeGame = _runtime?.FindGame(target.Title);
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
        "HOME" or "LIBRARY" or "GAMES" => new NavigationTarget("Library", null),
        "COLLECTIONS" => new NavigationTarget("Collections", null),
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

    private sealed record NavigationTarget(string Key, string? Title);
}

public sealed record SidebarGame(
    string Title,
    string Monogram,
    string Status,
    int RecentActivityOrder = 0,
    string? GameId = null,
    string? BuildId = null)
{
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
