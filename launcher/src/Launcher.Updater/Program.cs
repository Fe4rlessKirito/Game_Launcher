using Launcher.Security;
using System.IO.Compression;

namespace Launcher.Updater;

public sealed record UpdatePackage(string PackagePath, string ExpectedBlake3, string InstallDirectory, string ExecutablePath);

public static class UpdateCoordinator
{
    public static async Task ApplyAsync(UpdatePackage package, CancellationToken cancellationToken = default)
    {
        var installDirectory = Path.GetFullPath(package.InstallDirectory);
        if (!File.Exists(package.PackagePath)) throw new FileNotFoundException("Update package was not found.", package.PackagePath);
        var actual = await Hashing.ComputeFileHashAsync(package.PackagePath, cancellationToken).ConfigureAwait(false);
        if (!string.Equals(actual, package.ExpectedBlake3, StringComparison.Ordinal)) throw new InvalidDataException("Launcher update package hash mismatch.");
        Directory.CreateDirectory(installDirectory);
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
        using var archive = ZipFile.OpenRead(packagePath);
        foreach (var entry in archive.Entries)
        {
            cancellationToken.ThrowIfCancellationRequested();
            var portable = entry.FullName.Replace('\\', '/');
            if (portable.EndsWith('/'))
            {
                if (portable.Length > 1) Directory.CreateDirectory(PathGuard.ResolveUnderRoot(staging, portable.TrimEnd('/')));
                continue;
            }
            var destination = PathGuard.ResolveUnderRoot(staging, portable);
            Directory.CreateDirectory(Path.GetDirectoryName(destination)!);
            using var input = entry.Open();
            using var output = new FileStream(destination, FileMode.CreateNew, FileAccess.Write, FileShare.None);
            input.CopyTo(output);
        }
    }
}

public static class Program
{
    public static Task Main(string[] args) => Task.CompletedTask;
}
