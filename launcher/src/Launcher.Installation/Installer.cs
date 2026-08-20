using System.Diagnostics;
using System.Text.Json;
using Launcher.Core;
using Launcher.Downloads;
using Launcher.Manifests;
using Launcher.Security;
using Launcher.Storage;

namespace Launcher.Installation;

public sealed record InstallationProgress(string Stage, string? Path, long CompletedFiles, long TotalFiles);

public sealed record UpdateSummary(long ReusedInstalledBytes, long ReconstructedBytes, int AddedFiles, int ChangedFiles, int RemovedFiles);

public sealed class Installer(
    ChunkCache cache,
    LocalStateStore stateStore,
    InstallationFailureInjection? failureInjection = null,
    Func<string, long>? availableSpaceProvider = null)
{
    public async Task InstallAsync(Manifest manifest, string installationRoot, IProgress<InstallationProgress>? progress = null, CancellationToken cancellationToken = default)
    {
        ManifestValidator.Validate(manifest);
        PathGuard.EnsureSafeRoot(installationRoot);
        EnsureCapacity(installationRoot, SumFileSizes(manifest.Files));
        var root = Path.GetFullPath(installationRoot);
        var transactionId = Guid.NewGuid().ToString("N");
        var backupRoot = Path.Combine(Path.GetDirectoryName(root)!, $".launcher-install-{transactionId}.backup");
        var journalPath = Path.Combine(root, $".launcher-install-{transactionId}.json");
        var journal = NewJournal(transactionId, "install", manifest.GameId, "", manifest.BuildId, backupRoot);
        await WriteJournalAsync(journalPath, journal, cancellationToken).ConfigureAwait(false);
        try
        {
            for (var index = 0; index < manifest.Files.Count; index++)
            {
                cancellationToken.ThrowIfCancellationRequested();
                var file = manifest.Files[index];
                progress?.Report(new InstallationProgress("Reconstructing", file.Path, index, manifest.Files.Count));
                var destination = PathGuard.ResolveUnderRoot(root, file.Path);
                Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
                var temporary = destination + $".launcher-{transactionId}.part";
                try
                {
                    await ReconstructFileAsync(file, temporary, cancellationToken).ConfigureAwait(false);
                    if (index == 0) failureInjection?.ThrowIf(InstallationFailurePoint.AfterStagingFirstFile);
                    BackupExistingFile(root, destination, backupRoot);
                    File.Move(temporary, destination, true);
                }
                finally
                {
                    TryDeleteFile(temporary);
                }
                journal = journal with { CommittedPaths = [.. journal.CommittedPaths, file.Path] };
                await WriteJournalAsync(journalPath, journal, cancellationToken).ConfigureAwait(false);
            }
            failureInjection?.ThrowIf(InstallationFailurePoint.AfterStagingAllFiles);
            journal = journal with { State = "filesystem-committed" };
            await WriteJournalAsync(journalPath, journal, cancellationToken).ConfigureAwait(false);
            failureInjection?.ThrowIf(InstallationFailurePoint.BeforeDatabaseCommit);
            failureInjection?.ThrowIf(InstallationFailurePoint.AfterFilesystemCommitBeforeDatabaseCommit);
            await stateStore.SaveInstalledGameAsync(new InstalledGame(manifest.GameId, manifest.BuildId, manifest.DisplayVersion, root, ManifestJson.Serialize(manifest), DateTimeOffset.UtcNow), cancellationToken).ConfigureAwait(false);
            journal = journal with { State = "committed" };
            await WriteJournalAsync(journalPath, journal, cancellationToken).ConfigureAwait(false);
            CleanupTransaction(journalPath, backupRoot);
            progress?.Report(new InstallationProgress("Committed", null, manifest.Files.Count, manifest.Files.Count));
        }
        catch
        {
            await TryWriteFailedJournalAsync(journalPath, journal).ConfigureAwait(false);
            throw;
        }
    }

    public async Task<UpdateSummary> UpdateAsync(Manifest previous, Manifest next, string installationRoot, IProgress<InstallationProgress>? progress = null, CancellationToken cancellationToken = default)
    {
        ManifestValidator.Validate(previous);
        ManifestValidator.Validate(next);
        if (!string.Equals(previous.GameId, next.GameId, StringComparison.Ordinal)) throw new InvalidDataException("Cannot update different games in one installation.");
        PathGuard.EnsureSafeRoot(installationRoot);
        var root = Path.GetFullPath(installationRoot);
        var oldFiles = previous.Files.ToDictionary(file => file.Path, StringComparer.Ordinal);
        var newFiles = next.Files.ToDictionary(file => file.Path, StringComparer.Ordinal);
        long requiredBytes;
        try
        {
            requiredBytes = checked(
                SumFileSizes(next.Files.Where(file => !oldFiles.TryGetValue(file.Path, out var old) || old.Blake3 != file.Blake3))
                + SumFileSizes(previous.Files.Where(file => !newFiles.TryGetValue(file.Path, out var nextFile) || nextFile.Blake3 != file.Blake3)));
        }
        catch (OverflowException error)
        {
            throw new InvalidDataException("Manifest update size exceeds the supported range.", error);
        }
        EnsureCapacity(root, requiredBytes);
        var transactionId = Guid.NewGuid().ToString("N");
        var stageRoot = Path.Combine(Path.GetDirectoryName(root)!, $".launcher-update-{transactionId}.staging");
        var backupRoot = Path.Combine(Path.GetDirectoryName(root)!, $".launcher-update-{transactionId}.backup");
        var journalPath = Path.Combine(root, $".launcher-update-{transactionId}.json");
        try
        {
            Directory.CreateDirectory(stageRoot);
            Directory.CreateDirectory(backupRoot);
        }
        catch
        {
            TryDeleteDirectory(stageRoot);
            TryDeleteDirectory(backupRoot);
            throw;
        }
        var journal = NewJournal(transactionId, "update", next.GameId, previous.BuildId, next.BuildId, backupRoot);
        try
        {
            await WriteJournalAsync(journalPath, journal, cancellationToken).ConfigureAwait(false);
        }
        catch
        {
            TryDeleteDirectory(stageRoot);
            TryDeleteDirectory(backupRoot);
            TryDeleteFile(journalPath);
            TryDeleteFile(journalPath + ".part");
            throw;
        }
        var unchangedPaths = new HashSet<string>(StringComparer.Ordinal);
        long reusedInstalledBytes = 0;
        long reconstructedBytes = 0;
        var changedFiles = 0;
        try
        {
            for (var index = 0; index < next.Files.Count; index++)
            {
                cancellationToken.ThrowIfCancellationRequested();
                var file = next.Files[index];
                progress?.Report(new InstallationProgress("Staging update", file.Path, index, next.Files.Count));
                var staged = PathGuard.ResolveUnderRoot(stageRoot, file.Path);
                Directory.CreateDirectory(Path.GetDirectoryName(staged)!);
                var existing = PathGuard.ResolveUnderRoot(root, file.Path);
                if (oldFiles.TryGetValue(file.Path, out var old) && old.Blake3 == file.Blake3 && await Hashing.VerifyFileAsync(existing, old.Size, old.Blake3, cancellationToken).ConfigureAwait(false))
                {
                    unchangedPaths.Add(file.Path);
                    reusedInstalledBytes += file.Size;
                }
                else
                {
                    changedFiles++;
                    reconstructedBytes += file.Size;
                    var stagedTemporary = staged + $".launcher-{transactionId}.part";
                    try
                    {
                        await ReconstructFileAsync(file, stagedTemporary, cancellationToken).ConfigureAwait(false);
                        File.Move(stagedTemporary, staged, true);
                    }
                    finally
                    {
                        TryDeleteFile(stagedTemporary);
                    }
                }
            }
            failureInjection?.ThrowIf(InstallationFailurePoint.AfterStagingAllFiles);

            foreach (var old in previous.Files.Where(file => !newFiles.ContainsKey(file.Path)))
            {
                var oldPath = PathGuard.ResolveUnderRoot(root, old.Path);
                if (File.Exists(oldPath))
                {
                    BackupExistingFile(root, oldPath, backupRoot);
                    journal = journal with { RemovedPaths = [.. journal.RemovedPaths, old.Path] };
                }
            }
            for (var index = 0; index < next.Files.Count; index++)
            {
                var file = next.Files[index];
                if (unchangedPaths.Contains(file.Path)) continue;
                var destination = PathGuard.ResolveUnderRoot(root, file.Path);
                var staged = PathGuard.ResolveUnderRoot(stageRoot, file.Path);
                Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
                BackupExistingFile(root, destination, backupRoot);
                File.Move(staged, destination, true);
                journal = journal with { CommittedPaths = [.. journal.CommittedPaths, file.Path] };
                await WriteJournalAsync(journalPath, journal, cancellationToken).ConfigureAwait(false);
                if (index == 0) failureInjection?.ThrowIf(InstallationFailurePoint.DuringUpdateFileSwap);
            }
            journal = journal with { State = "filesystem-committed" };
            await WriteJournalAsync(journalPath, journal, cancellationToken).ConfigureAwait(false);
            failureInjection?.ThrowIf(InstallationFailurePoint.AfterFilesystemCommitBeforeDatabaseCommit);
            await stateStore.SaveInstalledGameAsync(new InstalledGame(next.GameId, next.BuildId, next.DisplayVersion, root, ManifestJson.Serialize(next), DateTimeOffset.UtcNow), cancellationToken).ConfigureAwait(false);
            journal = journal with { State = "committed" };
            await WriteJournalAsync(journalPath, journal, cancellationToken).ConfigureAwait(false);
            CleanupTransaction(journalPath, backupRoot);
            if (Directory.Exists(stageRoot)) Directory.Delete(stageRoot, true);
            progress?.Report(new InstallationProgress("Updated", null, next.Files.Count, next.Files.Count));
            return new UpdateSummary(reusedInstalledBytes, reconstructedBytes, next.Files.Count(file => !oldFiles.ContainsKey(file.Path)), changedFiles, previous.Files.Count(file => !newFiles.ContainsKey(file.Path)));
        }
        catch
        {
            await TryWriteFailedJournalAsync(journalPath, journal).ConfigureAwait(false);
            throw;
        }
    }

    public static async Task<IReadOnlyList<string>> VerifyAsync(Manifest manifest, string installationRoot, IProgress<InstallationProgress>? progress = null, CancellationToken cancellationToken = default)
    {
        ManifestValidator.Validate(manifest);
        var root = Path.GetFullPath(installationRoot);
        PathGuard.EnsureSafeRoot(root);
        var invalid = new List<string>();
        for (var index = 0; index < manifest.Files.Count; index++)
        {
            cancellationToken.ThrowIfCancellationRequested();
            var file = manifest.Files[index];
            var path = PathGuard.ResolveUnderRoot(root, file.Path);
            if (!await Hashing.VerifyFileAsync(path, file.Size, file.Blake3, cancellationToken).ConfigureAwait(false)) invalid.Add(file.Path);
            progress?.Report(new InstallationProgress("Verifying", file.Path, index + 1, manifest.Files.Count));
        }
        return invalid;
    }

    public async Task RepairAsync(Manifest manifest, string installationRoot, IProgress<InstallationProgress>? progress = null, CancellationToken cancellationToken = default)
    {
        var invalid = await VerifyAsync(manifest, installationRoot, progress, cancellationToken).ConfigureAwait(false);
        var transactionId = Guid.NewGuid().ToString("N");
        foreach (var portablePath in invalid)
        {
            var file = manifest.Files.Single(item => item.Path == portablePath);
            var destination = PathGuard.ResolveUnderRoot(installationRoot, portablePath);
            Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
            var temporary = destination + $".launcher-{transactionId}.part";
            try
            {
                await ReconstructFileAsync(file, temporary, cancellationToken).ConfigureAwait(false);
                File.Move(temporary, destination, true);
            }
            finally
            {
                TryDeleteFile(temporary);
            }
        }
    }

    public async Task UninstallAsync(InstalledGame installed, bool removeUserData = false, CancellationToken cancellationToken = default)
    {
        if (removeUserData) throw new NotSupportedException("User-data removal requires an explicit provider-owned save path in a future release.");
        var manifest = ManifestJson.Deserialize(installed.ManifestJson);
        ManifestValidator.Validate(manifest);
        foreach (var file in manifest.Files)
        {
            cancellationToken.ThrowIfCancellationRequested();
            var path = PathGuard.ResolveUnderRoot(installed.InstallRoot, file.Path);
            if (File.Exists(path) && !File.GetAttributes(path).HasFlag(FileAttributes.ReparsePoint)) File.Delete(path);
        }
        foreach (var directory in manifest.Files.Select(file => Path.GetDirectoryName(PathGuard.ResolveUnderRoot(installed.InstallRoot, file.Path))).Where(path => path is not null).Distinct(StringComparer.OrdinalIgnoreCase).OrderByDescending(path => path!.Length))
        {
            try { if (Directory.Exists(directory) && !Directory.EnumerateFileSystemEntries(directory).Any()) Directory.Delete(directory); } catch (IOException) { }
        }
        await stateStore.RemoveInstalledGameAsync(installed.GameId, cancellationToken).ConfigureAwait(false);
    }

    public static Task RecoverAsync(string installationRoot, CancellationToken cancellationToken = default) =>
        RecoverAsync(installationRoot, (LocalStateStore)null!, cancellationToken);

    public static async Task RecoverAsync(string installationRoot, LocalStateStore stateStore, CancellationToken cancellationToken = default)
    {
        if (!Directory.Exists(installationRoot)) return;
        PathGuard.EnsureSafeRoot(installationRoot);
        var root = Path.GetFullPath(installationRoot);
        foreach (var journalPath in Directory.EnumerateFiles(root, ".launcher-*.json", SearchOption.TopDirectoryOnly))
        {
            cancellationToken.ThrowIfCancellationRequested();
            TransactionJournal? journal = null;
            try { journal = JsonSerializer.Deserialize<TransactionJournal>(await File.ReadAllTextAsync(journalPath, cancellationToken).ConfigureAwait(false)); } catch (JsonException) { }
            var databaseCommitted = journal is not null
                && stateStore is not null
                && await IsDatabaseCommittedAsync(stateStore, root, journal, cancellationToken).ConfigureAwait(false);
            if (journal is not null && journal.State != "committed" && !databaseCommitted) RestoreTransaction(root, journal);
            if (journal is not null && Directory.Exists(journal.BackupRoot)) Directory.Delete(journal.BackupRoot, true);
            File.Delete(journalPath);
        }
        foreach (var partial in Directory.EnumerateFiles(root, "*.launcher-*.part", SearchOption.AllDirectories))
        {
            cancellationToken.ThrowIfCancellationRequested();
            File.Delete(partial);
        }
        foreach (var journalTemporary in Directory.EnumerateFiles(root, ".launcher-*.json.part", SearchOption.TopDirectoryOnly))
        {
            cancellationToken.ThrowIfCancellationRequested();
            File.Delete(journalTemporary);
        }
        var parent = Directory.GetParent(root)?.FullName;
        if (parent is not null)
        {
            foreach (var staging in Directory.EnumerateDirectories(parent, ".launcher-update-*.staging", SearchOption.TopDirectoryOnly))
            {
                cancellationToken.ThrowIfCancellationRequested();
                Directory.Delete(staging, true);
            }
        }
        await Task.CompletedTask.ConfigureAwait(false);
    }

    private static async Task<bool> IsDatabaseCommittedAsync(LocalStateStore stateStore, string root, TransactionJournal journal, CancellationToken cancellationToken)
    {
        var installed = await stateStore.GetInstalledGamesAsync(cancellationToken).ConfigureAwait(false);
        return installed.Any(game =>
            string.Equals(game.GameId, journal.GameId, StringComparison.Ordinal)
            && string.Equals(game.BuildId, journal.NewBuildId, StringComparison.Ordinal)
            && string.Equals(Path.GetFullPath(game.InstallRoot), root, StringComparison.OrdinalIgnoreCase));
    }

    public static Process Launch(Manifest manifest, string installationRoot)
    {
        ManifestValidator.Validate(manifest);
        var executable = PathGuard.ResolveUnderRoot(installationRoot, manifest.Launch.Executable);
        var workingDirectory = manifest.Launch.WorkingDirectory == "." ? Path.GetFullPath(installationRoot) : PathGuard.ResolveUnderRoot(installationRoot, manifest.Launch.WorkingDirectory);
        var startInfo = new ProcessStartInfo(executable) { WorkingDirectory = Directory.Exists(workingDirectory) ? workingDirectory : Path.GetDirectoryName(executable)!, UseShellExecute = false };
        foreach (var argument in manifest.Launch.Arguments ?? Array.Empty<string>()) startInfo.ArgumentList.Add(argument);
        foreach (var pair in manifest.Launch.Environment ?? new Dictionary<string, string>()) startInfo.Environment[pair.Key] = pair.Value;
        return Process.Start(startInfo) ?? throw new LauncherOperationException("The game process could not be started.");
    }

    private async Task ReconstructFileAsync(FileRecipe file, string temporary, CancellationToken cancellationToken)
    {
        await using (var output = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None, 1024 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan))
        {
            foreach (var chunk in file.Chunks)
            {
                var encoded = await cache.ReadAsync(chunk.EncodedHash, cancellationToken).ConfigureAwait(false) ?? throw new FileNotFoundException($"Chunk is not in the verified cache: {chunk.EncodedHash}");
                var raw = ZstdCodec.Decompress(encoded);
                if (raw.LongLength != chunk.RawSize || !string.Equals(Hashing.ComputeHash(raw), chunk.RawHash, StringComparison.Ordinal)) throw new InvalidDataException($"Raw chunk verification failed: {chunk.RawHash}");
                await output.WriteAsync(raw, cancellationToken).ConfigureAwait(false);
            }
            await output.FlushAsync(cancellationToken).ConfigureAwait(false);
        }
        if (!await Hashing.VerifyFileAsync(temporary, file.Size, file.Blake3, cancellationToken).ConfigureAwait(false)) throw new InvalidDataException($"File verification failed: {file.Path}");
    }

    private void EnsureCapacity(string root, long requiredBytes)
    {
        if (availableSpaceProvider is not null && availableSpaceProvider(root) < requiredBytes) throw new IOException($"Insufficient disk space for {requiredBytes} bytes.");
    }

    private static long SumFileSizes(IEnumerable<FileRecipe> files)
    {
        try
        {
            return files.Aggregate(0L, (total, file) => checked(total + file.Size));
        }
        catch (OverflowException error)
        {
            throw new InvalidDataException("Manifest file sizes exceed the supported range.", error);
        }
    }

    private static TransactionJournal NewJournal(string id, string operation, string gameId, string oldBuild, string newBuild, string backupRoot) => new(id, operation, gameId, oldBuild, newBuild, "started", backupRoot, [], [], DateTimeOffset.UtcNow);

    private static void BackupExistingFile(string root, string destination, string backupRoot)
    {
        if (!File.Exists(destination)) return;
        if (File.GetAttributes(destination).HasFlag(FileAttributes.ReparsePoint)) throw new IOException($"Refusing to replace reparse point: {destination}");
        var relative = Path.GetRelativePath(root, destination);
        var backup = Path.Combine(backupRoot, relative);
        Directory.CreateDirectory(Path.GetDirectoryName(backup)!);
        File.Move(destination, backup, true);
    }

    private static void RestoreTransaction(string root, TransactionJournal journal)
    {
        foreach (var portable in journal.CommittedPaths)
        {
            var path = PathGuard.ResolveUnderRoot(root, portable);
            if (File.Exists(path) && !File.GetAttributes(path).HasFlag(FileAttributes.ReparsePoint)) File.Delete(path);
        }
        if (!Directory.Exists(journal.BackupRoot)) return;
        foreach (var backup in Directory.EnumerateFiles(journal.BackupRoot, "*", SearchOption.AllDirectories))
        {
            var relative = Path.GetRelativePath(journal.BackupRoot, backup);
            var destination = PathGuard.ResolveUnderRoot(root, relative.Replace(Path.DirectorySeparatorChar, '/'));
            Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
            File.Move(backup, destination, true);
        }
    }

    private static void CleanupTransaction(string journalPath, string backupRoot)
    {
        if (Directory.Exists(backupRoot)) Directory.Delete(backupRoot, true);
        if (File.Exists(journalPath)) File.Delete(journalPath);
    }

    private static async Task WriteJournalAsync(string path, TransactionJournal journal, CancellationToken cancellationToken)
    {
        var temporary = path + ".part";
        try
        {
            await File.WriteAllTextAsync(temporary, JsonSerializer.Serialize(journal), cancellationToken).ConfigureAwait(false);
            File.Move(temporary, path, true);
        }
        catch
        {
            TryDeleteFile(temporary);
            throw;
        }
    }

    private static async Task TryWriteFailedJournalAsync(string path, TransactionJournal journal)
    {
        try { await WriteJournalAsync(path, journal with { State = "recoverable-failure" }, CancellationToken.None).ConfigureAwait(false); } catch (IOException) { } catch (UnauthorizedAccessException) { }
    }

    private static void TryDeleteFile(string path)
    {
        try { if (File.Exists(path)) File.Delete(path); } catch (IOException) { } catch (UnauthorizedAccessException) { }
    }

    private static void TryDeleteDirectory(string path)
    {
        try { if (Directory.Exists(path)) Directory.Delete(path, true); } catch (IOException) { } catch (UnauthorizedAccessException) { }
    }

    private sealed record TransactionJournal(string TransactionId, string Operation, string GameId, string OldBuildId, string NewBuildId, string State, string BackupRoot, IReadOnlyList<string> CommittedPaths, IReadOnlyList<string> RemovedPaths, DateTimeOffset StartedAt);
}
