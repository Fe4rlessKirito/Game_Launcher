using System.Text.Json;
using System.Net.Http.Headers;
using System.Diagnostics;
using Launcher.Core;
using Launcher.Downloads;
using Launcher.Installation;
using Launcher.Manifests;
using Launcher.Networking;
using Launcher.Security;
using Launcher.Storage;

namespace Launcher.App.Runtime;

public sealed record RuntimeGame(
    string Id,
    string Slug,
    string Title,
    string Description,
    string Monogram,
    string? BuildId,
    string DisplayVersion,
    long SizeBytes,
    GameState State,
    string InstallRoot,
    GameCatalogItem? Catalog,
    InstalledGame? Installed,
    SteamGameInstall? SteamInstall = null,
    string? ArtworkSource = null,
    string? IconArtworkSource = null)
{
    public bool IsSteamGame => SteamInstall is not null;
    public bool IsInstalled => Installed is not null || IsSteamGame;
    public bool HasBuild => !string.IsNullOrWhiteSpace(BuildId);

    public string StatusText => IsSteamGame
        ? "Installed"
        : State switch
    {
        GameState.Launchable or GameState.Installed => "Installed",
        GameState.UpdateAvailable => "Update available",
        GameState.Queued or GameState.Downloading or GameState.Installing => "Installing",
        GameState.Error => "Error",
        _ => "Ready to install"
    };

    public string SizeDisplay => FormatBytes(SizeBytes);

    private static string FormatBytes(long bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        var units = new[] { "KB", "MB", "GB", "TB" };
        var value = (double)bytes;
        var unit = -1;
        while (value >= 1024 && unit < units.Length - 1)
        {
            value /= 1024;
            unit++;
        }

        return $"{value:0.#} {units[unit]}";
    }
}

public sealed record LauncherRuntimeSnapshot(
    IReadOnlyList<RuntimeGame> Games,
    IReadOnlyList<PersistedDownloadJob> DownloadJobs,
    bool IsOnline,
    string ConnectionStatus,
    string? Error,
    LauncherUserProfile? User,
    SteamLibrarySnapshot? Steam = null,
    IReadOnlySet<string>? ExcludedGameIds = null);

public sealed class LauncherRuntime : IAsyncDisposable
{
    // The public release endpoint must be HTTPS. Operators can override it
    // with LAUNCHER_API_BASE_URL or the local settings file during cutover.
    private const string DefaultMantleApiBaseUrl = "https://vaultnode.pp.ua";
    private const string DefaultSupabaseUrl = "https://mywavagbkgjfqitkimcp.supabase.co";
    // This is Supabase's browser-safe publishable key, not a service-role secret.
    private const string DefaultSupabasePublishableKey = "sb_publishable_TkuLTCPZIozOSZMbDqDLSQ_4qNh8Fwq";
    private const string LegacyLocalApiBaseUrl = "http://127.0.0.1:8080";
    private const string LegacyMantleIpApiBaseUrl = "http://5.231.32.191";
    private static readonly IReadOnlyDictionary<string, string> DefaultMantleTrustedManifestKeys =
        new Dictionary<string, string>(StringComparer.Ordinal)
        {
            ["mantle-2026-08-14"] = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAn/FIINMz1NFm3jhcgclN\nKQNZ/1932tlQA9r1uYpXU0aGVoT+2W4KPWNr5u+QgyVDO/ocGk/LpkT7TGHlFOQg\nVAvxnRu/Q3TvSBTRsXlJFDT6sHCg0bEbWPzDJOEt1h7q1WZOMml/x8V9tSfeDqye\nk3ipyDI4GgaNsv5AYV/KPTePLUiyu61uEWnKVsGq1jndAjSi8Y3HadY7YDsxll77\njSiQi+CLmkmKNIRSzs1oA0pni5C2RkXDoIYuvAXHblFvIn144N8cAn+U+QL+IyRH\nVX55g5Ksa7QaEZ5trlw7X995oevjJCDJO59OyV+5LnrB9w8eAwW9UVldmjLy2xu6\nywIDAQAB\n-----END PUBLIC KEY-----",
            ["authorized-spacewar-2026"] = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA11UH/8GIAXxEYRODqxLc\nxp+KsUPK5KgrQLS/gL0LfexRbfSj9VWw1/uKFNbgG3fz/O3w3qf3GlslKFU+Cc7K\n17K9ZGbhgtQkkaJouTzLqfmH3gNxegHrZkfVyeYLEyXUemeGrjG/lj5Yv3Ek8JwB\nQgMMWnP1WdEO192Ab1qL5VsHuTrNVOZsWSgWAY23ZZKkKbStm5+Vh6MQi8VjYZ/j\nXgc/bo0+/A/06cjnZ1gwSk0NN69EZos6siLHO0SNCArT4gLL5pGAjZ/VAf0AVMyu\nnGK5O4kswB3tnX9ZzwfuMIaWdavZF9gVb0484g5Btm3ZCKoQI/87H6gqJjFZq1Ed\nnwIDAQAB\n-----END PUBLIC KEY-----",
            ["local-spacewar-key"] = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAvnXGMH7NIVouQjbXD6cA\nO61FI9xrfZfYlKQWopcg/B9M5Rd0VVeEYxauRok50KSxdz3elGzxPv0H1gYKkIQZ\nCaRttXSeUnYavT1+tpZQOA0EYFqIFcl8MfKbJd0jyR5BAGOJ77bBhsfgCw1TfmvJ\nac/HfcAXDaBdPtseeZPqcsVC8Shvn+PAcWIti9UlOW7S0eyT5s+Ek46hRnltxTeR\nWOQH+1n5fC3v5VlxcIMe6rIKg0xfpufaus2FqAfqHwjYgkClGK/cm9odrNeis1O/\n8ezkHAZkMvqss/MP4Cot3//LOpAx6K69oVS5WhiqWjbSdQhUAFn1+htrt/kFRN2a\nwQIDAQAB\n-----END PUBLIC KEY-----",
            ["staging-2026-01"] = "-----BEGIN PUBLIC KEY-----\nMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAwU/5VcHGDAJ4Ns64Iw5y\nWeaKerdWYPhM0qearJ5uXBaIvv29g8Cu+ol7Hqpp2K6bCKmpRigyyFhUy2tv+V7i\nvfJHmvRGyM3HoFIHguFZrcg43R9x3OSApu0kqKoixeX9ymz9Axuh7P+okpG+9art\n/F6TN/cCtp8kWIx5Sac+HYmmrTDYoy7DgC7HozRHlaYh2IxdinEcyqSyp0niBhgj\nwAh4D4+sAChnXkONhbMhegJRRTRW4N59C9Cl0X2q3YmAqM0D0z2Xq8G3KkpZIjMd\nuIz+FtsJz7e28lsfQbVFRqLUfCjSqq7otn15BEbiF/MlT80SQ+Z9oheCaMP/mQT/\nmQIDAQAB\n-----END PUBLIC KEY-----",
        };
    private readonly LauncherSettings _settings;
    private readonly LauncherPaths _paths;
    private readonly HttpClient _apiHttpClient;
    private readonly HttpClient _downloadHttpClient;
    private readonly bool _ownsApiHttpClient;
    private string? _accessToken;
    private readonly LauncherApiClient _apiClient;
    private readonly LocalStateStore _stateStore;
    private readonly ChunkCache _chunkCache;
    private readonly PackCache _packCache;
    private readonly Func<SteamLibrarySnapshot> _steamDiscovery;
    private readonly SemaphoreSlim _operationGate = new(1, 1);
    private readonly object _downloaderGate = new();
    private DownloadManager? _activeDownloader;
    private IReadOnlyList<GameCatalogItem> _catalog = [];
    private LauncherRuntimeSnapshot _snapshot = new([], [], false, "Offline-ready", null, null);
    private bool _initialized;

    public event Action<LauncherRuntimeSnapshot>? SnapshotChanged;
    public event Action<DownloadProgress>? ProgressChanged;

    public LauncherRuntime(
        LauncherSettings settings,
        string stateRoot,
        HttpClient? httpClient = null,
        string? accessToken = null,
        Func<SteamLibrarySnapshot>? steamDiscovery = null)
    {
        _settings = settings;
        _paths = LauncherPaths.FromRoot(stateRoot);
        // Catalog refresh has its own short linked timeout below. Download
        // and restore requests must be allowed to stream a physical pack;
        // FileMirage can legitimately take longer than eight seconds before
        // the first verified bytes arrive.
        _apiHttpClient = httpClient ?? CreateStreamingHttpClient();
        _downloadHttpClient = CreateStreamingHttpClient();
        _ownsApiHttpClient = httpClient is null;
        SetAccessToken(accessToken);
        _apiClient = new LauncherApiClient(_apiHttpClient, ParseBaseUri(settings.ApiBaseUrl));
        _stateStore = new LocalStateStore(_paths.DatabasePath);
        _chunkCache = new ChunkCache(_paths.CachePath, Math.Max(1, settings.CacheSizeBytes));
        var packCacheBytes = Math.Clamp(settings.CacheSizeBytes / 2, 512L * 1024 * 1024, 2L * 1024 * 1024 * 1024);
        _packCache = new PackCache(Path.Combine(_paths.Root, "pack-cache"), packCacheBytes);
        _steamDiscovery = steamDiscovery ?? (() => SteamLibraryDiscovery.Discover());
    }

    public LauncherRuntimeSnapshot Snapshot => _snapshot;

    public bool IsAuthenticated => !string.IsNullOrWhiteSpace(_accessToken);

    public async Task<LauncherUserProfile> SignInAsync(
        string email,
        string password,
        CancellationToken cancellationToken = default)
    {
        SetAccessToken(null);
        var authClient = CreateSupabaseAuthClient();

        var session = await authClient.SignInWithPasswordAsync(email, password, cancellationToken).ConfigureAwait(false);
        SetAccessToken(session.AccessToken);
        try
        {
            var snapshot = await RefreshAsync(cancellationToken).ConfigureAwait(false);
            return snapshot.User ?? throw new InvalidOperationException("The launcher API did not return an account profile.");
        }
        catch
        {
            SetAccessToken(null);
            throw;
        }
    }

    public async Task<LauncherUserProfile> UpdateUsernameAsync(
        string username,
        CancellationToken cancellationToken = default)
    {
        if (_accessToken is not { Length: > 0 } accessToken)
        {
            throw new InvalidOperationException("Sign in before changing your username.");
        }

        await CreateSupabaseAuthClient().UpdateUsernameAsync(accessToken, username, cancellationToken).ConfigureAwait(false);
        var snapshot = await RefreshAsync(cancellationToken).ConfigureAwait(false);
        return snapshot.User ?? throw new InvalidOperationException("The launcher API did not return the updated account profile.");
    }

    public async Task SignOutAsync(CancellationToken cancellationToken = default)
    {
        SetAccessToken(null);
        await RefreshAsync(cancellationToken).ConfigureAwait(false);
    }

    public static async Task<LauncherRuntime> CreateDefaultAsync(CancellationToken cancellationToken = default)
    {
        var localRoot = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        if (string.IsNullOrWhiteSpace(localRoot)) localRoot = AppContext.BaseDirectory;
        var stateRoot = Environment.GetEnvironmentVariable("LAUNCHER_STATE_ROOT");
        stateRoot = string.IsNullOrWhiteSpace(stateRoot) ? Path.Combine(localRoot, "Vaultnode") : stateRoot;
        var settingsPath = Environment.GetEnvironmentVariable("LAUNCHER_SETTINGS_PATH");
        settingsPath = string.IsNullOrWhiteSpace(settingsPath) ? Path.Combine(stateRoot, "settings.json") : settingsPath;

        LauncherSettings settings;
        var loadedSettings = false;
        try
        {
            settings = await new JsonSettingsStore(settingsPath).LoadAsync(cancellationToken).ConfigureAwait(false);
            loadedSettings = true;
        }
        catch (IOException)
        {
            settings = new LauncherSettings();
        }
        catch (JsonException)
        {
            settings = new LauncherSettings();
        }

        var migratedApiEndpoint = loadedSettings && IsLegacyApiBaseUrl(settings.ApiBaseUrl);
        if (migratedApiEndpoint)
        {
            settings = settings with { ApiBaseUrl = DefaultMantleApiBaseUrl };
            try
            {
                await new JsonSettingsStore(settingsPath).SaveAsync(settings, cancellationToken).ConfigureAwait(false);
            }
            catch (IOException)
            {
                // The in-memory migration is still safe if a read-only profile
                // prevents persisting the compatibility rewrite.
            }
            catch (UnauthorizedAccessException)
            {
                // The in-memory migration is still safe if a read-only profile
                // prevents persisting the compatibility rewrite.
            }
        }

        if (settings.ApiBaseUrl.Equals(DefaultMantleApiBaseUrl, StringComparison.OrdinalIgnoreCase)
            && !settings.RequireTrustedManifestKeys
            && (settings.TrustedManifestKeysPem is null || settings.TrustedManifestKeysPem.Count == 0))
        {
            settings = settings with
            {
                TrustedManifestKeysPem = DefaultMantleTrustedManifestKeys,
                RequireTrustedManifestKeys = true,
            };
        }

        var apiOverride = Environment.GetEnvironmentVariable("LAUNCHER_API_BASE_URL");
        if (!string.IsNullOrWhiteSpace(apiOverride)) settings = settings with { ApiBaseUrl = apiOverride.Trim() };
        var accessToken = Environment.GetEnvironmentVariable("LAUNCHER_ACCESS_TOKEN");
        return new LauncherRuntime(settings, stateRoot, accessToken: accessToken);
    }

    public async Task<LauncherRuntimeSnapshot> InitializeAsync(CancellationToken cancellationToken = default)
    {
        if (!_initialized)
        {
            await _stateStore.InitializeAsync(cancellationToken).ConfigureAwait(false);
            await _chunkCache.InitializeAsync(cancellationToken).ConfigureAwait(false);
            await _stateStore.FailInterruptedDownloadJobsAsync(cancellationToken).ConfigureAwait(false);
            _initialized = true;
        }

        return await RefreshAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task<LauncherRuntimeSnapshot> RefreshAsync(CancellationToken cancellationToken = default)
    {
        if (!_initialized)
        {
            await _stateStore.InitializeAsync(cancellationToken).ConfigureAwait(false);
            await _chunkCache.InitializeAsync(cancellationToken).ConfigureAwait(false);
            await _stateStore.FailInterruptedDownloadJobsAsync(cancellationToken).ConfigureAwait(false);
            _initialized = true;
        }

        var installed = await _stateStore.GetInstalledGamesAsync(cancellationToken).ConfigureAwait(false);
        var jobs = await _stateStore.GetDownloadJobsAsync(cancellationToken).ConfigureAwait(false);
        var excludedGameIds = await _stateStore.GetExcludedGameIdsAsync(cancellationToken).ConfigureAwait(false);
        var online = false;
        string? error = null;
        try
        {
            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            timeout.CancelAfter(TimeSpan.FromSeconds(8));
            _catalog = await _apiClient.GetGamesAsync(timeout.Token).ConfigureAwait(false);
            online = true;
        }
        catch (Exception exception) when (exception is HttpRequestException or TaskCanceledException or InvalidDataException)
        {
            error = exception.Message;
        }

        var steam = _steamDiscovery();
        var games = BuildGames(_catalog, installed, steam.Games);
        var connectionStatus = online ? "Online" : "Offline-ready";
        LauncherUserProfile? user = null;
        if (IsAuthenticated)
        {
            try
            {
                using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
                timeout.CancelAfter(TimeSpan.FromSeconds(8));
                user = await _apiClient.GetCurrentUserAsync(timeout.Token).ConfigureAwait(false);
            }
            catch (Exception exception) when (exception is HttpRequestException or TaskCanceledException or InvalidDataException or JsonException)
            {
                // Catalog access remains usable if the optional profile lookup
                // fails or the staging token has expired.
            }
        }

        _snapshot = new LauncherRuntimeSnapshot(games, jobs, online, connectionStatus, error, user, steam, excludedGameIds);
        SnapshotChanged?.Invoke(_snapshot);
        return _snapshot;
    }

    public RuntimeGame? FindGame(string? idOrTitle)
    {
        if (string.IsNullOrWhiteSpace(idOrTitle)) return null;
        return _snapshot.Games.FirstOrDefault(game =>
            string.Equals(game.Id, idOrTitle, StringComparison.OrdinalIgnoreCase)
            || string.Equals(game.Title, idOrTitle, StringComparison.OrdinalIgnoreCase));
    }

    public async Task<LauncherRuntimeSnapshot> RemoveFromLibraryAsync(
        string gameId,
        CancellationToken cancellationToken = default)
    {
        if (string.IsNullOrWhiteSpace(gameId))
        {
            throw new ArgumentException("A game id is required.", nameof(gameId));
        }

        if (!_initialized)
        {
            await InitializeAsync(cancellationToken).ConfigureAwait(false);
        }

        await _stateStore.SetGameExcludedAsync(gameId, excluded: true, cancellationToken).ConfigureAwait(false);
        var excludedGameIds = await _stateStore.GetExcludedGameIdsAsync(cancellationToken).ConfigureAwait(false);
        _snapshot = _snapshot with { ExcludedGameIds = excludedGameIds };
        SnapshotChanged?.Invoke(_snapshot);
        return _snapshot;
    }

    public void PauseActiveDownload()
    {
        lock (_downloaderGate) _activeDownloader?.Pause();
    }

    public void ResumeActiveDownload()
    {
        lock (_downloaderGate) _activeDownloader?.Resume();
    }

    public async Task InstallAsync(string gameId, IProgress<DownloadProgress>? progress = null, CancellationToken cancellationToken = default)
    {
        await _operationGate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            var game = FindGame(gameId) ?? throw new LauncherOperationException($"Game was not found in the catalog: {gameId}");
            if (string.IsNullOrWhiteSpace(game.BuildId)) throw new LauncherOperationException($"No published build is available for {game.Title}.");

            var signedManifest = await _apiClient.GetManifestWithBytesAsync(game.BuildId, cancellationToken).ConfigureAwait(false);
            var signature = await _apiClient.GetManifestSignatureAsync(game.BuildId, cancellationToken).ConfigureAwait(false);
            ManifestSignatureVerifier.Verify(
                signedManifest.RawBytes,
                signature,
                _settings.TrustedManifestKeysPem,
                allowEmbeddedPublicKey: !_settings.RequireTrustedManifestKeys
                    && (_settings.TrustedManifestKeysPem is null || _settings.TrustedManifestKeysPem.Count == 0));

            using var downloader = new DownloadManager(
                _downloadHttpClient,
                _apiClient,
                _chunkCache,
                Math.Clamp(_settings.ConcurrentDownloads, 1, 32),
                _stateStore,
                packCache: _packCache);
            lock (_downloaderGate) _activeDownloader = downloader;
            var trackedProgress = new Progress<DownloadProgress>(value =>
            {
                ProgressChanged?.Invoke(value);
                progress?.Report(value);
            });
            await downloader.DownloadAsync(signedManifest.Manifest, $"install-{game.Id}-{signedManifest.Manifest.BuildId}", trackedProgress, cancellationToken).ConfigureAwait(false);

            var installer = new Installer(_chunkCache, _stateStore);
            var installRoot = game.Installed?.InstallRoot ?? BuildInstallRoot(game);
            if (game.Installed is not null)
            {
                var previous = ManifestJson.Deserialize(game.Installed.ManifestJson);
                await installer.UpdateAsync(previous, signedManifest.Manifest, installRoot, cancellationToken: cancellationToken).ConfigureAwait(false);
            }
            else
            {
                await installer.InstallAsync(signedManifest.Manifest, installRoot, cancellationToken: cancellationToken).ConfigureAwait(false);
            }

            await RefreshAsync(cancellationToken).ConfigureAwait(false);
        }
        finally
        {
            lock (_downloaderGate) _activeDownloader = null;
            _operationGate.Release();
        }
    }

    public async Task RepairAsync(string gameId, CancellationToken cancellationToken = default)
    {
        await _operationGate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            var game = FindGame(gameId) ?? throw new LauncherOperationException($"Game was not found: {gameId}");
            if (game.Installed is null) throw new LauncherOperationException($"{game.Title} is not installed.");
            var manifest = ManifestJson.Deserialize(game.Installed.ManifestJson);
            await new Installer(_chunkCache, _stateStore).RepairAsync(manifest, game.Installed.InstallRoot, cancellationToken: cancellationToken).ConfigureAwait(false);
            await RefreshAsync(cancellationToken).ConfigureAwait(false);
        }
        finally
        {
            _operationGate.Release();
        }
    }

    public async Task ClearCompletedDownloadsAsync(CancellationToken cancellationToken = default)
    {
        await _stateStore.DeleteCompletedDownloadJobsAsync(cancellationToken).ConfigureAwait(false);
        await RefreshAsync(cancellationToken).ConfigureAwait(false);
    }

    public async Task UninstallAsync(string gameId, CancellationToken cancellationToken = default)
    {
        await _operationGate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            var game = FindGame(gameId) ?? throw new LauncherOperationException($"Game was not found: {gameId}");
            if (game.Installed is null) return;
            await new Installer(_chunkCache, _stateStore).UninstallAsync(game.Installed, cancellationToken: cancellationToken).ConfigureAwait(false);
            await RefreshAsync(cancellationToken).ConfigureAwait(false);
        }
        finally
        {
            _operationGate.Release();
        }
    }

    public Task LaunchAsync(string gameId)
    {
        var game = FindGame(gameId) ?? throw new LauncherOperationException($"Game was not found: {gameId}");
        if (game.SteamInstall is not null)
        {
            return SteamLauncher.LaunchAsync(game.SteamInstall);
        }

        if (game.Installed is null) throw new LauncherOperationException($"{game.Title} is not installed.");

        var manifest = ManifestJson.Deserialize(game.Installed.ManifestJson);
        _ = Installer.Launch(manifest, game.Installed.InstallRoot);
        return Task.CompletedTask;
    }

    public async ValueTask DisposeAsync()
    {
        _operationGate.Dispose();
        _downloadHttpClient.Dispose();
        if (_ownsApiHttpClient) _apiHttpClient.Dispose();
        await Task.CompletedTask.ConfigureAwait(false);
    }

    private static List<RuntimeGame> BuildGames(
        IReadOnlyList<GameCatalogItem> catalog,
        IReadOnlyList<InstalledGame> installed,
        IReadOnlyList<SteamGameInstall> steamGames)
    {
        var games = new List<RuntimeGame>(catalog.Count + installed.Count);
        var matchedInstalled = new HashSet<string>(StringComparer.Ordinal);
        foreach (var item in catalog)
        {
            var local = installed.FirstOrDefault(game => string.Equals(game.GameId, item.Id, StringComparison.OrdinalIgnoreCase));
            if (local is not null) matchedInstalled.Add(local.GameId);
            var build = item.LatestBuild;
            var state = local is null
                ? GameState.NotInstalled
                : build is not null && !string.Equals(local.BuildId, build.Id, StringComparison.Ordinal)
                    ? GameState.UpdateAvailable
                    : GameState.Launchable;
            games.Add(new RuntimeGame(
                item.Id,
                string.IsNullOrWhiteSpace(item.Slug) ? item.Id : item.Slug,
                item.Title,
                item.Description,
                BuildMonogram(item.Title),
                build?.Id,
                build?.DisplayVersion ?? local?.DisplayVersion ?? "—",
                build?.SizeBytes ?? 0,
                state,
                local?.InstallRoot ?? string.Empty,
                item,
                local,
                ArtworkSource: item.CoverImageUrl ?? item.HeroImageUrl));
        }

        foreach (var local in installed.Where(game => !matchedInstalled.Contains(game.GameId)))
        {
            games.Add(new RuntimeGame(
                local.GameId,
                local.GameId,
                local.GameId,
                "Installed local build",
                BuildMonogram(local.GameId),
                local.BuildId,
                local.DisplayVersion,
                0,
                GameState.Launchable,
                local.InstallRoot,
                null,
                local,
                ArtworkSource: null));
        }

        foreach (var steamGame in steamGames)
        {
            var id = $"steam:{steamGame.AppId}";
            if (games.Any(game => string.Equals(game.Id, id, StringComparison.OrdinalIgnoreCase)))
            {
                continue;
            }

            games.Add(new RuntimeGame(
                id,
                id,
                steamGame.Name,
                "Installed through Steam. Steam manages this installation and its updates.",
                BuildMonogram(steamGame.Name),
                null,
                "Steam",
                steamGame.SizeBytes,
                GameState.Launchable,
                steamGame.InstallRoot,
                null,
                null,
                steamGame,
                steamGame.ArtworkPath,
                steamGame.IconArtworkPath));
        }

        return games;
    }

    private string BuildInstallRoot(RuntimeGame game)
    {
        var baseRoot = string.IsNullOrWhiteSpace(_settings.DefaultGameDirectory) ? _paths.GamesPath : _settings.DefaultGameDirectory;
        var segment = string.IsNullOrWhiteSpace(game.Slug) ? game.Id : game.Slug;
        var invalid = Path.GetInvalidFileNameChars();
        segment = new string(segment.Select(character => invalid.Contains(character) ? '_' : character).ToArray()).Trim();
        if (segment.Length == 0 || segment is "." or "..") segment = game.Id;
        return Path.Combine(baseRoot, segment);
    }

    private static Uri ParseBaseUri(string value)
    {
        if (!Uri.TryCreate(value, UriKind.Absolute, out var uri) || uri is null || uri.Scheme is not ("http" or "https"))
        {
            throw new InvalidDataException($"Launcher API base URL is invalid: {value}");
        }

        return new Uri(uri.ToString().TrimEnd('/') + "/", UriKind.Absolute);
    }

    private void SetAccessToken(string? accessToken)
    {
        _accessToken = string.IsNullOrWhiteSpace(accessToken) ? null : accessToken.Trim();
        _apiHttpClient.DefaultRequestHeaders.Authorization = _accessToken is null
            ? null
            : new AuthenticationHeaderValue("Bearer", _accessToken);
    }

    private SupabaseAuthClient CreateSupabaseAuthClient()
    {
        var supabaseUrl = Environment.GetEnvironmentVariable("LAUNCHER_SUPABASE_URL");
        var publishableKey = Environment.GetEnvironmentVariable("LAUNCHER_SUPABASE_ANON_KEY");
        return new SupabaseAuthClient(
            _apiHttpClient,
            ParseBaseUri(string.IsNullOrWhiteSpace(supabaseUrl) ? DefaultSupabaseUrl : supabaseUrl),
            string.IsNullOrWhiteSpace(publishableKey) ? DefaultSupabasePublishableKey : publishableKey.Trim());
    }

    private static HttpClient CreateStreamingHttpClient() => new()
    {
        Timeout = TimeSpan.FromMinutes(10),
    };

    private static bool IsLegacyApiBaseUrl(string? value)
    {
        if (string.IsNullOrWhiteSpace(value)) return true;
        var normalized = value.Trim().TrimEnd('/');
        return normalized.Equals(LegacyLocalApiBaseUrl, StringComparison.OrdinalIgnoreCase)
            || normalized.Equals(LegacyMantleIpApiBaseUrl, StringComparison.OrdinalIgnoreCase)
            || normalized.Equals("https://5.231.32.191", StringComparison.OrdinalIgnoreCase);
    }

    private static string BuildMonogram(string title)
    {
        var words = title.Split(' ', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        var monogram = words.Length > 1
            ? string.Concat(words.Take(2).Select(word => char.ToUpperInvariant(word[0])))
            : new string(title.Where(char.IsLetterOrDigit).Take(2).Select(char.ToUpperInvariant).ToArray());
        return monogram.Length == 0 ? "G" : monogram;
    }
}
