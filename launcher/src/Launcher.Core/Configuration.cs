using System.Text.Json;

namespace Launcher.Core;

public sealed record LauncherPaths(string Root, string DatabasePath, string CachePath, string DownloadsPath, string GamesPath)
{
    public static LauncherPaths FromRoot(string root)
    {
        var fullRoot = Path.GetFullPath(root);
        return new LauncherPaths(fullRoot, Path.Combine(fullRoot, "launcher.db"), Path.Combine(fullRoot, "cache"), Path.Combine(fullRoot, "downloads"), Path.Combine(fullRoot, "games"));
    }
}

public sealed class JsonSettingsStore(string path)
{
    private static readonly JsonSerializerOptions Options = new(JsonSerializerDefaults.Web) { WriteIndented = true };

    public async Task<LauncherSettings> LoadAsync(CancellationToken cancellationToken = default)
    {
        if (!File.Exists(path)) return new LauncherSettings();
        await using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read, 4096, FileOptions.Asynchronous | FileOptions.SequentialScan);
        return await JsonSerializer.DeserializeAsync<LauncherSettings>(stream, Options, cancellationToken).ConfigureAwait(false) ?? new LauncherSettings();
    }

    public async Task SaveAsync(LauncherSettings settings, CancellationToken cancellationToken = default)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
        var temporary = path + ".part";
        await using (var stream = new FileStream(temporary, FileMode.Create, FileAccess.Write, FileShare.None, 4096, FileOptions.Asynchronous | FileOptions.SequentialScan))
        {
            await JsonSerializer.SerializeAsync(stream, settings, Options, cancellationToken).ConfigureAwait(false);
            await stream.FlushAsync(cancellationToken).ConfigureAwait(false);
        }
        File.Move(temporary, path, true);
    }
}
