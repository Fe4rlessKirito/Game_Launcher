using Launcher.Security;
using System.IO.Compression;

namespace Launcher.Updater;

public sealed record UpdatePackage(string PackagePath, string ExpectedBlake3, string InstallDirectory, string ExecutablePath);

public static class UpdateCoordinator
{
    private const int MaxArchiveEntries = 100_000;
    private const long MaxPackageBytes = 4L * 1024 * 1024 * 1024;
    private const long MaxExpandedBytes = 4L * 1024 * 1024 * 1024;
    private const long MaxEntryBytes = 2L * 1024 * 1024 * 1024;

    public static async Task ApplyAsync(UpdatePackage package, CancellationToken cancellationToken = default)
    {
        var installDirectory = Path.GetFullPath(package.InstallDirectory);
        if (!File.Exists(package.PackagePath)) throw new FileNotFoundException("Update package was not found.", package.PackagePath);
        PathGuard.EnsureSafeRoot(installDirectory);
        var actual = await Hashing.ComputeFileHashAsync(package.PackagePath, cancellationToken).ConfigureAwait(false);
        if (!string.Equals(actual, package.ExpectedBlake3, StringComparison.Ordinal)) throw new InvalidDataException("Launcher update package hash mismatch.");
        var parentDirectory = Directory.GetParent(installDirectory)?.FullName ?? throw new InvalidDataException("Launcher install directory has no parent.");
        var staging = Path.Combine(parentDirectory, ".launcher-update-staging-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(staging);
        try
        {
            ExtractAndValidatePackage(package.PackagePath, staging, cancellationToken);
            var destination = Path.GetFullPath(package.ExecutablePath);
            var installPrefix = installDirectory.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar) + Path.DirectorySeparatorChar;
            if (!destination.StartsWith(installPrefix, StringComparison.OrdinalIgnoreCase)) throw new InvalidDataException("Updater destination escaped launcher directory.");
            var executableRelative = Path.GetRelativePath(installDirectory, destination).Replace('\\', '/');
            var stagedExecutable = PathGuard.ResolveUnderRoot(staging, executableRelative);
            if (!File.Exists(stagedExecutable)) throw new InvalidDataException("Validated staging executable is missing.");

            var backup = installDirectory + ".previous";
            if (Directory.Exists(backup)) Directory.Delete(backup, true);
            Directory.Move(installDirectory, backup);
            try
            {
                Directory.Move(staging, installDirectory);
                if (!File.Exists(destination)) throw new InvalidDataException("Swapped launcher executable is missing.");
                PreserveUserFiles(backup, installDirectory);
                Directory.Delete(backup, true);
            }
            catch
            {
                if (Directory.Exists(installDirectory)) Directory.Delete(installDirectory, true);
                if (Directory.Exists(backup)) Directory.Move(backup, installDirectory);
                throw;
            }
        }
        finally { if (Directory.Exists(staging)) Directory.Delete(staging, true); }
    }

    private static void ExtractAndValidatePackage(string packagePath, string staging, CancellationToken cancellationToken)
    {
        var packageLength = new FileInfo(packagePath).Length;
        if (packageLength > MaxPackageBytes) throw new InvalidDataException("Launcher update package is too large.");
        using var archive = ZipFile.OpenRead(packagePath);
        var seenPaths = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        long expandedBytes = 0;
        var entryCount = 0;
        foreach (var entry in archive.Entries)
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (++entryCount > MaxArchiveEntries) throw new InvalidDataException("Launcher update package has too many entries.");
            var portable = entry.FullName.Replace('\\', '/');
            if (string.IsNullOrEmpty(portable) || portable == "/") throw new InvalidDataException("Launcher update package contains an empty path.");
            var normalizedPath = portable.TrimEnd('/');
            if (!seenPaths.Add(normalizedPath)) throw new InvalidDataException($"Duplicate update package path: {portable}");
            if (entry.Length < 0 || entry.Length > MaxEntryBytes || expandedBytes > MaxExpandedBytes - entry.Length)
            {
                throw new InvalidDataException("Launcher update package expands beyond its safety limit.");
            }
            expandedBytes += entry.Length;
            if (portable.EndsWith('/'))
            {
                Directory.CreateDirectory(PathGuard.ResolveUnderRoot(staging, normalizedPath));
                continue;
            }
            var destination = PathGuard.ResolveUnderRoot(staging, portable);
            Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
            using var input = entry.Open();
            using var output = new FileStream(destination, FileMode.CreateNew, FileAccess.Write, FileShare.None);
            var buffer = new byte[1024 * 1024];
            long copied = 0;
            int read;
            while ((read = input.Read(buffer, 0, buffer.Length)) > 0)
            {
                cancellationToken.ThrowIfCancellationRequested();
                copied = checked(copied + read);
                if (copied > entry.Length) throw new InvalidDataException("Launcher update package entry length was invalid.");
                output.Write(buffer, 0, read);
            }
            if (copied != entry.Length) throw new InvalidDataException("Launcher update package entry was truncated.");
        }
    }

    private static void PreserveUserFiles(string backup, string destination)
    {
        if (!Directory.Exists(backup)) return;
        foreach (var source in Directory.EnumerateFiles(backup, "*", SearchOption.AllDirectories))
        {
            var relative = Path.GetRelativePath(backup, source);
            if (!IsUserFile(relative)) continue;
            var target = PathGuard.ResolveUnderRoot(destination, relative.Replace(Path.DirectorySeparatorChar, '/'));
            if (File.Exists(target)) continue;
            Directory.CreateDirectory(Path.GetDirectoryName(target)!);
            File.Move(source, target, true);
        }
    }

    private static bool IsUserFile(string relative)
    {
        var extension = Path.GetExtension(relative);
        return !string.Equals(extension, ".exe", StringComparison.OrdinalIgnoreCase)
            && !string.Equals(extension, ".dll", StringComparison.OrdinalIgnoreCase)
            && !string.Equals(extension, ".pdb", StringComparison.OrdinalIgnoreCase)
            && !relative.EndsWith(".deps.json", StringComparison.OrdinalIgnoreCase)
            && !relative.EndsWith(".runtimeconfig.json", StringComparison.OrdinalIgnoreCase);
    }
}

public static class Program
{
    public static Task Main(string[] args) => Task.CompletedTask;
}
