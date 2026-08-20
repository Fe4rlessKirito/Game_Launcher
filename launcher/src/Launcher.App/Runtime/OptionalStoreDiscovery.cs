using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.Text.Json;
using Launcher.Core;

namespace Launcher.App.Runtime;

public enum OptionalStoreProvider
{
    Gog,
    UbisoftConnect,
    EaApp,
    BattleNet,
    Xbox,
    Itch
}

public static class OptionalStoreProviderExtensions
{
    public static string DisplayName(this OptionalStoreProvider provider) => provider switch
    {
        OptionalStoreProvider.Gog => "GOG Galaxy",
        OptionalStoreProvider.UbisoftConnect => "Ubisoft Connect",
        OptionalStoreProvider.EaApp => "EA app",
        OptionalStoreProvider.BattleNet => "Battle.net",
        OptionalStoreProvider.Xbox => "Xbox / Microsoft Store",
        OptionalStoreProvider.Itch => "itch.io",
        _ => provider.ToString()
    };
}

public sealed record OptionalStoreGameInstall(
    OptionalStoreProvider Provider,
    string AppId,
    string Name,
    string InstallRoot,
    string LaunchPath,
    long SizeBytes,
    string? MetadataPath = null)
{
    public string ProviderName => Provider.DisplayName();
    public string SizeDisplay => SizeBytes <= 0 ? "Size unavailable" : FormatBytes(SizeBytes);

    private static string FormatBytes(long bytes)
    {
        var units = new[] { "B", "KB", "MB", "GB", "TB" };
        var value = (double)bytes;
        var unit = 0;
        while (value >= 1024 && unit < units.Length - 1)
        {
            value /= 1024;
            unit++;
        }

        return $"{value:0.#} {units[unit]}";
    }
}

public sealed record OptionalStoreSnapshot(
    OptionalStoreProvider Provider,
    bool Enabled,
    bool IsDetected,
    IReadOnlyList<string> InstallRoots,
    IReadOnlyList<OptionalStoreGameInstall> Games,
    string? Error)
{
    public string DisplayName => Provider.DisplayName();
    public string StatusText => !Enabled
        ? "Disabled"
        : Error
            ?? (!IsDetected
                ? $"{DisplayName} was not detected on this PC."
                : Games.Count == 0
                    ? $"{DisplayName} detected · No launchable games found."
                    : $"{Games.Count} installed {DisplayName} title{(Games.Count == 1 ? string.Empty : "s")} detected.");

    public static OptionalStoreSnapshot Disabled(OptionalStoreProvider provider) =>
        new(provider, false, false, [], [], null);
}

public static class OptionalStoreDiscovery
{
    private const int MaxGogManifestBytes = 4 * 1024 * 1024;
    private const int MaxDirectoriesPerRoot = 256;
    private const int MaxExecutablesPerDirectory = 128;
    private const int MaxExecutableSearchDepth = 3;
    private const int MaxExecutableCandidates = 512;

    public static IReadOnlyList<OptionalStoreSnapshot> Discover(
        IReadOnlyDictionary<OptionalStoreProvider, bool> enabled)
    {
        return Enum.GetValues<OptionalStoreProvider>()
            .Select(provider => Discover(
                provider,
                enabled.TryGetValue(provider, out var isEnabled) && isEnabled))
            .ToArray();
    }

    public static OptionalStoreSnapshot Discover(
        OptionalStoreProvider provider,
        bool enabled,
        IReadOnlyList<string>? rootsOverride = null)
    {
        if (!enabled)
        {
            return OptionalStoreSnapshot.Disabled(provider);
        }

        var roots = NormalizeRoots(rootsOverride ?? FindRoots(provider));
        if (roots.Count == 0)
        {
            return new OptionalStoreSnapshot(provider, true, false, [], [], null);
        }

        try
        {
            var games = provider == OptionalStoreProvider.Gog
                ? DiscoverGogGames(roots)
                : DiscoverDirectoryGames(provider, roots);
            var distinctGames = games
                .GroupBy(game => game.InstallRoot, StringComparer.OrdinalIgnoreCase)
                .Select(group => group.First())
                .OrderBy(game => game.Name, StringComparer.OrdinalIgnoreCase)
                .ToArray();
            return new OptionalStoreSnapshot(provider, true, true, roots, distinctGames, null);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidDataException or ArgumentException)
        {
            return new OptionalStoreSnapshot(provider, true, true, roots, [], error.Message);
        }
    }

    private static List<OptionalStoreGameInstall> DiscoverGogGames(IReadOnlyList<string> roots)
    {
        var games = new List<OptionalStoreGameInstall>();
        foreach (var root in roots)
        {
            foreach (var metadataPath in EnumerateGogMetadata(root))
            {
                var game = ReadGogManifest(metadataPath);
                if (game is not null)
                {
                    games.Add(game);
                }
            }
        }

        return games;
    }

    private static List<OptionalStoreGameInstall> DiscoverDirectoryGames(
        OptionalStoreProvider provider,
        IReadOnlyList<string> roots)
    {
        var games = new List<OptionalStoreGameInstall>();
        foreach (var root in roots)
        {
            foreach (var installRoot in EnumerateDirectories(root))
            {
                var launchPath = FindLaunchExecutable(installRoot);
                if (launchPath is null) continue;

                var name = Path.GetFileName(installRoot.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar));
                if (string.IsNullOrWhiteSpace(name)) continue;
                games.Add(new OptionalStoreGameInstall(
                    provider,
                    name,
                    name,
                    installRoot,
                    launchPath,
                    0));
            }
        }

        return games;
    }

    private static OptionalStoreGameInstall? ReadGogManifest(string metadataPath)
    {
        try
        {
            var info = new FileInfo(metadataPath);
            if (info.Length <= 0 || info.Length > MaxGogManifestBytes) return null;
            using var stream = new FileStream(metadataPath, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
            using var document = JsonDocument.Parse(stream, new JsonDocumentOptions
            {
                AllowTrailingCommas = true,
                CommentHandling = JsonCommentHandling.Skip,
                MaxDepth = 64
            });

            var root = document.RootElement;
            var installRoot = Path.GetDirectoryName(Path.GetFullPath(metadataPath));
            if (string.IsNullOrWhiteSpace(installRoot) || !Directory.Exists(installRoot)) return null;

            var appId = ReadScalar(root, "gameId") ?? Path.GetFileNameWithoutExtension(metadataPath);
            var name = ReadScalar(root, "name") ?? Path.GetFileName(installRoot);
            var launchPath = ReadGogLaunchPath(root, installRoot) ?? FindLaunchExecutable(installRoot);
            if (string.IsNullOrWhiteSpace(appId) || string.IsNullOrWhiteSpace(name) || launchPath is null) return null;
            var sizeBytes = ReadInt64(root, "installSize");
            return new OptionalStoreGameInstall(
                OptionalStoreProvider.Gog,
                appId,
                name,
                installRoot,
                launchPath,
                Math.Max(0, sizeBytes),
                Path.GetFullPath(metadataPath));
        }
        catch (JsonException)
        {
            return null;
        }
        catch (IOException)
        {
            return null;
        }
        catch (UnauthorizedAccessException)
        {
            return null;
        }
        catch (ArgumentException)
        {
            return null;
        }
    }

    private static string? ReadGogLaunchPath(JsonElement root, string installRoot)
    {
        if (!TryGetProperty(root, "playTasks", out var playTasks)
            || playTasks.ValueKind != JsonValueKind.Array)
        {
            return null;
        }

        var tasks = playTasks.EnumerateArray().ToArray();
        foreach (var task in tasks.OrderByDescending(task => ReadBoolean(task, "isPrimary")))
        {
            var relativePath = ReadScalar(task, "path") ?? ReadScalar(task, "exePath");
            var launchPath = ResolveExecutablePath(installRoot, relativePath);
            if (launchPath is not null) return launchPath;
        }

        return null;
    }

    private static IEnumerable<string> EnumerateGogMetadata(string root)
    {
        foreach (var path in EnumerateFiles(root, "goggame-*.info"))
        {
            yield return path;
        }

        foreach (var directory in EnumerateDirectories(root))
        {
            foreach (var path in EnumerateFiles(directory, "goggame-*.info"))
            {
                yield return path;
            }
        }
    }

    private static string? FindLaunchExecutable(string installRoot)
    {
        var candidates = EnumerateExecutableCandidates(installRoot).ToList();

        return candidates
            .Where(path => IsLikelyGameExecutable(path))
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .OrderByDescending(path => ScoreExecutable(installRoot, path))
            .ThenBy(path => path, StringComparer.OrdinalIgnoreCase)
            .FirstOrDefault();
    }

    private static IEnumerable<string> EnumerateExecutableCandidates(string installRoot)
    {
        var pending = new Queue<(string Path, int Depth)>();
        pending.Enqueue((installRoot, 0));
        var scannedDirectories = 0;
        var candidateCount = 0;

        while (pending.Count > 0 && scannedDirectories < MaxDirectoriesPerRoot)
        {
            var (directory, depth) = pending.Dequeue();
            scannedDirectories++;
            foreach (var executable in EnumerateFiles(directory, "*.exe"))
            {
                yield return executable;
                if (++candidateCount >= MaxExecutableCandidates) yield break;
            }

            if (depth >= MaxExecutableSearchDepth) continue;
            foreach (var child in EnumerateDirectories(directory).Take(64))
            {
                try
                {
                    if ((File.GetAttributes(child) & FileAttributes.ReparsePoint) != 0) continue;
                }
                catch (IOException)
                {
                    continue;
                }
                catch (UnauthorizedAccessException)
                {
                    continue;
                }

                pending.Enqueue((child, depth + 1));
            }
        }
    }

    private static int ScoreExecutable(string installRoot, string path)
    {
        var rootName = NormalizeName(Path.GetFileName(installRoot));
        var fileName = NormalizeName(Path.GetFileNameWithoutExtension(path));
        var score = fileName.Equals(rootName, StringComparison.OrdinalIgnoreCase) ? 100 : 0;
        if (path.Contains("\\Binaries\\", StringComparison.OrdinalIgnoreCase)
            || path.Contains("/Binaries/", StringComparison.OrdinalIgnoreCase))
        {
            score += 20;
        }

        try
        {
            score += (int)Math.Min(20, new FileInfo(path).Length / (1024L * 1024L * 100L));
        }
        catch (IOException)
        {
            // Metadata scoring is optional.
        }

        return score;
    }

    private static bool IsLikelyGameExecutable(string path)
    {
        var name = Path.GetFileNameWithoutExtension(path).ToLowerInvariant();
        var blocked = new[]
        {
            "unins", "uninstall", "setup", "install", "launcher", "updater", "update",
            "crash", "redist", "vcredist", "directx", "dxsetup", "eadesktop", "ealauncher",
            "ubisoft", "upc", "battle.net", "battlenet", "galaxyclient", "galaxyredist",
            "unitycrashhandler", "dotnet", "prereq"
        };
        return !blocked.Any(name.Contains)
            && !name.EndsWith("helper", StringComparison.Ordinal)
            && !name.EndsWith("service", StringComparison.Ordinal);
    }

    private static string? ResolveExecutablePath(string installRoot, string? value)
    {
        if (string.IsNullOrWhiteSpace(value)) return null;
        try
        {
            var path = Path.GetFullPath(Path.IsPathRooted(value)
                ? value
                : Path.Combine(installRoot, value.TrimStart('.', Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar)));
            return path.EndsWith(".exe", StringComparison.OrdinalIgnoreCase)
                && IsWithin(installRoot, path)
                && File.Exists(path)
                ? path
                : null;
        }
        catch (ArgumentException)
        {
            return null;
        }
        catch (NotSupportedException)
        {
            return null;
        }
    }

    private static List<string> FindRoots(OptionalStoreProvider provider)
    {
        var programFiles = Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles);
        var programFilesX86 = Environment.GetFolderPath(Environment.SpecialFolder.ProgramFilesX86);
        var localAppData = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        var roamingAppData = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
        var systemDrive = Environment.GetEnvironmentVariable("SystemDrive") ?? "C:";
        var roots = new List<string>();

        void Add(params string[] parts)
        {
            var basePath = parts[0];
            if (string.IsNullOrWhiteSpace(basePath)) return;
            roots.Add(parts.Length == 1 ? basePath : Path.Combine(parts));
        }

        switch (provider)
        {
            case OptionalStoreProvider.Gog:
                Add(systemDrive + "\\GOG Games");
                Add(programFilesX86, "GOG Galaxy", "Games");
                Add(programFiles, "GOG Galaxy", "Games");
                break;
            case OptionalStoreProvider.UbisoftConnect:
                Add(programFilesX86, "Ubisoft", "Ubisoft Game Launcher", "games");
                Add(programFiles, "Ubisoft", "Ubisoft Game Launcher", "games");
                break;
            case OptionalStoreProvider.EaApp:
                Add(programFilesX86, "EA Games");
                Add(programFiles, "EA Games");
                break;
            case OptionalStoreProvider.BattleNet:
                Add(programFilesX86, "Battle.net", "Games");
                Add(programFiles, "Battle.net", "Games");
                break;
            case OptionalStoreProvider.Xbox:
                Add(systemDrive + "\\XboxGames");
                break;
            case OptionalStoreProvider.Itch:
                Add(localAppData, "itch", "apps");
                Add(roamingAppData, "itch", "apps");
                Add(systemDrive + "\\itch\\apps");
                break;
        }

        return roots;
    }

    private static List<string> NormalizeRoots(IEnumerable<string> candidates)
    {
        var roots = new List<string>();
        foreach (var candidate in candidates)
        {
            try
            {
                var fullPath = Path.GetFullPath(candidate.Trim().Trim('"'));
                if (!Directory.Exists(fullPath)
                    || roots.Any(existing => string.Equals(existing, fullPath, StringComparison.OrdinalIgnoreCase)))
                {
                    continue;
                }

                roots.Add(fullPath);
            }
            catch (ArgumentException)
            {
                // Ignore malformed local paths.
            }
            catch (NotSupportedException)
            {
                // Ignore malformed local paths.
            }
        }

        return roots;
    }

    private static string[] EnumerateDirectories(string root)
    {
        try
        {
            return Directory.EnumerateDirectories(root, "*", SearchOption.TopDirectoryOnly)
                .Take(MaxDirectoriesPerRoot)
                .ToArray();
        }
        catch (IOException)
        {
            return [];
        }
        catch (UnauthorizedAccessException)
        {
            return [];
        }
    }

    private static string[] EnumerateFiles(string root, string pattern)
    {
        try
        {
            return Directory.EnumerateFiles(root, pattern, SearchOption.TopDirectoryOnly)
                .Take(MaxExecutablesPerDirectory)
                .ToArray();
        }
        catch (IOException)
        {
            return [];
        }
        catch (UnauthorizedAccessException)
        {
            return [];
        }
    }

    private static string? ReadScalar(JsonElement element, string propertyName)
    {
        if (!TryGetProperty(element, propertyName, out var property)) return null;
        return property.ValueKind switch
        {
            JsonValueKind.String => property.GetString()?.Trim(),
            JsonValueKind.Number => property.GetRawText(),
            _ => null
        };
    }

    private static long ReadInt64(JsonElement element, string propertyName)
    {
        var value = ReadScalar(element, propertyName);
        return long.TryParse(value, NumberStyles.Integer, CultureInfo.InvariantCulture, out var result) ? result : 0;
    }

    private static bool ReadBoolean(JsonElement element, string propertyName)
    {
        return TryGetProperty(element, propertyName, out var property)
            && ((property.ValueKind == JsonValueKind.True)
                || (property.ValueKind == JsonValueKind.String
                    && bool.TryParse(property.GetString(), out var value)
                    && value));
    }

    private static bool TryGetProperty(JsonElement element, string propertyName, out JsonElement value)
    {
        foreach (var property in element.EnumerateObject())
        {
            if (string.Equals(property.Name, propertyName, StringComparison.OrdinalIgnoreCase))
            {
                value = property.Value;
                return true;
            }
        }

        value = default;
        return false;
    }

    private static string NormalizeName(string value) =>
        new string(value.Where(char.IsLetterOrDigit).ToArray());

    private static bool IsWithin(string root, string candidate)
    {
        var normalizedRoot = Path.TrimEndingDirectorySeparator(Path.GetFullPath(root)) + Path.DirectorySeparatorChar;
        var normalizedCandidate = Path.TrimEndingDirectorySeparator(Path.GetFullPath(candidate)) + Path.DirectorySeparatorChar;
        return normalizedCandidate.StartsWith(normalizedRoot, StringComparison.OrdinalIgnoreCase)
            || string.Equals(
                Path.TrimEndingDirectorySeparator(Path.GetFullPath(root)),
                Path.TrimEndingDirectorySeparator(Path.GetFullPath(candidate)),
                StringComparison.OrdinalIgnoreCase);
    }

    public static bool IsPathWithinInstall(string root, string candidate) => IsWithin(root, candidate);
}

public static class OptionalStoreLauncher
{
    public static Task LaunchAsync(OptionalStoreGameInstall game)
    {
        if (!OperatingSystem.IsWindows())
        {
            throw new LauncherOperationException($"{game.ProviderName} launching is currently supported on Windows only.");
        }

        if (!File.Exists(game.LaunchPath)
            || !game.LaunchPath.EndsWith(".exe", StringComparison.OrdinalIgnoreCase))
        {
            throw new LauncherOperationException($"The launch executable for {game.Name} could not be found.");
        }

        if (!OptionalStoreDiscovery.IsPathWithinInstall(game.InstallRoot, game.LaunchPath))
        {
            throw new LauncherOperationException("The store metadata points outside the installed game directory.");
        }

        try
        {
            using var started = Process.Start(new ProcessStartInfo
            {
                FileName = game.LaunchPath,
                WorkingDirectory = game.InstallRoot,
                UseShellExecute = true
            });
            if (started is not null)
            {
                return Task.CompletedTask;
            }
        }
        catch (ArgumentException)
        {
            // Convert all launch failures into one actionable message.
        }
        catch (InvalidOperationException)
        {
            // Convert all launch failures into one actionable message.
        }
        catch (Win32Exception)
        {
            // Convert all launch failures into one actionable message.
        }

        throw new LauncherOperationException($"{game.ProviderName} could not launch {game.Name}.");
    }
}
