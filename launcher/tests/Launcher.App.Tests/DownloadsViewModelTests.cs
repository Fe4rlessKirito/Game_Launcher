using Launcher.App.ViewModels;
using Launcher.Core;

namespace Launcher.App.Tests;

public sealed class DownloadsViewModelTests
{
    [Fact]
    public void RuntimeJobsAreSeparatedByActualState()
    {
        var now = DateTimeOffset.UtcNow;
        var viewModel = new DownloadsViewModel();
        viewModel.ApplyRuntimeJobs(
        [
            new PersistedDownloadJob("active", "build-active", DownloadJobState.Downloading, 50, 100, now),
            new PersistedDownloadJob("queued", "build-queued", DownloadJobState.Queued, 0, 200, now.AddMinutes(-1)),
            new PersistedDownloadJob("complete", "build-complete", DownloadJobState.Ready, 300, 300, now.AddMinutes(-2)),
            new PersistedDownloadJob("failed", "build-failed", DownloadJobState.Failed, 20, 400, now.AddMinutes(-3), "provider unavailable")
        ],
        []);

        Assert.Single(viewModel.Active);
        Assert.Single(viewModel.UpNext);
        Assert.Single(viewModel.Completed);
        Assert.Single(viewModel.NeedsAttention);
        Assert.Equal("50 B / 100 B · Downloading", viewModel.Active[0].Detail);
        Assert.Equal(1, viewModel.UpNextCount);
    }

    [Fact]
    public void ProgressMovesEntryFromActiveToCompleted()
    {
        var viewModel = new DownloadsViewModel();
        viewModel.ApplyRuntimeJobs(
            [new PersistedDownloadJob("job", "build", DownloadJobState.Downloading, 10, 100, DateTimeOffset.UtcNow)],
            []);

        viewModel.ApplyProgress(new DownloadProgress("job", DownloadJobState.Downloading, 60, 100, 120, null));
        Assert.Equal(60, viewModel.Active[0].DownloadedBytes);
        Assert.Equal(60, viewModel.Active[0].ProgressPercentage);

        viewModel.ApplyProgress(new DownloadProgress("job", DownloadJobState.Ready, 100, 100, 0, TimeSpan.Zero));
        Assert.Empty(viewModel.Active);
        Assert.Single(viewModel.Completed);
        Assert.Equal("100 B / 100 B · Complete", viewModel.Completed[0].Detail);
    }
}
