using System.ComponentModel;
using System.Diagnostics;
using System.Globalization;
using System.Text.Json;
using Launcher.Core;

namespace Launcher.App.Runtime;

public sealed record EpicGameInstall(
    string AppName,
    string Name,
    string InstallRoot,
    string LaunchExecutable,
    long SizeBytes,
    string ManifestPath)
{
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

public sealed record EpicLibrarySnapshot(
    IReadOnlyList<string> ManifestRoots,
    IReadOnlyList<EpicGameInstall> Games,
    string? Error)
{
    public static EpicLibrarySnapshot Empty { get; } = new([], [], null);
    public bool IsDetected => ManifestRoots.Count > 0;
}

public static class EpicLibraryDiscovery
{
    private const int MaxManifestBytes = 4 * 1024 * 1024;

    public static EpicLibrarySnapshot Discover(string? manifestRoot = null)
    {
        var roots = new List<string>();
        if (!string.IsNullOrWhiteSpace(manifestRoot))
        {
            AddManifestRoot(roots, manifestRoot);
        }
        else
        {
            foreach (var candidate in FindManifestRoots())
            {
                AddManifestRoot(roots, candidate);
            }
        }

        if (roots.Count == 0)
        {
            return new EpicLibrarySnapshot([], [], "Epic Games Launcher manifests were not found on this PC.");
        }

        var games = new List<EpicGameInstall>();
        foreach (var root in roots)
        {
            try
            {
                foreach (var manifestPath in Directory.EnumerateFiles(root, "*.item", SearchOption.TopDirectoryOnly))
                {
                    var game = ReadManifest(manifestPath);
                    if (game is not null)
                    {
                        games.Add(game);
                    }
                }
            }
            catch (IOException)
            {
                // A manifest directory can be rewritten while the Epic client
                // is updating. Keep other roots usable.
            }
            catch (UnauthorizedAccessException)
            {
                // A locked manifest directory must not hide other installs.
            }
        }

        var distinctGames = games
            .GroupBy(game => game.AppName, StringComparer.OrdinalIgnoreCase)
            .Select(group => group
                .OrderByDescending(game => Directory.Exists(game.InstallRoot))
                .First())
            .OrderBy(game => game.Name, StringComparer.OrdinalIgnoreCase)
            .ToArray();
        return new EpicLibrarySnapshot(roots, distinctGames, null);
    }

    public static string? FindEpicLauncherExecutable(string? launcherRoot = null)
    {
        if (!OperatingSystem.IsWindows()) return null;

        var candidates = new List<string>();
        if (!string.IsNullOrWhiteSpace(launcherRoot))
        {
            AddLauncherCandidates(candidates, launcherRoot);
        }
        else
        {
            var programFiles = Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles);
            var programFilesX86 = Environment.GetFolderPath(Environment.SpecialFolder.ProgramFilesX86);
            foreach (var basePath in new[] { programFilesX86, programFiles }.Distinct(StringComparer.OrdinalIgnoreCase))
            {
                if (string.IsNullOrWhiteSpace(basePath)) continue;
                AddLauncherCandidates(candidates, Path.Combine(basePath, "Epic Games", "Launcher"));
            }
        }

        return candidates.FirstOrDefault(File.Exists);
    }

    public static bool IsValidAppName(string? appName)
    {
        var value = appName?.Trim();
        return !string.IsNullOrWhiteSpace(value)
            && value.Length <= 256
            && value.All(character => char.IsLetterOrDigit(character) || character is '_' or '-' or '.');
    }

    private static EpicGameInstall? ReadManifest(string manifestPath)
    {
        try
        {
            var info = new FileInfo(manifestPath);
            if (info.Length <= 0 || info.Length > MaxManifestBytes) return null;

            using var stream = new FileStream(manifestPath, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
            using var document = JsonDocument.Parse(stream, new JsonDocumentOptions
            {
                AllowTrailingCommas = true,
                CommentHandling = JsonCommentHandling.Skip,
                MaxDepth = 64
            });
            var root = document.RootElement;
            var appName = ReadString(root, "AppName")?.Trim();
            var installLocation = ReadString(root, "InstallLocation")?.Trim();
            if (!IsValidAppName(appName) || string.IsNullOrWhiteSpace(installLocation) || !Path.IsPathRooted(installLocation))
            {
                return null;
            }

            var installRoot = Path.GetFullPath(installLocation);
            if (!Directory.Exists(installRoot)) return null;

            var launchExecutable = ReadString(root, "LaunchExecutable")?.Trim() ?? string.Empty;
            if (!IsValidLaunchExecutable(launchExecutable)) return null;
            if (launchExecutable.Length > 0)
            {
                var executablePath = Path.GetFullPath(Path.Combine(installRoot, launchExecutable));
                if (!IsWithin(installRoot, executablePath)) return null;
            }

            var name = ReadString(root, "DisplayName")?.Trim();
            if (string.IsNullOrWhiteSpace(name)) name = appName;
            var sizeBytes = ReadInt64(root, "InstallSize");
            return new EpicGameInstall(
                appName!,
                name!,
                installRoot,
                launchExecutable,
                Math.Max(0, sizeBytes),
                Path.GetFullPath(manifestPath));
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
        catch (NotSupportedException)
        {
            return null;
        }
    }

    private static IEnumerable<string> FindManifestRoots()
    {
        var commonData = Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData);
        if (!string.IsNullOrWhiteSpace(commonData))
        {
            yield return Path.Combine(commonData, "Epic", "EpicGamesLauncher", "Data", "Manifests");
        }
    }

    private static void AddManifestRoot(List<string> roots, string path)
    {
        try
        {
            var fullPath = Path.GetFullPath(path.Trim().Trim('"'));
            if (!Directory.Exists(fullPath)) return;
            if (!roots.Any(existing => string.Equals(existing, fullPath, StringComparison.OrdinalIgnoreCase)))
            {
                roots.Add(fullPath);
            }
        }
        catch (ArgumentException)
        {
            // Ignore malformed paths from local configuration.
        }
        catch (NotSupportedException)
        {
            // Ignore malformed paths from local configuration.
        }
    }

    private static void AddLauncherCandidates(List<string> candidates, string root)
    {
        try
        {
            var candidateRoot = root.Trim().Trim('"');
            if (File.Exists(candidateRoot))
            {
                if (string.Equals(Path.GetFileName(candidateRoot), "EpicGamesLauncher.exe", StringComparison.OrdinalIgnoreCase))
                {
                    candidates.Add(Path.GetFullPath(candidateRoot));
                }

                return;
            }

            candidates.Add(Path.Combine(candidateRoot, "Portal", "Binaries", "Win64", "EpicGamesLauncher.exe"));
            candidates.Add(Path.Combine(candidateRoot, "Portal", "Binaries", "Win32", "EpicGamesLauncher.exe"));
            candidates.Add(Path.Combine(candidateRoot, "EpicGamesLauncher.exe"));
        }
        catch (ArgumentException)
        {
            // Ignore malformed paths from local configuration.
        }
        catch (NotSupportedException)
        {
            // Ignore malformed paths from local configuration.
        }
    }

    private static string? ReadString(JsonElement root, string propertyName)
    {
        foreach (var property in root.EnumerateObject())
        {
            if (string.Equals(property.Name, propertyName, StringComparison.OrdinalIgnoreCase)
                && property.Value.ValueKind == JsonValueKind.String)
            {
                return property.Value.GetString();
            }
        }

        return null;
    }

    private static long ReadInt64(JsonElement root, string propertyName)
    {
        foreach (var property in root.EnumerateObject())
        {
            if (!string.Equals(property.Name, propertyName, StringComparison.OrdinalIgnoreCase)) continue;
            if (property.Value.ValueKind == JsonValueKind.Number
                && property.Value.TryGetInt64(out var number))
            {
                return number;
            }

            if (property.Value.ValueKind == JsonValueKind.String
                && long.TryParse(property.Value.GetString(), NumberStyles.Integer, CultureInfo.InvariantCulture, out var text))
            {
                return text;
            }
        }

        return 0;
    }

    private static bool IsValidLaunchExecutable(string value) =>
        value.Length <= 1024
        && !Path.IsPathRooted(value)
        && !value.Split(['/', '\\'], StringSplitOptions.RemoveEmptyEntries)
            .Any(segment => segment is "." or "..");

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
}

public static class EpicLauncher
{
    public static Task LaunchAsync(EpicGameInstall game)
    {
        if (!OperatingSystem.IsWindows())
        {
            throw new LauncherOperationException("Epic Games launching is currently supported on Windows only.");
        }

        if (!EpicLibraryDiscovery.IsValidAppName(game.AppName))
        {
            throw new LauncherOperationException("Epic returned an invalid application name.");
        }

        var uri = $"com.epicgames.launcher://apps/{Uri.EscapeDataString(game.AppName)}?action=launch&silent=true";
        try
        {
            using var started = Process.Start(new ProcessStartInfo
            {
                FileName = uri,
                UseShellExecute = true
            });
            if (started is not null)
            {
                return Task.CompletedTask;
            }
        }
        catch (ArgumentException)
        {
            // Fall through to the Epic executable handoff.
        }
        catch (InvalidOperationException)
        {
            // Fall through to the Epic executable handoff.
        }
        catch (PlatformNotSupportedException)
        {
            // Fall through to the Epic executable handoff.
        }
        catch (Win32Exception)
        {
            // Fall through to the Epic executable handoff.
        }

        var executable = EpicLibraryDiscovery.FindEpicLauncherExecutable();
        if (executable is null)
        {
            throw new LauncherOperationException("Epic Games Launcher is not installed or its executable could not be found.");
        }

        try
        {
            using var started = Process.Start(new ProcessStartInfo
            {
                FileName = executable,
                UseShellExecute = false,
                WorkingDirectory = Path.GetDirectoryName(executable) ?? string.Empty,
                ArgumentList = { $"-OpenApp={game.AppName}" }
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

        throw new LauncherOperationException("Epic Games Launcher could not open this game.");
    }

    public static Task OpenAsync()
    {
        if (!OperatingSystem.IsWindows())
        {
            throw new LauncherOperationException("Epic Games Launcher is currently supported on Windows only.");
        }

        var executable = EpicLibraryDiscovery.FindEpicLauncherExecutable();
        if (executable is null)
        {
            throw new LauncherOperationException("Epic Games Launcher is not installed or its executable could not be found.");
        }

        try
        {
            using var started = Process.Start(new ProcessStartInfo
            {
                FileName = executable,
                UseShellExecute = true,
                WorkingDirectory = Path.GetDirectoryName(executable) ?? string.Empty
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

        throw new LauncherOperationException("Epic Games Launcher could not be opened.");
    }
}
