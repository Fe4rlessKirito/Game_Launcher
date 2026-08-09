using System.IO.Compression;
using Launcher.Security;
using Launcher.Updater;

namespace Launcher.Updater.Tests;

public sealed class UpdaterTests
{
    [Fact]
    public async Task ValidPackageSwapsExecutableAndPreservesPackageIntegrity()
    {
        await using var fixture = await Fixture.CreateAsync(includeExecutable: true);
        await UpdateCoordinator.ApplyAsync(new UpdatePackage(fixture.PackagePath, Hashing.ComputeHash(await File.ReadAllBytesAsync(fixture.PackagePath)), fixture.InstallRoot, Path.Combine(fixture.InstallRoot, "launcher.exe")));
        Assert.Equal("new", await File.ReadAllTextAsync(Path.Combine(fixture.InstallRoot, "launcher.exe")));
        Assert.True(File.Exists(Path.Combine(fixture.InstallRoot, "unrelated.txt")));
    }

    [Fact]
    public async Task HashMismatchTraversalAndMissingExecutableAreRejectedWithoutReplacingOldInstall()
    {
        await using var wrongHash = await Fixture.CreateAsync(includeExecutable: true);
        await Assert.ThrowsAsync<InvalidDataException>(() => UpdateCoordinator.ApplyAsync(new UpdatePackage(wrongHash.PackagePath, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", wrongHash.InstallRoot, Path.Combine(wrongHash.InstallRoot, "launcher.exe"))));
        Assert.Equal("old", await File.ReadAllTextAsync(Path.Combine(wrongHash.InstallRoot, "launcher.exe")));

        await using var traversal = await Fixture.CreateAsync(includeExecutable: true, entryName: "../escape.txt");
        var traversalHash = Hashing.ComputeHash(await File.ReadAllBytesAsync(traversal.PackagePath));
        await Assert.ThrowsAsync<InvalidDataException>(() => UpdateCoordinator.ApplyAsync(new UpdatePackage(traversal.PackagePath, traversalHash, traversal.InstallRoot, Path.Combine(traversal.InstallRoot, "launcher.exe"))));
        Assert.False(File.Exists(Path.Combine(Path.GetDirectoryName(traversal.InstallRoot)!, "escape.txt")));

        await using var missing = await Fixture.CreateAsync(includeExecutable: false);
        var missingHash = Hashing.ComputeHash(await File.ReadAllBytesAsync(missing.PackagePath));
        await Assert.ThrowsAsync<InvalidDataException>(() => UpdateCoordinator.ApplyAsync(new UpdatePackage(missing.PackagePath, missingHash, missing.InstallRoot, Path.Combine(missing.InstallRoot, "launcher.exe"))));
        Assert.Equal("old", await File.ReadAllTextAsync(Path.Combine(missing.InstallRoot, "launcher.exe")));
    }

    private sealed class Fixture : IAsyncDisposable
    {
        public required string Root { get; init; }
        public required string InstallRoot { get; init; }
        public required string PackagePath { get; init; }

        public static async Task<Fixture> CreateAsync(bool includeExecutable, string? entryName = null)
        {
            var root = Path.Combine(Path.GetTempPath(), "launcher-updater-" + Guid.NewGuid().ToString("N"));
            var install = Path.Combine(root, "install");
            var package = Path.Combine(root, "update.zip");
            Directory.CreateDirectory(install);
            await File.WriteAllTextAsync(Path.Combine(install, "launcher.exe"), "old");
            await File.WriteAllTextAsync(Path.Combine(install, "unrelated.txt"), "keep");
            using (var archive = ZipFile.Open(package, ZipArchiveMode.Create))
            {
                if (includeExecutable)
                {
                    var entry = archive.CreateEntry("launcher.exe");
                    await using var writer = new StreamWriter(entry.Open());
                    await writer.WriteAsync("new");
                }
                if (entryName is not null)
                {
                    var entry = archive.CreateEntry(entryName);
                    await using var writer = new StreamWriter(entry.Open());
                    await writer.WriteAsync("escape");
                }
            }
            return new Fixture { Root = root, InstallRoot = install, PackagePath = package };
        }

        public ValueTask DisposeAsync()
        {
            if (Directory.Exists(Root)) Directory.Delete(Root, true);
            return ValueTask.CompletedTask;
        }
    }
}
