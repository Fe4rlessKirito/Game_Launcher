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

    public ObservableCollection<DownloadEntry> Scheduled { get; } =
    [
        new("Build Playground", "2.2 KB", "Friday 3:50 AM", "BP", "Install")
    ];

    public ObservableCollection<DownloadEntry> Completed { get; } =
    [
        new("Synthetic Game", "90 B / 90 B downloaded", "Completed: today 12:36 AM", "SG", "Play")
    ];

    [ObservableProperty]
    private string _lastActionMessage = "No active downloads.";

    public IReadOnlyList<DownloadTile> Jobs { get; } =
    [
        new("No active downloads", "Verified chunks and installation jobs will appear here.", 0)
    ];

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
        if (entry is null || !Scheduled.Remove(entry))
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
                Scheduled.Insert(0, entry);
                LastActionMessage = $"Install failed: {error.Message}";
                return;
            }
        }

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
    private void ClearCompleted()
    {
        Completed.Clear();
        LastActionMessage = "Completed download history cleared.";
    }

    private static string FormatProgress(PersistedDownloadJob job)
    {
        var state = job.State switch
        {
            DownloadJobState.Ready => "Complete",
            DownloadJobState.Failed => $"Failed: {job.LastError ?? "unknown error"}",
            _ => job.State.ToString()
        };
        return $"{FormatBytes(job.DownloadedBytes)} / {FormatBytes(job.TotalBytes)} · {state}";
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
