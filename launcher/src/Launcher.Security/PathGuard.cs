namespace Launcher.Security;

public static class PathGuard
{
    public static string ResolveUnderRoot(string root, string portablePath)
    {
        Launcher.Manifests.ManifestValidator.ValidatePortablePath(portablePath);
        var fullRoot = Path.GetFullPath(root);
        var candidate = Path.GetFullPath(Path.Combine(fullRoot, portablePath.Replace('/', Path.DirectorySeparatorChar)));
        var rootWithSeparator = fullRoot.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar) + Path.DirectorySeparatorChar;
        if (!candidate.StartsWith(rootWithSeparator, StringComparison.OrdinalIgnoreCase)) throw new InvalidDataException("Path escapes installation root.");
        EnsureNoReparsePointEscape(fullRoot, candidate);
        return candidate;
    }

    public static void EnsureSafeRoot(string root)
    {
        var fullRoot = Path.GetFullPath(root);
        Directory.CreateDirectory(fullRoot);
        var attributes = File.GetAttributes(fullRoot);
        if (attributes.HasFlag(FileAttributes.ReparsePoint)) throw new IOException("Installation root may not be a reparse point.");
    }

    private static void EnsureNoReparsePointEscape(string root, string candidate)
    {
        var relative = Path.GetRelativePath(root, candidate);
        var current = new DirectoryInfo(root);
        foreach (var segment in relative.Split(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar, StringSplitOptions.RemoveEmptyEntries))
        {
            current = new DirectoryInfo(Path.Combine(current.FullName, segment));
            if (!current.Exists) continue;
            if (current.Attributes.HasFlag(FileAttributes.ReparsePoint)) throw new IOException($"Reparse point in installation path: {current.FullName}");
        }
    }
}
