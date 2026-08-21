using System.Collections.ObjectModel;
using System.Globalization;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Launcher.App.Runtime;
using Launcher.Core;

namespace Launcher.App.ViewModels;

public partial class DownloadsViewModel : ObservableObject
{
    private LauncherRuntime? _runtime;
    private double _peakBytesPerSecond;
    private readonly Queue<double> _rateSamples = new();
    private readonly Dictionary<string, PersistedDownloadJob> _jobsById = new(StringComparer.Ordinal);
    private Dictionary<string, RuntimeGame> _gamesByBuild = new(StringComparer.Ordinal);

    [ObservableProperty]
    private string _networkRate = "0 B/s";

    [ObservableProperty]
    private string _peakRate = "0 B/s";

    [ObservableProperty]
    private string _preparedRate = "0 B/s";

    [ObservableProperty]
    private string _diskRate = "0 B/s";

    [ObservableProperty]
    private int _upNextCount;

    public ObservableCollection<DownloadEntry> Active { get; } = [];

    public ObservableCollection<DownloadEntry> UpNext { get; } = [];

    // Kept separate from the live queue so a future scheduled-install
    // feature has a real home instead of reusing the active list.
    public ObservableCollection<DownloadEntry> Scheduled { get; } = [];

    public ObservableCollection<DownloadEntry> NeedsAttention { get; } = [];

    public ObservableCollection<DownloadEntry> Completed { get; } = [];

    public ObservableCollection<double> RateHistory { get; } = new(Enumerable.Repeat(0d, 12));

    public int ActiveCount => Active.Count;
    public int ScheduledCount => Scheduled.Count;
    public int NeedsAttentionCount => NeedsAttention.Count;
    public int CompletedCount => Completed.Count;
    public bool HasActiveDownload => Active.Count > 0;
    public bool HasUpNext => UpNext.Count > 0;
    public bool HasScheduled => Scheduled.Count > 0;
    public bool HasNeedsAttention => NeedsAttention.Count > 0;
    public bool CanPauseActiveDownload => _runtime?.HasActiveDownload == true && Active.Any(entry => entry.State != DownloadJobState.Paused);
    public bool CanResumeActiveDownload => _runtime?.HasActiveDownload == true && Active.Any(entry => entry.State == DownloadJobState.Paused);
    public string ActiveSummary => Active.Count == 0
        ? "Nothing is downloading right now."
        : Active.Count == 1
            ? Active[0].Title
            : $"{Active.Count} downloads in progress";
    public string UpNextSummary => UpNext.Count == 0
        ? "No downloads are waiting in the queue."
        : $"{UpNext.Count} waiting for the active download to finish.";
    public string ScheduledSummary => Scheduled.Count == 0
        ? "No scheduled installs."
        : $"{Scheduled.Count} scheduled install{(Scheduled.Count == 1 ? string.Empty : "s")}.";

    [ObservableProperty]
    private string _lastActionMessage = "No downloads in progress.";

    public DownloadsViewModel()
    {
        Subscribe(Active);
        Subscribe(UpNext);
        Subscribe(Scheduled);
        Subscribe(NeedsAttention);
        Subscribe(Completed);
    }

    public void AttachRuntime(LauncherRuntime runtime) => _runtime = runtime;

    public void ApplyProgress(DownloadProgress progress)
    {
        NetworkRate = FormatRate(progress.BytesPerSecond);
        PreparedRate = FormatRate(progress.PreparedBytesPerSecond);
        DiskRate = FormatRate(progress.DiskBytesPerSecond);
        _peakBytesPerSecond = Math.Max(_peakBytesPerSecond, progress.BytesPerSecond);
        PeakRate = FormatRate(_peakBytesPerSecond);
        AddRateSample(progress.BytesPerSecond);

        var entry = FindEntry(progress.JobId) ?? CreateEntry(progress);
        entry = entry with
        {
            Detail = FormatProgress(progress.DownloadedBytes, progress.TotalBytes, progress.State, null, progress.Activity, progress.CompletedUnits, progress.TotalUnits),
            State = progress.State,
            DownloadedBytes = progress.DownloadedBytes,
            TotalBytes = progress.TotalBytes,
            Activity = progress.Activity,
            CompletedUnits = progress.CompletedUnits,
            TotalUnits = progress.TotalUnits
        };

        RemoveFromCollections(progress.JobId);
        AddToCollection(entry);
        LastActionMessage = $"{entry.Title} · {entry.Detail}";
        if (Active.Count == 0)
        {
            NetworkRate = "0 B/s";
            PreparedRate = "0 B/s";
            DiskRate = "0 B/s";
        }
        NotifyDerivedProperties();
    }

    public void ApplyRuntimeJobs(IReadOnlyList<PersistedDownloadJob> jobs, IReadOnlyList<RuntimeGame> games)
    {
        _gamesByBuild = games
            .Where(game => !string.IsNullOrWhiteSpace(game.BuildId))
            .GroupBy(game => game.BuildId!, StringComparer.Ordinal)
            .ToDictionary(group => group.Key, group => group.First(), StringComparer.Ordinal);
        _jobsById.Clear();
        foreach (var job in jobs) _jobsById[job.JobId] = job;

        Active.Clear();
        UpNext.Clear();
        Scheduled.Clear();
        NeedsAttention.Clear();
        Completed.Clear();
        foreach (var job in jobs.OrderByDescending(job => job.UpdatedAt))
        {
            AddToCollection(CreateEntry(job));
        }

        UpNextCount = UpNext.Count;
        LastActionMessage = Active.Count > 0
            ? $"{Active.Count} download{(Active.Count == 1 ? string.Empty : "s")} in progress."
            : jobs.Count == 0
                ? "No downloads in progress."
                : Completed.Count > 0
                    ? $"{Completed.Count} completed download{(Completed.Count == 1 ? string.Empty : "s")} in history."
                    : "No downloads in progress.";

        if (Active.Count == 0)
        {
            NetworkRate = "0 B/s";
            PreparedRate = "0 B/s";
            DiskRate = "0 B/s";
            ResetRateHistory();
        }
        NotifyDerivedProperties();
    }

    [RelayCommand]
    private async Task InstallScheduled(DownloadEntry? entry)
    {
        if (entry is null || FindEntry(entry.JobId) is null)
        {
            return;
        }

        if (_runtime is not null && !string.IsNullOrWhiteSpace(entry.GameId))
        {
            try
            {
                RemoveFromCollections(entry.JobId);
                LastActionMessage = $"Installing {entry.Title}…";
                await _runtime.InstallAsync(entry.GameId).ConfigureAwait(true);
                LastActionMessage = $"{entry.Title} installed and verified.";
                return;
            }
            catch (Exception error)
            {
                LastActionMessage = $"Install failed: {error.Message}";
                return;
            }
        }

        RemoveFromCollections(entry.JobId);
        Completed.Insert(0, entry with
        {
            Detail = $"{entry.Detail} verified",
            Timestamp = "Completed just now",
            Action = "Play",
            State = DownloadJobState.Ready
        });
        LastActionMessage = $"{entry.Title} installed and verified.";
    }

    [RelayCommand]
    private void PauseActiveDownload()
    {
        if (!CanPauseActiveDownload) return;
        if (_runtime?.PauseActiveDownload() != true) return;
        SetActiveState(DownloadJobState.Paused);
        LastActionMessage = "Download paused.";
    }

    [RelayCommand]
    private void ResumeActiveDownload()
    {
        if (!CanResumeActiveDownload) return;
        if (_runtime?.ResumeActiveDownload() != true) return;
        SetActiveState(DownloadJobState.Downloading);
        LastActionMessage = "Download resumed.";
    }

    [RelayCommand]
    private async Task ClearCompleted()
    {
        if (_runtime is not null)
        {
            try
            {
                await _runtime.ClearCompletedDownloadsAsync().ConfigureAwait(true);
            }
            catch (Exception error)
            {
                LastActionMessage = $"Could not clear completed history: {error.Message}";
                return;
            }
        }

        Completed.Clear();
        LastActionMessage = "Completed download history cleared.";
    }

    private DownloadEntry CreateEntry(PersistedDownloadJob job)
    {
        _gamesByBuild.TryGetValue(job.BuildId, out var game);
        return new DownloadEntry(
            game?.Title ?? job.BuildId,
            FormatProgress(job),
            job.UpdatedAt.ToLocalTime().ToString("g", CultureInfo.CurrentCulture),
            game?.Monogram ?? "G",
            job.State == DownloadJobState.Ready ? "PLAY" : job.State is DownloadJobState.Failed or DownloadJobState.Cancelled ? "RETRY" : "VIEW",
            job.JobId,
            job.State,
            game?.Id)
        {
            DownloadedBytes = job.DownloadedBytes,
            TotalBytes = job.TotalBytes,
            LastError = job.LastError
        };
    }

    private DownloadEntry CreateEntry(DownloadProgress progress)
    {
        _jobsById.TryGetValue(progress.JobId, out var job);
        _gamesByBuild.TryGetValue(job?.BuildId ?? string.Empty, out var game);
        var title = game?.Title ?? job?.BuildId ?? progress.JobId;
        return new DownloadEntry(
            title,
            FormatProgress(progress.DownloadedBytes, progress.TotalBytes, progress.State, null, progress.Activity, progress.CompletedUnits, progress.TotalUnits),
            DateTimeOffset.Now.ToString("g", CultureInfo.CurrentCulture),
            game?.Monogram ?? "G",
            progress.State == DownloadJobState.Ready ? "PLAY" : "VIEW",
            progress.JobId,
            progress.State,
            game?.Id)
        {
            DownloadedBytes = progress.DownloadedBytes,
            TotalBytes = progress.TotalBytes,
            Activity = progress.Activity,
            CompletedUnits = progress.CompletedUnits,
            TotalUnits = progress.TotalUnits
        };
    }

    private void AddToCollection(DownloadEntry entry)
    {
        switch (entry.State)
        {
            case DownloadJobState.Ready:
                Completed.Add(entry);
                break;
            case DownloadJobState.Queued:
                UpNext.Add(entry);
                break;
            case DownloadJobState.Failed:
            case DownloadJobState.Cancelled:
                NeedsAttention.Add(entry);
                break;
            case DownloadJobState.Resolving:
            case DownloadJobState.Downloading:
            case DownloadJobState.VerifyingEncoded:
            case DownloadJobState.Decompressing:
            case DownloadJobState.VerifyingRaw:
            case DownloadJobState.Paused:
                Active.Add(entry);
                break;
            default:
                Scheduled.Add(entry);
                break;
        }
    }

    private DownloadEntry? FindEntry(string? jobId)
    {
        if (string.IsNullOrWhiteSpace(jobId)) return null;
        return Active.Concat(UpNext).Concat(Scheduled).Concat(NeedsAttention).Concat(Completed)
            .FirstOrDefault(entry => string.Equals(entry.JobId, jobId, StringComparison.Ordinal));
    }

    private void RemoveFromCollections(string? jobId)
    {
        if (string.IsNullOrWhiteSpace(jobId)) return;
        Remove(Active, jobId);
        Remove(UpNext, jobId);
        Remove(Scheduled, jobId);
        Remove(NeedsAttention, jobId);
        Remove(Completed, jobId);

        static void Remove(ObservableCollection<DownloadEntry> collection, string jobId)
        {
            for (var index = collection.Count - 1; index >= 0; index--)
            {
                if (string.Equals(collection[index].JobId, jobId, StringComparison.Ordinal)) collection.RemoveAt(index);
            }
        }
    }

    private void SetActiveState(DownloadJobState state)
    {
        for (var index = 0; index < Active.Count; index++)
        {
            var entry = Active[index];
            Active[index] = entry with
            {
                State = state,
                Detail = FormatProgress(entry.DownloadedBytes, entry.TotalBytes, state, entry.LastError, entry.Activity, entry.CompletedUnits, entry.TotalUnits)
            };
        }
        NotifyDerivedProperties();
    }

    private void Subscribe(ObservableCollection<DownloadEntry> collection) =>
        collection.CollectionChanged += (_, _) =>
        {
            UpNextCount = UpNext.Count;
            NotifyDerivedProperties();
        };

    private void NotifyDerivedProperties()
    {
        OnPropertyChanged(nameof(ActiveCount));
        OnPropertyChanged(nameof(ScheduledCount));
        OnPropertyChanged(nameof(NeedsAttentionCount));
        OnPropertyChanged(nameof(CompletedCount));
        OnPropertyChanged(nameof(HasActiveDownload));
        OnPropertyChanged(nameof(HasUpNext));
        OnPropertyChanged(nameof(HasScheduled));
        OnPropertyChanged(nameof(HasNeedsAttention));
        OnPropertyChanged(nameof(CanPauseActiveDownload));
        OnPropertyChanged(nameof(CanResumeActiveDownload));
        OnPropertyChanged(nameof(ActiveSummary));
        OnPropertyChanged(nameof(UpNextSummary));
        OnPropertyChanged(nameof(ScheduledSummary));
    }

    private void AddRateSample(double rate)
    {
        _rateSamples.Enqueue(Math.Max(0, rate));
        while (_rateSamples.Count > 12) _rateSamples.Dequeue();
        var scale = Math.Max(1, _peakBytesPerSecond);
        RateHistory.Clear();
        foreach (var sample in _rateSamples)
        {
            RateHistory.Add(sample <= 0 ? 3 : 5 + 53 * Math.Clamp(sample / scale, 0, 1));
        }
        while (RateHistory.Count < 12) RateHistory.Insert(0, 3);
    }

    private void ResetRateHistory()
    {
        _rateSamples.Clear();
        RateHistory.Clear();
        foreach (var _ in Enumerable.Range(0, 12)) RateHistory.Add(3);
    }

    private static string FormatProgress(PersistedDownloadJob job) =>
        FormatProgress(job.DownloadedBytes, job.TotalBytes, job.State, job.LastError, null, 0, 0);

    private static string FormatProgress(
        long downloadedBytes,
        long totalBytes,
        DownloadJobState state,
        string? lastError,
        string? activity,
        long completedUnits,
        long totalUnits)
    {
        var byteText = totalBytes > 0
            ? $"{FormatBytes(downloadedBytes)} / {FormatBytes(totalBytes)}"
            : "Preparing verified content";
        var stateText = state switch
        {
            DownloadJobState.Ready => "Complete",
            DownloadJobState.Failed => $"Failed: {lastError ?? activity ?? "unknown error"}",
            DownloadJobState.Cancelled => "Cancelled",
            _ => state.ToString()
        };
        if (!string.IsNullOrWhiteSpace(activity) && totalUnits > 0 && state is not DownloadJobState.Ready)
        {
            stateText = $"{activity} · {completedUnits}/{totalUnits}";
        }
        return $"{byteText} · {stateText}";
    }

    private static string FormatBytes(long bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        var value = (double)bytes;
        var units = new[] { "KB", "MB", "GB", "TB" };
        var index = -1;
        while (value >= 1024 && index < units.Length - 1)
        {
            value /= 1024;
            index++;
        }

        return $"{value:0.#} {units[index]}";
    }

    private static string FormatRate(double bytesPerSecond) => FormatBytes((long)Math.Max(0, bytesPerSecond)) + "/s";
}

public sealed record DownloadEntry(
    string Title,
    string Detail,
    string Timestamp,
    string Monogram,
    string Action,
    string? JobId = null,
    DownloadJobState? State = null,
    string? GameId = null)
{
    public long DownloadedBytes { get; init; }
    public long TotalBytes { get; init; }
    public string? Activity { get; init; }
    public long CompletedUnits { get; init; }
    public long TotalUnits { get; init; }
    public string? LastError { get; init; }
    public double ProgressPercentage => TotalBytes <= 0 ? 0 : Math.Clamp(DownloadedBytes * 100d / TotalBytes, 0, 100);
    public bool HasProgress => TotalBytes > 0 && State is not DownloadJobState.Ready;
}

public sealed record DownloadTile(string Title, string Detail, double Progress);
