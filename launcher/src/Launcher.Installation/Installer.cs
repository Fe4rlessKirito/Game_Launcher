using System.Diagnostics;
using System.Text.Json;
using Launcher.Core;
using Launcher.Downloads;
using Launcher.Manifests;
using Launcher.Security;
using Launcher.Storage;

namespace Launcher.Installation;

public sealed record InstallationProgress(string Stage, string? Path, long CompletedFiles, long TotalFiles);

public sealed class Installer(ChunkCache cache, LocalStateStore stateStore)
{
    public async Task InstallAsync(Manifest manifest, string installationRoot, IProgress<InstallationProgress>? progress = null, CancellationToken cancellationToken = default)
    {
        ManifestValidator.Validate(manifest);
        PathGuard.EnsureSafeRoot(installationRoot);
        var transactionId = Guid.NewGuid().ToString("N");
        var journalPath = Path.Combine(Path.GetFullPath(installationRoot), $".launcher-install-{transactionId}.json");
        var journal = new InstallationJournal(transactionId, manifest.GameId, manifest.BuildId, "started", DateTimeOffset.UtcNow);
        await WriteJournalAsync(journalPath, journal, cancellationToken).ConfigureAwait(false);
        try
        {
            long completed = 0;
            foreach (var file in manifest.Files)
            {
                cancellationToken.ThrowIfCancellationRequested();
                progress?.Report(new InstallationProgress("Reconstructing", file.Path, completed, manifest.Files.Count));
                var destination = PathGuard.ResolveUnderRoot(installationRoot, file.Path);
                Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
                var temporary = destination + $".launcher-{transactionId}.part";
                await ReconstructFileAsync(file, temporary, cancellationToken).ConfigureAwait(false);
                File.Move(temporary, destination, true);
                completed++;
                journal = journal with { CompletedFiles = completed };
                await WriteJournalAsync(journalPath, journal, cancellationToken).ConfigureAwait(false);
            }
            progress?.Report(new InstallationProgress("Committed", null, manifest.Files.Count, manifest.Files.Count));
            var manifestJson = ManifestJson.Serialize(manifest);
            await stateStore.SaveInstalledGameAsync(new InstalledGame(manifest.GameId, manifest.BuildId, manifest.DisplayVersion, Path.GetFullPath(installationRoot), manifestJson, DateTimeOffset.UtcNow), cancellationToken).ConfigureAwait(false);
            journal = journal with { State = "committed" };
            await WriteJournalAsync(journalPath, journal, cancellationToken).ConfigureAwait(false);
            File.Delete(journalPath);
        }
        catch
        {
            journal = journal with { State = "recoverable-failure" };
            await WriteJournalAsync(journalPath, journal, CancellationToken.None).ConfigureAwait(false);
            throw;
        }
    }

    public static async Task<IReadOnlyList<string>> VerifyAsync(Manifest manifest, string installationRoot, IProgress<InstallationProgress>? progress = null, CancellationToken cancellationToken = default)
    {
        ManifestValidator.Validate(manifest);
        var invalid = new List<string>();
        long completed = 0;
        foreach (var file in manifest.Files)
        {
            cancellationToken.ThrowIfCancellationRequested();
            var path = PathGuard.ResolveUnderRoot(installationRoot, file.Path);
            if (!await Hashing.VerifyFileAsync(path, file.Size, file.Blake3, cancellationToken).ConfigureAwait(false)) invalid.Add(file.Path);
            completed++;
            progress?.Report(new InstallationProgress("Verifying", file.Path, completed, manifest.Files.Count));
        }
        return invalid;
    }

    public async Task RepairAsync(Manifest manifest, string installationRoot, IProgress<InstallationProgress>? progress = null, CancellationToken cancellationToken = default)
    {
        var invalid = await VerifyAsync(manifest, installationRoot, progress, cancellationToken).ConfigureAwait(false);
        if (invalid.Count == 0) return;
        var transactionId = Guid.NewGuid().ToString("N");
        foreach (var portablePath in invalid)
        {
            var file = manifest.Files.Single(item => item.Path == portablePath);
            var destination = PathGuard.ResolveUnderRoot(installationRoot, portablePath);
            var temporary = destination + $".launcher-{transactionId}.part";
            await ReconstructFileAsync(file, temporary, cancellationToken).ConfigureAwait(false);
            File.Move(temporary, destination, true);
        }
    }

    public async Task UninstallAsync(InstalledGame installed, bool removeUserData = false, CancellationToken cancellationToken = default)
    {
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
        if (removeUserData) throw new NotSupportedException("User-data removal requires an explicit provider-owned save path in a future release.");
        await stateStore.RemoveInstalledGameAsync(installed.GameId, cancellationToken).ConfigureAwait(false);
    }

    public static async Task RecoverAsync(string installationRoot, CancellationToken cancellationToken = default)
    {
        if (!Directory.Exists(installationRoot)) return;
        PathGuard.EnsureSafeRoot(installationRoot);
        foreach (var journal in Directory.EnumerateFiles(installationRoot, ".launcher-install-*.json", SearchOption.TopDirectoryOnly))
        {
            cancellationToken.ThrowIfCancellationRequested();
            File.Delete(journal);
        }
        foreach (var partial in Directory.EnumerateFiles(installationRoot, "*.launcher-*.part", SearchOption.AllDirectories))
        {
            cancellationToken.ThrowIfCancellationRequested();
            File.Delete(partial);
        }
        await Task.CompletedTask.ConfigureAwait(false);
    }

    public static Process Launch(Manifest manifest, string installationRoot)
    {
        ManifestValidator.Validate(manifest);
        var executable = PathGuard.ResolveUnderRoot(installationRoot, manifest.Launch.Executable);
        var workingDirectory = PathGuard.ResolveUnderRoot(installationRoot, manifest.Launch.WorkingDirectory == "." ? manifest.Launch.Executable : manifest.Launch.WorkingDirectory);
        var startInfo = new ProcessStartInfo(executable) { WorkingDirectory = Directory.Exists(workingDirectory) ? workingDirectory : Path.GetDirectoryName(executable)!, UseShellExecute = false };
        foreach (var argument in manifest.Launch.Arguments) startInfo.ArgumentList.Add(argument);
        foreach (var pair in manifest.Launch.Environment) startInfo.Environment[pair.Key] = pair.Value;
        return Process.Start(startInfo) ?? throw new LauncherOperationException("The game process could not be started.");
    }

    private async Task ReconstructFileAsync(FileRecipe file, string temporary, CancellationToken cancellationToken)
    {
        await using (var output = new FileStream(temporary, FileMode.Create, FileAccess.Write, FileShare.None, 1024 * 1024, FileOptions.Asynchronous | FileOptions.SequentialScan))
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

    private static async Task WriteJournalAsync(string path, InstallationJournal journal, CancellationToken cancellationToken)
    {
        await File.WriteAllTextAsync(path, JsonSerializer.Serialize(journal), cancellationToken).ConfigureAwait(false);
    }

    private sealed record InstallationJournal(string TransactionId, string GameId, string BuildId, string State, DateTimeOffset StartedAt, long CompletedFiles = 0);
}
