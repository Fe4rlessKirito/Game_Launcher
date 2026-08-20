using Launcher.Core;
using Launcher.Storage;
using Microsoft.Data.Sqlite;

namespace Launcher.Installation.Tests;

public sealed class LocalStateStoreTests
{
    [Fact]
    public async Task FreshMigrationPersistsJobsAndInstalledBuildAcrossReopen()
    {
        var root = Path.Combine(Path.GetTempPath(), "launcher-state-" + Guid.NewGuid().ToString("N"));
        try
        {
            Directory.CreateDirectory(root);
            var dbPath = Path.Combine(root, "launcher.db");
            var store = new LocalStateStore(dbPath);
            await store.InitializeAsync();
            await store.SaveDownloadJobAsync(new PersistedDownloadJob("job", "build-a", DownloadJobState.Downloading, 3, 9, DateTimeOffset.UtcNow));
            await store.SaveInstalledGameAsync(new InstalledGame("game", "build-a", "A", Path.Combine(root, "game"), "{}", DateTimeOffset.UtcNow));
            Assert.Equal("build-a", (await store.GetDownloadJobAsync("job"))!.BuildId);
            Assert.Single(await store.GetInstalledGamesAsync());

            var reopened = new LocalStateStore(dbPath);
            await reopened.InitializeAsync();
            Assert.Equal(DownloadJobState.Downloading, (await reopened.GetDownloadJobAsync("job"))!.State);
            await reopened.DeleteDownloadJobAsync("job");
            Assert.Null(await reopened.GetDownloadJobAsync("job"));
        }
        finally
        {
            SqliteConnection.ClearAllPools();
            if (Directory.Exists(root)) Directory.Delete(root, true);
        }
    }

    [Fact]
    public async Task ExistingOldDownloadSchemaGetsUpgradeColumn()
    {
        var root = Path.Combine(Path.GetTempPath(), "launcher-state-old-" + Guid.NewGuid().ToString("N"));
        try
        {
            Directory.CreateDirectory(root);
            var dbPath = Path.Combine(root, "launcher.db");
            using (var connection = new SqliteConnection(new SqliteConnectionStringBuilder { DataSource = dbPath }.ToString()))
            {
                connection.Open();
                using var command = connection.CreateCommand();
                command.CommandText = "CREATE TABLE download_jobs(job_id TEXT PRIMARY KEY, build_id TEXT NOT NULL, state TEXT NOT NULL, downloaded_bytes INTEGER NOT NULL, total_bytes INTEGER NOT NULL, updated_at TEXT NOT NULL);";
                command.ExecuteNonQuery();
            }
            var store = new LocalStateStore(dbPath);
            await store.InitializeAsync();
            await store.SaveDownloadJobAsync(new PersistedDownloadJob("job", "build", DownloadJobState.Queued, 0, 1, DateTimeOffset.UtcNow, "retry"));
            Assert.Equal("retry", (await store.GetDownloadJobAsync("job"))!.LastError);
        }
        finally
        {
            SqliteConnection.ClearAllPools();
            if (Directory.Exists(root)) Directory.Delete(root, true);
        }
    }

    [Fact]
    public async Task ClearCompletedDownloadJobsRemovesReadyJobsOnly()
    {
        var root = Path.Combine(Path.GetTempPath(), "launcher-state-clear-" + Guid.NewGuid().ToString("N"));
        try
        {
            Directory.CreateDirectory(root);
            var dbPath = Path.Combine(root, "launcher.db");
            var store = new LocalStateStore(dbPath);
            await store.InitializeAsync();
            await store.SaveDownloadJobAsync(new PersistedDownloadJob("ready", "build-ready", DownloadJobState.Ready, 10, 10, DateTimeOffset.UtcNow));
            await store.SaveDownloadJobAsync(new PersistedDownloadJob("failed", "build-failed", DownloadJobState.Failed, 2, 10, DateTimeOffset.UtcNow, "network"));
            await store.SaveDownloadJobAsync(new PersistedDownloadJob("active", "build-active", DownloadJobState.Downloading, 2, 10, DateTimeOffset.UtcNow));

            await store.DeleteCompletedDownloadJobsAsync();

            var remaining = await store.GetDownloadJobsAsync();
            Assert.DoesNotContain(remaining, job => job.JobId == "ready");
            Assert.Contains(remaining, job => job.JobId == "failed");
            Assert.Contains(remaining, job => job.JobId == "active");
        }
        finally
        {
            SqliteConnection.ClearAllPools();
            if (Directory.Exists(root)) Directory.Delete(root, true);
        }
    }

    [Fact]
    public async Task InterruptedDownloadJobsBecomeRetryableFailures()
    {
        var root = Path.Combine(Path.GetTempPath(), "launcher-state-recovery-" + Guid.NewGuid().ToString("N"));
        try
        {
            Directory.CreateDirectory(root);
            var store = new LocalStateStore(Path.Combine(root, "launcher.db"));
            await store.InitializeAsync();
            await store.SaveDownloadJobAsync(new PersistedDownloadJob("active", "build-active", DownloadJobState.Resolving, 2, 10, DateTimeOffset.UtcNow));
            await store.SaveDownloadJobAsync(new PersistedDownloadJob("ready", "build-ready", DownloadJobState.Ready, 10, 10, DateTimeOffset.UtcNow));
            await store.SaveDownloadJobAsync(new PersistedDownloadJob("failed", "build-failed", DownloadJobState.Failed, 2, 10, DateTimeOffset.UtcNow, "network"));

            Assert.Equal(1, await store.FailInterruptedDownloadJobsAsync());

            var jobs = await store.GetDownloadJobsAsync();
            var active = Assert.Single(jobs, job => job.JobId == "active");
            Assert.Equal(DownloadJobState.Failed, active.State);
            Assert.Equal("Download was interrupted; choose Install to retry.", active.LastError);
            Assert.Equal(DownloadJobState.Ready, Assert.Single(jobs, job => job.JobId == "ready").State);
            Assert.Equal("network", Assert.Single(jobs, job => job.JobId == "failed").LastError);
        }
        finally
        {
            SqliteConnection.ClearAllPools();
            if (Directory.Exists(root)) Directory.Delete(root, true);
        }
    }

    [Fact]
    public async Task LibraryCategoriesPersistExpansionOrderAndMembership()
    {
        var root = Path.Combine(Path.GetTempPath(), "launcher-state-categories-" + Guid.NewGuid().ToString("N"));
        try
        {
            Directory.CreateDirectory(root);
            var dbPath = Path.Combine(root, "launcher.db");
            var store = new LocalStateStore(dbPath);
            await store.InitializeAsync();
            await store.SaveLibraryCategoriesAsync(
            [
                new LibraryCategoryState("Co-op", 1, false, ["game-b", "game-a"]),
                new LibraryCategoryState("Favorites-ish", 0, true, ["game-c"]),
            ]);

            var categories = await new LocalStateStore(dbPath).GetLibraryCategoriesAsync();
            Assert.Equal(["Favorites-ish", "Co-op"], categories.Select(category => category.Name));
            Assert.False(categories[1].IsExpanded);
            Assert.Equal(["game-b", "game-a"], categories[1].GameIds);
        }
        finally
        {
            SqliteConnection.ClearAllPools();
            if (Directory.Exists(root)) Directory.Delete(root, true);
        }
    }
}
