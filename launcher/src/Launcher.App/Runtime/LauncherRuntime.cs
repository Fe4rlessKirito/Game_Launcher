using System.Text.Json;
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
    InstalledGame? Installed)
{
    public bool IsInstalled => Installed is not null;
    public bool HasBuild => !string.IsNullOrWhiteSpace(BuildId);

    public string StatusText => State switch
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
    string? Error);

public sealed class LauncherRuntime : IAsyncDisposable
{
    // The public release endpoint must be HTTPS. Operators can override it
    // with LAUNCHER_API_BASE_URL or the local settings file during cutover.
    private const string DefaultMantleApiBaseUrl = "https://vaultnode.pp.ua";
    private const string LegacyLocalApiBaseUrl = "http://127.0.0.1:8080";
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
    private readonly HttpClient _httpClient;
    private readonly bool _ownsHttpClient;
    private readonly LauncherApiClient _apiClient;
    private readonly LocalStateStore _stateStore;
    private readonly ChunkCache _chunkCache;
    private readonly PackCache _packCache;
    private readonly SemaphoreSlim _operationGate = new(1, 1);
    private readonly object _downloaderGate = new();
    private DownloadManager? _activeDownloader;
    private IReadOnlyList<GameCatalogItem> _catalog = [];
    private LauncherRuntimeSnapshot _snapshot = new([], [], false, "Offline-ready", null);
    private bool _initialized;

    public event Action<LauncherRuntimeSnapshot>? SnapshotChanged;
    public event Action<DownloadProgress>? ProgressChanged;

    public LauncherRuntime(LauncherSettings settings, string stateRoot, HttpClient? httpClient = null)
    {
        _settings = settings;
        _paths = LauncherPaths.FromRoot(stateRoot);
        // Catalog refresh has its own short linked timeout below. Download
        // and restore requests must be allowed to stream a physical pack;
        // FileMirage can legitimately take longer than eight seconds before
        // the first verified bytes arrive.
        _httpClient = httpClient ?? new HttpClient { Timeout = TimeSpan.FromMinutes(10) };
        _ownsHttpClient = httpClient is null;
        _apiClient = new LauncherApiClient(_httpClient, ParseBaseUri(settings.ApiBaseUrl));
        _stateStore = new LocalStateStore(_paths.DatabasePath);
        _chunkCache = new ChunkCache(_paths.CachePath, Math.Max(1, settings.CacheSizeBytes));
        var packCacheBytes = Math.Clamp(settings.CacheSizeBytes / 2, 512L * 1024 * 1024, 2L * 1024 * 1024 * 1024);
        _packCache = new PackCache(Path.Combine(_paths.Root, "pack-cache"), packCacheBytes);
    }

    public LauncherRuntimeSnapshot Snapshot => _snapshot;

    public static async Task<LauncherRuntime> CreateDefaultAsync(CancellationToken cancellationToken = default)
    {
        var localRoot = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        if (string.IsNullOrWhiteSpace(localRoot)) localRoot = AppContext.BaseDirectory;
        var stateRoot = Environment.GetEnvironmentVariable("LAUNCHER_STATE_ROOT");
        stateRoot = string.IsNullOrWhiteSpace(stateRoot) ? Path.Combine(localRoot, "Vaultnode") : stateRoot;
        var settingsPath = Environment.GetEnvironmentVariable("LAUNCHER_SETTINGS_PATH");
        settingsPath = string.IsNullOrWhiteSpace(settingsPath) ? Path.Combine(stateRoot, "settings.json") : settingsPath;

        LauncherSettings settings;
        try
        {
            settings = await new JsonSettingsStore(settingsPath).LoadAsync(cancellationToken).ConfigureAwait(false);
        }
        catch (IOException)
        {
            settings = new LauncherSettings();
        }
        catch (JsonException)
        {
            settings = new LauncherSettings();
        }

        if (string.IsNullOrWhiteSpace(settings.ApiBaseUrl)
            || settings.ApiBaseUrl.Equals(LegacyLocalApiBaseUrl, StringComparison.OrdinalIgnoreCase))
        {
            settings = settings with { ApiBaseUrl = DefaultMantleApiBaseUrl };
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
        return new LauncherRuntime(settings, stateRoot);
    }

    public async Task<LauncherRuntimeSnapshot> InitializeAsync(CancellationToken cancellationToken = default)
    {
        if (!_initialized)
        {
            await _stateStore.InitializeAsync(cancellationToken).ConfigureAwait(false);
            await _chunkCache.InitializeAsync(cancellationToken).ConfigureAwait(false);
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
            _initialized = true;
        }

        var installed = await _stateStore.GetInstalledGamesAsync(cancellationToken).ConfigureAwait(false);
        var jobs = await _stateStore.GetDownloadJobsAsync(cancellationToken).ConfigureAwait(false);
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

        var games = BuildGames(_catalog, installed);
        var connectionStatus = online ? "Online" : "Offline-ready";
        _snapshot = new LauncherRuntimeSnapshot(games, jobs, online, connectionStatus, error);
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
                _httpClient,
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
        if (game.Installed is null) throw new LauncherOperationException($"{game.Title} is not installed.");
        var manifest = ManifestJson.Deserialize(game.Installed.ManifestJson);
        _ = Installer.Launch(manifest, game.Installed.InstallRoot);
        return Task.CompletedTask;
    }

    public async ValueTask DisposeAsync()
    {
        _operationGate.Dispose();
        if (_ownsHttpClient) _httpClient.Dispose();
        await Task.CompletedTask.ConfigureAwait(false);
    }

    private static List<RuntimeGame> BuildGames(IReadOnlyList<GameCatalogItem> catalog, IReadOnlyList<InstalledGame> installed)
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
                local));
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
                local));
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

    private static string BuildMonogram(string title)
    {
        var words = title.Split(' ', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        var monogram = words.Length > 1
            ? string.Concat(words.Take(2).Select(word => char.ToUpperInvariant(word[0])))
            : new string(title.Where(char.IsLetterOrDigit).Take(2).Select(char.ToUpperInvariant).ToArray());
        return monogram.Length == 0 ? "G" : monogram;
    }
}
