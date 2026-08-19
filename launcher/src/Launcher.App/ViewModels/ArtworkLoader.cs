using System.Collections.Concurrent;
using Avalonia.Media.Imaging;

namespace Launcher.App.ViewModels;

internal static class ArtworkLoader
{
    private static readonly ConcurrentDictionary<string, Bitmap> Cache = new(StringComparer.OrdinalIgnoreCase);

    public static Bitmap? Load(string? source)
    {
        var path = ResolveLocalPath(source);
        if (path is null || !File.Exists(path)) return null;
        if (Cache.TryGetValue(path, out var cached)) return cached;

        try
        {
            using var stream = File.OpenRead(path);
            var bitmap = new Bitmap(stream);
            return Cache.GetOrAdd(path, bitmap);
        }
        catch (Exception) when (source is not null)
        {
            // Artwork is optional. A stale or unsupported Steam cache entry
            // must leave the monogram fallback usable.
            return null;
        }
    }

    private static string? ResolveLocalPath(string? source)
    {
        if (string.IsNullOrWhiteSpace(source)) return null;
        if (Path.IsPathRooted(source)) return Path.GetFullPath(source);

        return Uri.TryCreate(source, UriKind.Absolute, out var uri) && uri.IsFile
            ? uri.LocalPath
            : null;
    }
}
