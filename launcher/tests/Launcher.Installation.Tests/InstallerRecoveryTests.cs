using System.Text;
using System.Text.Json;
using Launcher.Core;
using Launcher.Installation;
using Launcher.Manifests;
using Launcher.Security;
using Launcher.Storage;
using Microsoft.Data.Sqlite;

namespace Launcher.Installation.Tests;

public sealed class InstallerRecoveryTests
{
    [Fact]
    public async Task UpdateReusesUntouchedFilesRemovesOwnedFilesAndPreservesUserData()
    {
        await using var fixture = await Fixture.CreateAsync();
        await fixture.Installer.InstallAsync(fixture.BuildA, fixture.InstallRoot);
        var unchanged = Path.Combine(fixture.InstallRoot, "unchanged.bin");
        var beforeWrite = File.GetLastWriteTimeUtc(unchanged);
        var summary = await fixture.Installer.UpdateAsync(fixture.BuildA, fixture.BuildB, fixture.InstallRoot);
        Assert.Equal(fixture.BuildA.Files.Single(file => file.Path == "unchanged.bin").Size, summary.ReusedInstalledBytes);
        Assert.Equal(beforeWrite, File.GetLastWriteTimeUtc(unchanged));
        Assert.False(File.Exists(Path.Combine(fixture.InstallRoot, "removed.bin")));
        Assert.True(File.Exists(Path.Combine(fixture.InstallRoot, "added.bin")));
        Assert.Equal("keep me", await File.ReadAllTextAsync(Path.Combine(fixture.InstallRoot, "user.txt")));
        Assert.Equal("save", await File.ReadAllTextAsync(Path.Combine(fixture.InstallRoot, "save.dat")));
        Assert.Empty(await Installer.VerifyAsync(fixture.BuildB, fixture.InstallRoot));
        Assert.Equal("build-b", (await fixture.State.GetInstalledGamesAsync()).Single().BuildId);
    }

    [Fact]
    public async Task RepairRestoresMissingModifiedAndTruncatedFilesOnly()
    {
        await using var fixture = await Fixture.CreateAsync();
        await fixture.Installer.InstallAsync(fixture.BuildB, fixture.InstallRoot);
        await File.WriteAllBytesAsync(Path.Combine(fixture.InstallRoot, "changed.bin"), new byte[] { 0 });
        File.Delete(Path.Combine(fixture.InstallRoot, "added.bin"));
        var unchanged = Path.Combine(fixture.InstallRoot, "unchanged.bin");
        var unchangedTimestamp = File.GetLastWriteTimeUtc(unchanged);
        await File.WriteAllBytesAsync(Path.Combine(fixture.InstallRoot, "empty.dat"), Array.Empty<byte>());
        await fixture.Installer.RepairAsync(fixture.BuildB, fixture.InstallRoot);
        Assert.Empty(await Installer.VerifyAsync(fixture.BuildB, fixture.InstallRoot));
        Assert.Equal(unchangedTimestamp, File.GetLastWriteTimeUtc(unchanged));
        Assert.Equal("keep me", await File.ReadAllTextAsync(Path.Combine(fixture.InstallRoot, "user.txt")));
    }

    [Fact]
    public async Task FailedInstallAndUpdateLeaveRecoverableJournals()
    {
        await using var fixture = await Fixture.CreateAsync();
        var failingInstall = new Installer(fixture.Cache, fixture.State, new InstallationFailureInjection(InstallationFailurePoint.AfterStagingFirstFile));
        await Assert.ThrowsAsync<IOException>(() => failingInstall.InstallAsync(fixture.BuildA, fixture.InstallRoot));
        await File.WriteAllTextAsync(Path.Combine(fixture.InstallRoot, ".launcher-stale.json.part"), "stale");
        await Installer.RecoverAsync(fixture.InstallRoot);
        Assert.Empty(Directory.EnumerateFiles(fixture.InstallRoot, "*.launcher-*.part", SearchOption.AllDirectories));
        Assert.Empty(Directory.EnumerateFiles(fixture.InstallRoot, ".launcher-*.json.part", SearchOption.TopDirectoryOnly));
        Assert.Empty(await fixture.State.GetInstalledGamesAsync());

        await fixture.Installer.InstallAsync(fixture.BuildA, fixture.InstallRoot);
        var failingUpdate = new Installer(fixture.Cache, fixture.State, new InstallationFailureInjection(InstallationFailurePoint.DuringUpdateFileSwap));
        await Assert.ThrowsAsync<IOException>(() => failingUpdate.UpdateAsync(fixture.BuildA, fixture.BuildB, fixture.InstallRoot));
        await Installer.RecoverAsync(fixture.InstallRoot);
        Assert.Empty(await Installer.VerifyAsync(fixture.BuildA, fixture.InstallRoot));
        Assert.True(File.Exists(Path.Combine(fixture.InstallRoot, "removed.bin")));
        Assert.False(File.Exists(Path.Combine(fixture.InstallRoot, "added.bin")));
    }

    [Theory]
    [InlineData(InstallationFailurePoint.AfterStagingAllFiles)]
    [InlineData(InstallationFailurePoint.BeforeDatabaseCommit)]
    [InlineData(InstallationFailurePoint.AfterFilesystemCommitBeforeDatabaseCommit)]
    public async Task EveryInstallCommitBoundaryRollsBackAfterRecovery(InstallationFailurePoint point)
    {
        await using var fixture = await Fixture.CreateAsync();
        var failing = new Installer(fixture.Cache, fixture.State, new InstallationFailureInjection(point));
        await Assert.ThrowsAsync<IOException>(() => failing.InstallAsync(fixture.BuildA, fixture.InstallRoot));

        await Installer.RecoverAsync(fixture.InstallRoot);

        Assert.Empty(await fixture.State.GetInstalledGamesAsync());
        Assert.Empty(Directory.EnumerateFiles(fixture.InstallRoot, ".launcher-*.json", SearchOption.TopDirectoryOnly));
        Assert.Empty(Directory.EnumerateFiles(fixture.InstallRoot, "*.launcher-*.part", SearchOption.AllDirectories));
        Assert.False(File.Exists(Path.Combine(fixture.InstallRoot, "SyntheticGame.exe")));
        Assert.Empty(Directory.EnumerateDirectories(Path.GetDirectoryName(fixture.InstallRoot)!, ".launcher-update-*.staging"));
    }

    [Fact]
    public async Task CancellationDuringStagingIsRecoverable()
    {
        await using var fixture = await Fixture.CreateAsync();
        using var cancellation = new CancellationTokenSource();
        var progress = new Progress<InstallationProgress>(value =>
        {
            if (value.CompletedFiles >= 1) cancellation.Cancel();
        });
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => fixture.Installer.InstallAsync(fixture.BuildB, fixture.InstallRoot, progress, cancellation.Token));
        await Installer.RecoverAsync(fixture.InstallRoot);
        Assert.Empty(await fixture.State.GetInstalledGamesAsync());
        Assert.NotEmpty(await Installer.VerifyAsync(fixture.BuildB, fixture.InstallRoot));
    }

    [Fact]
    public async Task UpdateRollbackAfterFilesystemCommitRestoresPreviousBuild()
    {
        await using var fixture = await Fixture.CreateAsync();
        await fixture.Installer.InstallAsync(fixture.BuildA, fixture.InstallRoot);
        var failing = new Installer(fixture.Cache, fixture.State, new InstallationFailureInjection(InstallationFailurePoint.AfterFilesystemCommitBeforeDatabaseCommit));
        await Assert.ThrowsAsync<IOException>(() => failing.UpdateAsync(fixture.BuildA, fixture.BuildB, fixture.InstallRoot));
        await Installer.RecoverAsync(fixture.InstallRoot);
        Assert.Empty(await Installer.VerifyAsync(fixture.BuildA, fixture.InstallRoot));
        Assert.Equal("build-a", (await fixture.State.GetInstalledGamesAsync()).Single().BuildId);
        Assert.False(File.Exists(Path.Combine(fixture.InstallRoot, "added.bin")));
        Assert.True(File.Exists(Path.Combine(fixture.InstallRoot, "removed.bin")));
    }

    [Fact]
    public async Task DiskSpacePreflightAndCancellationFailBeforeDatabaseCommit()
    {
        await using var fixture = await Fixture.CreateAsync();
        var noSpace = new Installer(fixture.Cache, fixture.State, availableSpaceProvider: _ => 0);
        await Assert.ThrowsAsync<IOException>(() => noSpace.InstallAsync(fixture.BuildA, fixture.InstallRoot));
        using var cancellation = new CancellationTokenSource();
        cancellation.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => fixture.Installer.InstallAsync(fixture.BuildA, fixture.InstallRoot, cancellationToken: cancellation.Token));
        Assert.Empty(await fixture.State.GetInstalledGamesAsync());
    }

    [Fact]
    public async Task MalformedOrOutOfScopeJournalsArePreserved()
    {
        await using var fixture = await Fixture.CreateAsync();
        var malformedId = Guid.NewGuid().ToString("N");
        var malformed = Path.Combine(fixture.InstallRoot, $".launcher-install-{malformedId}.json");
        await File.WriteAllTextAsync(malformed, "{not-json");

        var outOfScopeId = Guid.NewGuid().ToString("N");
        var outsideBackup = Path.Combine(fixture.Root, "attacker-backup");
        Directory.CreateDirectory(outsideBackup);
        await File.WriteAllTextAsync(Path.Combine(outsideBackup, "outside.txt"), "must remain outside");
        var outOfScope = Path.Combine(fixture.InstallRoot, $".launcher-install-{outOfScopeId}.json");
        await File.WriteAllTextAsync(outOfScope, JsonSerializer.Serialize(new
        {
            TransactionId = outOfScopeId,
            Operation = "install",
            GameId = "game",
            OldBuildId = "",
            NewBuildId = "build",
            State = "recoverable-failure",
            BackupRoot = outsideBackup,
            CommittedPaths = Array.Empty<string>(),
            RemovedPaths = Array.Empty<string>(),
            StartedAt = DateTimeOffset.UtcNow,
        }));

        await Installer.RecoverAsync(fixture.InstallRoot);

        Assert.True(File.Exists(malformed));
        Assert.True(File.Exists(outOfScope));
        Assert.True(File.Exists(Path.Combine(outsideBackup, "outside.txt")));
        Assert.False(File.Exists(Path.Combine(fixture.InstallRoot, "outside.txt")));
    }

    [Fact]
    public async Task UninstallRemovesOwnedFilesButPreservesSavesAndUnrelatedFiles()
    {
        await using var fixture = await Fixture.CreateAsync();
        await fixture.Installer.InstallAsync(fixture.BuildB, fixture.InstallRoot);
        var installed = (await fixture.State.GetInstalledGamesAsync()).Single();
        await fixture.Installer.UninstallAsync(installed);
        Assert.False(File.Exists(Path.Combine(fixture.InstallRoot, "changed.bin")));
        Assert.True(File.Exists(Path.Combine(fixture.InstallRoot, "user.txt")));
        Assert.True(File.Exists(Path.Combine(fixture.InstallRoot, "save.dat")));
        Assert.Empty(await fixture.State.GetInstalledGamesAsync());
    }

    [Fact]
    public async Task UnsupportedUserDataRemovalFailsBeforeChangingInstallation()
    {
        await using var fixture = await Fixture.CreateAsync();
        await fixture.Installer.InstallAsync(fixture.BuildB, fixture.InstallRoot);
        var installed = (await fixture.State.GetInstalledGamesAsync()).Single();

        await Assert.ThrowsAsync<NotSupportedException>(() => fixture.Installer.UninstallAsync(installed, removeUserData: true));

        Assert.True(File.Exists(Path.Combine(fixture.InstallRoot, "changed.bin")));
        Assert.Single(await fixture.State.GetInstalledGamesAsync());
    }

    private sealed class Fixture : IAsyncDisposable
    {
        public required string Root { get; init; }
        public required string InstallRoot { get; init; }
        public required ChunkCache Cache { get; init; }
        public required LocalStateStore State { get; init; }
        public required Installer Installer { get; init; }
        public required Manifest BuildA { get; init; }
        public required Manifest BuildB { get; init; }

        public static async Task<Fixture> CreateAsync()
        {
            var root = Path.Combine(Path.GetTempPath(), "launcher-installer-" + Guid.NewGuid().ToString("N"));
            var installRoot = Path.Combine(root, "install");
            Directory.CreateDirectory(installRoot);
            await File.WriteAllTextAsync(Path.Combine(installRoot, "user.txt"), "keep me");
            await File.WriteAllTextAsync(Path.Combine(installRoot, "save.dat"), "save");
            var cache = new ChunkCache(Path.Combine(root, "cache"), 256 * 1024 * 1024);
            await cache.InitializeAsync();
            var aFiles = new Dictionary<string, byte[]>(StringComparer.Ordinal)
            {
                ["SyntheticGame.exe"] = Encoding.UTF8.GetBytes("synthetic game A"),
                ["unchanged.bin"] = DeterministicBytes(100_000, 7),
                ["changed.bin"] = Encoding.UTF8.GetBytes("changed A"),
                ["removed.bin"] = Encoding.UTF8.GetBytes("remove me"),
                ["empty.dat"] = []
            };
            var bFiles = new Dictionary<string, byte[]>(StringComparer.Ordinal)
            {
                ["SyntheticGame.exe"] = Encoding.UTF8.GetBytes("synthetic game B"),
                ["unchanged.bin"] = aFiles["unchanged.bin"],
                ["changed.bin"] = Encoding.UTF8.GetBytes("changed B with a new payload"),
                ["added.bin"] = DeterministicBytes(80_000, 19),
                ["empty.dat"] = []
            };
            var buildA = await BuildManifestAsync(aFiles, "build-a", "A", cache);
            var buildB = await BuildManifestAsync(bFiles, "build-b", "B", cache);
            var state = new LocalStateStore(Path.Combine(root, "launcher.db"));
            await state.InitializeAsync();
            return new Fixture { Root = root, InstallRoot = installRoot, Cache = cache, State = state, Installer = new Installer(cache, state), BuildA = buildA, BuildB = buildB };
        }

        public ValueTask DisposeAsync()
        {
            SqliteConnection.ClearAllPools();
            if (Directory.Exists(Root)) Directory.Delete(Root, true);
            return ValueTask.CompletedTask;
        }

        private static async Task<Manifest> BuildManifestAsync(Dictionary<string, byte[]> files, string buildId, string version, ChunkCache cache)
        {
            var recipes = new List<FileRecipe>();
            foreach (var pair in files.OrderBy(pair => pair.Key, StringComparer.Ordinal))
            {
                var chunks = new List<ChunkReference>();
                if (pair.Value.Length > 0)
                {
                    using var compressor = new ZstdSharp.Compressor(3);
                    var encoded = compressor.Wrap(pair.Value).ToArray();
                    var rawHash = Hashing.ComputeHash(pair.Value);
                    var encodedHash = Hashing.ComputeHash(encoded);
                    await cache.PutAsync(encodedHash, encoded);
                    chunks.Add(new ChunkReference(rawHash, pair.Value.Length, encodedHash, encoded.Length, $"chunks/encoded/{encodedHash}.bin"));
                }
                recipes.Add(new FileRecipe(pair.Key, pair.Value.Length, Hashing.ComputeHash(pair.Value), chunks));
            }
            return new Manifest(1, "manifest-" + buildId, "synthetic-game", buildId, version, DateTimeOffset.UnixEpoch, ChunkingConfig.Default, EncodingConfig.Default, recipes, new LaunchProfile("SyntheticGame.exe", ".", [], new Dictionary<string, string>()));
        }

        private static byte[] DeterministicBytes(int length, byte seed)
        {
            var bytes = new byte[length];
            var value = (uint)(0xA5A5_0000 | seed);
            for (var index = 0; index < bytes.Length; index++)
            {
                value ^= value << 13;
                value ^= value >> 17;
                value ^= value << 5;
                bytes[index] = (byte)value;
            }
            return bytes;
        }
    }
}
