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

    [ObservableProperty]
    private string _networkRate = "0 B/s";

    [ObservableProperty]
    private string _peakRate = "0 B/s";

    [ObservableProperty]
    private string _diskRate = "0 B/s";

    [ObservableProperty]
    private int _upNextCount;

    public int ScheduledCount => Scheduled.Count;
    public int CompletedCount => Completed.Count;

    public ObservableCollection<DownloadEntry> Scheduled { get; } = [];

    public ObservableCollection<DownloadEntry> Completed { get; } = [];

    [ObservableProperty]
    private string _lastActionMessage = "No active downloads.";

    public DownloadsViewModel()
    {
        Scheduled.CollectionChanged += (_, _) => OnPropertyChanged(nameof(ScheduledCount));
        Completed.CollectionChanged += (_, _) => OnPropertyChanged(nameof(CompletedCount));
    }

    public void AttachRuntime(LauncherRuntime runtime) => _runtime = runtime;

    public void ApplyProgress(DownloadProgress progress)
    {
        NetworkRate = FormatRate(progress.BytesPerSecond);
        _peakBytesPerSecond = Math.Max(_peakBytesPerSecond, progress.BytesPerSecond);
        PeakRate = FormatRate(_peakBytesPerSecond);
        if (progress.State is DownloadJobState.Ready or DownloadJobState.Cancelled or DownloadJobState.Failed)
        {
            NetworkRate = "0 B/s";
        }

        var entryIndex = -1;
        for (var index = 0; index < Scheduled.Count; index++)
        {
            if (string.Equals(Scheduled[index].JobId, progress.JobId, StringComparison.Ordinal))
            {
                entryIndex = index;
                break;
            }
        }
        if (entryIndex < 0 || entryIndex >= Scheduled.Count)
        {
            LastActionMessage = $"{FormatBytes(progress.DownloadedBytes)} / {FormatBytes(progress.TotalBytes)} · {progress.State}";
            return;
        }

        var entry = Scheduled[entryIndex];
        var detail = FormatProgress(progress.DownloadedBytes, progress.TotalBytes, progress.State);
        Scheduled[entryIndex] = entry with { Detail = detail, State = progress.State };
        LastActionMessage = $"{entry.Title} · {detail}";
    }

    public void ApplyRuntimeJobs(IReadOnlyList<PersistedDownloadJob> jobs, IReadOnlyList<RuntimeGame> games)
    {
        var gameByBuild = games
            .Where(game => !string.IsNullOrWhiteSpace(game.BuildId))
            .ToDictionary(game => game.BuildId!, StringComparer.Ordinal);

        Scheduled.Clear();
        Completed.Clear();
        foreach (var job in jobs.OrderByDescending(job => job.UpdatedAt))
        {
            gameByBuild.TryGetValue(job.BuildId, out var game);
            var title = game?.Title ?? job.BuildId;
            var monogram = game?.Monogram ?? "G";
            var detail = FormatProgress(job);
            var timestamp = job.UpdatedAt.ToLocalTime().ToString("g", CultureInfo.CurrentCulture);
            var entry = new DownloadEntry(title, detail, timestamp, monogram, job.State == DownloadJobState.Ready ? "PLAY" : "VIEW", job.JobId, job.State, game?.Id);
            if (job.State == DownloadJobState.Ready)
            {
                Completed.Add(entry);
            }
            else if (job.State is not DownloadJobState.Cancelled)
            {
                Scheduled.Add(entry);
            }
        }

        LastActionMessage = jobs.Count == 0
            ? "No active downloads."
            : $"Showing {jobs.Count} persisted download job{(jobs.Count == 1 ? string.Empty : "s")}.";
        UpNextCount = 0;
        if (jobs.All(job => job.State is DownloadJobState.Ready or DownloadJobState.Cancelled or DownloadJobState.Failed)) NetworkRate = "0 B/s";
    }

    [RelayCommand]
    private async Task InstallScheduled(DownloadEntry? entry)
    {
        if (entry is null || !Scheduled.Any(item => string.Equals(item.JobId, entry.JobId, StringComparison.Ordinal)))
        {
            return;
        }

        if (_runtime is not null && !string.IsNullOrWhiteSpace(entry.GameId))
        {
            try
            {
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

        Scheduled.Remove(entry);
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
        _runtime?.PauseActiveDownload();
        LastActionMessage = "Download paused.";
    }

    [RelayCommand]
    private void ResumeActiveDownload()
    {
        _runtime?.ResumeActiveDownload();
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

    private static string FormatProgress(PersistedDownloadJob job) =>
        FormatProgress(job.DownloadedBytes, job.TotalBytes, job.State, job.LastError);

    private static string FormatProgress(long downloadedBytes, long totalBytes, DownloadJobState state, string? lastError = null)
    {
        var stateText = state switch
        {
            DownloadJobState.Ready => "Complete",
            DownloadJobState.Failed => $"Failed: {lastError ?? "unknown error"}",
            _ => state.ToString()
        };
        return $"{FormatBytes(downloadedBytes)} / {FormatBytes(totalBytes)} · {stateText}";
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
    string? GameId = null);
public sealed record DownloadTile(string Title, string Detail, double Progress);
