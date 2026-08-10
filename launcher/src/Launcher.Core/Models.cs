namespace Launcher.Core;

public enum GameState
{
    NotInstalled,
    Queued,
    Downloading,
    Installing,
    Installed,
    UpdateAvailable,
    Updating,
    Verifying,
    Repairing,
    Launchable,
    Running,
    Error
}

public enum DownloadJobState
{
    Queued,
    Resolving,
    Downloading,
    VerifyingEncoded,
    Decompressing,
    VerifyingRaw,
    Ready,
    Paused,
    Cancelled,
    Failed
}

public sealed record GameCatalogItem(
    string Id,
    string Slug,
    string Title,
    string Description,
    string? HeroImageUrl,
    string? CoverImageUrl,
    BuildSummary? LatestBuild);

public sealed record BuildSummary(
    string Id,
    string GameId,
    string DisplayVersion,
    long SizeBytes,
    DateTimeOffset? PublishedAt);

public sealed record InstalledGame(
    string GameId,
    string BuildId,
    string DisplayVersion,
    string InstallRoot,
    string ManifestJson,
    DateTimeOffset InstalledAt);

public sealed record DownloadProgress(
    string JobId,
    DownloadJobState State,
    long DownloadedBytes,
    long TotalBytes,
    double BytesPerSecond,
    TimeSpan? Eta,
    string? CurrentChunkHash = null)
{
    public double Fraction => TotalBytes <= 0 ? 0 : Math.Clamp((double)DownloadedBytes / TotalBytes, 0, 1);
}

public sealed record PersistedDownloadJob(
    string JobId,
    string BuildId,
    DownloadJobState State,
    long DownloadedBytes,
    long TotalBytes,
    DateTimeOffset UpdatedAt,
    string? LastError = null);

public enum InstallationFailurePoint
{
    None,
    AfterStagingFirstFile,
    AfterStagingAllFiles,
    BeforeDatabaseCommit,
    AfterFilesystemCommitBeforeDatabaseCommit,
    DuringUpdateFileSwap
}

public sealed record InstallationFailureInjection(InstallationFailurePoint Point)
{
    public void ThrowIf(InstallationFailurePoint point)
    {
        if (Point == point) throw new IOException($"Deterministic failure injection: {point}");
    }
}

public sealed class DownloadFailureInjection(long failAfterBytes)
{
    private int _triggered;

    public long FailAfterBytes { get; } = Math.Max(1, failAfterBytes);

    public bool TryLimitWrite(long received, int requested, out int writable)
    {
        writable = requested;
        if (received >= FailAfterBytes || received + requested < FailAfterBytes || Interlocked.Exchange(ref _triggered, 1) != 0)
        {
            return false;
        }

        writable = checked((int)(FailAfterBytes - received));
        return true;
    }
}

public sealed record LauncherSettings(
    bool LaunchOnStartup = false,
    bool MinimizeOnClose = true,
    string DownloadDirectory = "",
    int ConcurrentDownloads = 4,
    long BandwidthLimitBytesPerSecond = 0,
    long CacheSizeBytes = 20L * 1024 * 1024 * 1024,
    string DefaultGameDirectory = "",
    bool ReducedMotion = false,
    string ApiBaseUrl = "http://127.0.0.1:8080",
    IReadOnlyDictionary<string, string>? TrustedManifestKeysPem = null);

public sealed class LauncherOperationException(string message, Exception? inner = null) : Exception(message, inner);
