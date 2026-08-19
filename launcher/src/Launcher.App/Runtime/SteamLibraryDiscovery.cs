using System.Runtime.Versioning;
using System.Text;
using System.Text.RegularExpressions;
using System.Diagnostics;
using Microsoft.Win32;

namespace Launcher.App.Runtime;

public sealed record SteamGameInstall(
    string AppId,
    string Name,
    string InstallDirectory,
    string InstallRoot,
    string LibraryRoot,
    long SizeBytes,
    string? ArtworkPath = null,
    string? IconArtworkPath = null,
    bool IsFavorite = false)
{
    public string SizeDisplay => FormatBytes(SizeBytes);

    private static string FormatBytes(long bytes)
    {
        if (bytes <= 0) return "Size unavailable";
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

public sealed record SteamLibrarySnapshot(
    IReadOnlyList<string> LibraryRoots,
    IReadOnlyList<SteamGameInstall> Games,
    string? Error)
{
    public static SteamLibrarySnapshot Empty { get; } = new([], [], null);
    public bool IsDetected => LibraryRoots.Count > 0;
}

public static class SteamLibraryDiscovery
{
    private const int MaxVdfBytes = 8 * 1024 * 1024;
    private const ulong SteamId64Base = 76561197960265728;

    public static SteamLibrarySnapshot Discover(string? steamRoot = null)
    {
        var roots = new List<string>();
        if (!string.IsNullOrWhiteSpace(steamRoot))
        {
            AddRoot(roots, steamRoot);
        }
        else
        {
            foreach (var candidate in FindSteamRoots())
            {
                AddRoot(roots, candidate);
            }
        }

        if (roots.Count == 0)
        {
            return new SteamLibrarySnapshot([], [], "Steam was not found on this PC.");
        }

        var libraryRoots = new List<string>();
        foreach (var root in roots)
        {
            AddRoot(libraryRoots, root);
            var libraryFoldersPath = Path.Combine(root, "steamapps", "libraryfolders.vdf");
            if (!File.Exists(libraryFoldersPath)) continue;

            foreach (var path in ReadLibraryFolderPaths(libraryFoldersPath))
            {
                AddRoot(libraryRoots, path);
            }
        }

        var favoriteAppIds = ReadFavoriteAppIds(roots);
        var games = new List<SteamGameInstall>();
        foreach (var libraryRoot in libraryRoots)
        {
            try
            {
                var steamAppsRoot = Path.Combine(libraryRoot, "steamapps");
                if (!Directory.Exists(steamAppsRoot)) continue;

                foreach (var manifestPath in Directory.EnumerateFiles(steamAppsRoot, "appmanifest_*.acf", SearchOption.TopDirectoryOnly))
                {
                    var game = ReadGameManifest(manifestPath, libraryRoot);
                    if (game is not null)
                    {
                        games.Add(game);
                    }
                }
            }
            catch (IOException)
            {
                // A disconnected or inaccessible library must not block the rest.
            }
            catch (UnauthorizedAccessException)
            {
                // A disconnected or inaccessible library must not block the rest.
            }
        }

        var distinctGames = games
            .GroupBy(game => game.AppId, StringComparer.OrdinalIgnoreCase)
            .Select(group => group.First())
            .OrderBy(game => game.Name, StringComparer.OrdinalIgnoreCase)
            .Select(game => game with
            {
                ArtworkPath = FindArtworkPath(roots, game.AppId),
                IconArtworkPath = FindIconArtworkPath(roots, game.AppId),
                IsFavorite = favoriteAppIds.Contains(game.AppId)
            })
            .ToArray();
        return new SteamLibrarySnapshot(libraryRoots, distinctGames, null);
    }

    private static SteamGameInstall? ReadGameManifest(string manifestPath, string libraryRoot)
    {
        try
        {
            var text = ReadBoundedText(manifestPath);
            var appId = ReadVdfValue(text, "appid")
                ?? Path.GetFileNameWithoutExtension(manifestPath)["appmanifest_".Length..];
            var installDirectory = ReadVdfValue(text, "installdir");
            if (string.IsNullOrWhiteSpace(appId) || string.IsNullOrWhiteSpace(installDirectory)) return null;

            var commonRoot = Path.GetFullPath(Path.Combine(libraryRoot, "steamapps", "common"));
            var installRoot = Path.GetFullPath(Path.Combine(commonRoot, installDirectory));
            if (!IsWithin(commonRoot, installRoot) || !Directory.Exists(installRoot)) return null;

            var sizeBytes = long.TryParse(ReadVdfValue(text, "SizeOnDisk"), out var parsedSize)
                ? Math.Max(0, parsedSize)
                : 0;
            return new SteamGameInstall(
                appId,
                ReadVdfValue(text, "name") ?? installDirectory,
                installDirectory,
                installRoot,
                libraryRoot,
                sizeBytes);
        }
        catch (IOException)
        {
            return null;
        }
        catch (InvalidDataException)
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

    private static string[] ReadLibraryFolderPaths(string path)
    {
        try
        {
            var text = ReadBoundedText(path);
            return Regex.Matches(text, "\\\"path\\\"\\s+\\\"(?<value>(?:\\\\.|[^\\\"\\\\])*)\\\"", RegexOptions.CultureInvariant)
                .Select(match => UnescapeVdf(match.Groups["value"].Value))
                .Where(value => !string.IsNullOrWhiteSpace(value))
                .ToArray();
        }
        catch (IOException)
        {
            return [];
        }
        catch (InvalidDataException)
        {
            return [];
        }
        catch (UnauthorizedAccessException)
        {
            return [];
        }
    }

    private static HashSet<string> ReadFavoriteAppIds(IEnumerable<string> steamRoots)
    {
        var favoriteAppIds = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var steamRoot in steamRoots)
        {
            var userdataRoot = Path.Combine(steamRoot, "userdata");
            string[] accountRoots;
            try
            {
                if (!Directory.Exists(userdataRoot)) continue;
                var allAccountRoots = Directory.EnumerateDirectories(userdataRoot).ToArray();
                var activeAccountIds = ReadActiveSteamAccountIds(Path.Combine(steamRoot, "config", "loginusers.vdf"));
                accountRoots = activeAccountIds is { Count: > 0 }
                    ? allAccountRoots
                        .Where(path => activeAccountIds.Contains(Path.GetFileName(path)))
                        .ToArray()
                    : allAccountRoots;
            }
            catch (IOException)
            {
                continue;
            }
            catch (UnauthorizedAccessException)
            {
                continue;
            }

            foreach (var accountRoot in accountRoots)
            {
                var configPaths = new[]
                {
                    Path.Combine(accountRoot, "config", "localconfig.vdf"),
                    Path.Combine(accountRoot, "config", "sharedconfig.vdf"),
                    Path.Combine(accountRoot, "7", "remote", "sharedconfig.vdf"),
                    Path.Combine(accountRoot, "7", "remote", "localconfig.vdf")
                };

                foreach (var configPath in configPaths.Distinct(StringComparer.OrdinalIgnoreCase))
                {
                    if (!File.Exists(configPath)) continue;
                    try
                    {
                        favoriteAppIds.UnionWith(ReadFavoriteAppIdsFromFile(configPath));
                    }
                    catch (IOException)
                    {
                        // Steam may be rewriting a config while it is being read.
                    }
                    catch (UnauthorizedAccessException)
                    {
                        // A locked account config must not hide the installed library.
                    }
                    catch (InvalidDataException)
                    {
                        // A malformed config is optional metadata, not a discovery failure.
                    }
                }
            }
        }

        return favoriteAppIds;
    }

    private static HashSet<string>? ReadActiveSteamAccountIds(string path)
    {
        if (!File.Exists(path)) return null;

        try
        {
            var document = ParseVdf(ReadBoundedText(path));
            var users = FindObjects(document, "users").FirstOrDefault();
            if (users is null) return null;

            var candidates = users.Values
                .Where(entry => ulong.TryParse(entry.Key, out _)
                    && entry.Value.Object is not null)
                .Select(entry =>
                {
                    var user = entry.Value.Object!;
                    var accountId = ToSteamAccountId(entry.Key);
                    var autoLogin = user.Values.TryGetValue("AutoLogin", out var autoLoginValue)
                        && string.Equals(autoLoginValue.Scalar, "1", StringComparison.Ordinal);
                    var timestamp = user.Values.TryGetValue("Timestamp", out var timestampValue)
                        && long.TryParse(timestampValue.Scalar, out var parsedTimestamp)
                            ? parsedTimestamp
                            : 0;
                    return (accountId, autoLogin, timestamp);
                })
                .Where(candidate => candidate.accountId is not null)
                .ToArray();
            if (candidates.Length == 0) return null;

            var active = candidates
                .Where(candidate => candidate.autoLogin)
                .Select(candidate => candidate.accountId!)
                .ToHashSet(StringComparer.OrdinalIgnoreCase);
            if (active.Count > 0) return active;

            var mostRecent = candidates
                .OrderByDescending(candidate => candidate.timestamp)
                .First()
                .accountId;
            return mostRecent is null
                ? null
                : new HashSet<string>([mostRecent], StringComparer.OrdinalIgnoreCase);
        }
        catch (IOException)
        {
            return null;
        }
        catch (UnauthorizedAccessException)
        {
            return null;
        }
        catch (InvalidDataException)
        {
            return null;
        }
    }

    private static string? ToSteamAccountId(string steamId64)
    {
        if (!ulong.TryParse(steamId64, out var parsed)) return null;
        if (parsed >= SteamId64Base) return (parsed - SteamId64Base).ToString(System.Globalization.CultureInfo.InvariantCulture);
        return parsed <= uint.MaxValue ? parsed.ToString(System.Globalization.CultureInfo.InvariantCulture) : null;
    }

    private static HashSet<string> ReadFavoriteAppIdsFromFile(string path)
    {
        var document = ParseVdf(ReadBoundedText(path));
        var favoriteAppIds = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var apps in FindObjects(document, "apps"))
        {
            foreach (var entry in apps.Values)
            {
                if (!long.TryParse(entry.Key, out _) || entry.Value.Object is not { } app) continue;
                if (!app.Values.TryGetValue("tags", out var tagValue) || tagValue.Object is not { } tags) continue;
                if (tags.Values.Values.Any(value => string.Equals(value.Scalar, "favorite", StringComparison.OrdinalIgnoreCase)))
                {
                    favoriteAppIds.Add(entry.Key);
                }
            }
        }

        return favoriteAppIds;
    }

    private static IEnumerable<VdfObject> FindObjects(VdfObject parent, string name)
    {
        foreach (var entry in parent.Values)
        {
            if (entry.Value.Object is not { } child) continue;
            if (string.Equals(entry.Key, name, StringComparison.OrdinalIgnoreCase))
            {
                yield return child;
            }

            foreach (var nested in FindObjects(child, name))
            {
                yield return nested;
            }
        }
    }

    private static VdfObject ParseVdf(string text)
    {
        var tokens = TokenizeVdf(text);
        var index = 0;
        var root = new VdfObject();
        ParseVdfObject(tokens, ref index, root, depth: 0);
        return root;
    }

    private static void ParseVdfObject(IReadOnlyList<VdfToken> tokens, ref int index, VdfObject target, int depth)
    {
        if (depth > 64) throw new InvalidDataException("Steam metadata nesting is too deep.");

        while (index < tokens.Count)
        {
            var key = tokens[index++];
            if (key.IsCloseBrace) return;
            if (key.IsOpenBrace) continue;
            if (index >= tokens.Count) return;

            var value = tokens[index++];
            if (value.IsCloseBrace) return;
            if (value.IsOpenBrace)
            {
                var child = new VdfObject();
                ParseVdfObject(tokens, ref index, child, depth + 1);
                target.Values[key.Value] = new VdfValue(null, child);
            }
            else
            {
                target.Values[key.Value] = new VdfValue(value.Value, null);
            }
        }
    }

    private static List<VdfToken> TokenizeVdf(string text)
    {
        var tokens = new List<VdfToken>();
        var index = 0;
        while (index < text.Length)
        {
            while (index < text.Length && char.IsWhiteSpace(text[index])) index++;
            if (index >= text.Length) break;

            if (text[index] == '/' && index + 1 < text.Length && text[index + 1] == '/')
            {
                index += 2;
                while (index < text.Length && text[index] != '\n') index++;
                continue;
            }

            if (text[index] == '{')
            {
                tokens.Add(new VdfToken("{", IsOpenBrace: true));
                index++;
                continue;
            }

            if (text[index] == '}')
            {
                tokens.Add(new VdfToken("}", IsCloseBrace: true));
                index++;
                continue;
            }

            if (text[index] == '"')
            {
                index++;
                var value = new StringBuilder();
                var closed = false;
                while (index < text.Length)
                {
                    var character = text[index++];
                    if (character == '"')
                    {
                        closed = true;
                        break;
                    }

                    if (character == '\\' && index < text.Length)
                    {
                        value.Append(character);
                        value.Append(text[index++]);
                    }
                    else
                    {
                        value.Append(character);
                    }
                }

                if (!closed) throw new InvalidDataException("Steam metadata contains an unterminated string.");
                tokens.Add(new VdfToken(UnescapeVdf(value.ToString())));
                continue;
            }

            var start = index;
            while (index < text.Length && !char.IsWhiteSpace(text[index]) && text[index] is not ('{' or '}')) index++;
            if (index > start)
            {
                tokens.Add(new VdfToken(text[start..index]));
            }
        }

        return tokens;
    }

    private static string? ReadVdfValue(string text, string key)
    {
        var pattern = $"\\\"{Regex.Escape(key)}\\\"\\s+\\\"(?<value>(?:\\\\.|[^\\\"\\\\])*)\\\"";
        var match = Regex.Match(text, pattern, RegexOptions.CultureInvariant | RegexOptions.IgnoreCase);
        return match.Success ? UnescapeVdf(match.Groups["value"].Value) : null;
    }

    private static string ReadBoundedText(string path)
    {
        var info = new FileInfo(path);
        if (info.Length > MaxVdfBytes)
        {
            throw new InvalidDataException($"Steam metadata is too large: {path}");
        }

        return File.ReadAllText(path, Encoding.UTF8);
    }

    private static string? FindArtworkPath(IEnumerable<string> steamRoots, string appId)
    {
        foreach (var steamRoot in steamRoots)
        {
            var cacheRoot = Path.Combine(steamRoot, "appcache", "librarycache", appId);
            if (!Directory.Exists(cacheRoot)) continue;

            var preferredNames = new[]
            {
                "header.jpg",
                "library_header.jpg",
                "library_hero.jpg",
                "library_600x900.jpg",
                "logo.png"
            };
            foreach (var name in preferredNames)
            {
                var candidate = Path.Combine(cacheRoot, name);
                if (File.Exists(candidate)) return candidate;
            }

            try
            {
                var fallback = Directory.EnumerateFiles(cacheRoot, "*.*", SearchOption.AllDirectories)
                    .Where(path => path.EndsWith(".jpg", StringComparison.OrdinalIgnoreCase)
                        || path.EndsWith(".jpeg", StringComparison.OrdinalIgnoreCase)
                        || path.EndsWith(".png", StringComparison.OrdinalIgnoreCase))
                    .OrderBy(path => path, StringComparer.OrdinalIgnoreCase)
                    .FirstOrDefault();
                if (fallback is not null) return fallback;
            }
            catch (IOException)
            {
                // Artwork is optional; a partially available cache is not fatal.
            }
            catch (UnauthorizedAccessException)
            {
                // Artwork is optional; a partially available cache is not fatal.
            }
        }

        return null;
    }

    private static string? FindIconArtworkPath(IEnumerable<string> steamRoots, string appId)
    {
        foreach (var steamRoot in steamRoots)
        {
            var cacheRoot = Path.Combine(steamRoot, "appcache", "librarycache", appId);
            if (!Directory.Exists(cacheRoot)) continue;

            try
            {
                // Steam's hash-named cache image is the compact 32x32 app icon.
                // Prefer it over logo.png, which is usually a wide capsule logo
                // and looks wrong when rendered in the sidebar.
                var hashedArtwork = Directory.EnumerateFiles(cacheRoot, "*.*", SearchOption.TopDirectoryOnly)
                    .Where(path => path.EndsWith(".jpg", StringComparison.OrdinalIgnoreCase)
                        || path.EndsWith(".jpeg", StringComparison.OrdinalIgnoreCase)
                        || path.EndsWith(".png", StringComparison.OrdinalIgnoreCase))
                    .Where(path => !Path.GetFileName(path).StartsWith("library_", StringComparison.OrdinalIgnoreCase)
                        && !string.Equals(Path.GetFileName(path), "header.jpg", StringComparison.OrdinalIgnoreCase)
                        && !string.Equals(Path.GetFileName(path), "header.jpeg", StringComparison.OrdinalIgnoreCase)
                        && !string.Equals(Path.GetFileName(path), "header.png", StringComparison.OrdinalIgnoreCase))
                    .Where(path => Path.GetFileNameWithoutExtension(path).Length >= 16)
                    .OrderBy(path => path, StringComparer.OrdinalIgnoreCase)
                    .FirstOrDefault();
                if (hashedArtwork is not null) return hashedArtwork;

                var logo = Path.Combine(cacheRoot, "logo.png");
                if (File.Exists(logo)) return logo;

                foreach (var name in new[] { "header.jpg", "library_header.jpg", "library_hero.jpg", "library_600x900.jpg" })
                {
                    var candidate = Path.Combine(cacheRoot, name);
                    if (File.Exists(candidate)) return candidate;
                }
            }
            catch (IOException)
            {
                // Artwork is optional; a partially available cache is not fatal.
            }
            catch (UnauthorizedAccessException)
            {
                // Artwork is optional; a partially available cache is not fatal.
            }
        }

        return null;
    }

    private static string UnescapeVdf(string value)
    {
        var builder = new StringBuilder(value.Length);
        for (var index = 0; index < value.Length; index++)
        {
            if (value[index] != '\\' || index + 1 >= value.Length)
            {
                builder.Append(value[index]);
                continue;
            }

            var escaped = value[++index];
            builder.Append(escaped switch
            {
                '\\' => '\\',
                '"' => '"',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                _ => escaped
            });
        }

        return builder.ToString();
    }

    private static List<string> FindSteamRoots()
    {
        var candidates = new List<string>();
        AddCandidate(candidates, Environment.GetEnvironmentVariable("STEAM_PATH"));
        if (OperatingSystem.IsWindows())
        {
            AddCandidate(candidates, ReadSteamRegistryValue(RegistryHive.CurrentUser, @"Software\Valve\Steam", "SteamPath"));
            AddCandidate(candidates, ReadSteamRegistryValue(RegistryHive.LocalMachine, @"Software\Valve\Steam", "InstallPath"));
            AddCandidate(candidates, ReadSteamRegistryValue(RegistryHive.LocalMachine, @"Software\WOW6432Node\Valve\Steam", "InstallPath"));
        }

        var programFilesX86 = Environment.GetFolderPath(Environment.SpecialFolder.ProgramFilesX86);
        var programFiles = Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles);
        AddCandidate(candidates, Path.Combine(programFilesX86, "Steam"));
        AddCandidate(candidates, Path.Combine(programFiles, "Steam"));
        return candidates;
    }

    [SupportedOSPlatform("windows")]
    private static string? ReadSteamRegistryValue(RegistryHive hive, string subKey, string valueName)
    {
        try
        {
            using var baseKey = RegistryKey.OpenBaseKey(hive, RegistryView.Default);
            using var key = baseKey.OpenSubKey(subKey);
            return key?.GetValue(valueName) as string;
        }
        catch (Exception) when (OperatingSystem.IsWindows())
        {
            return null;
        }
        catch (PlatformNotSupportedException)
        {
            return null;
        }
    }

    private static void AddCandidate(List<string> candidates, string? path)
    {
        if (!string.IsNullOrWhiteSpace(path)) candidates.Add(path);
    }

    private static void AddRoot(List<string> roots, string path)
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
            // Ignore malformed paths in Steam metadata.
        }
        catch (NotSupportedException)
        {
            // Ignore malformed paths in Steam metadata.
        }
    }

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

    private sealed class VdfObject
    {
        public Dictionary<string, VdfValue> Values { get; } = new(StringComparer.OrdinalIgnoreCase);
    }

    private sealed record VdfValue(string? Scalar, VdfObject? Object);

    private readonly record struct VdfToken(string Value, bool IsOpenBrace = false, bool IsCloseBrace = false);
}

public static class SteamLauncher
{
    public static Task LaunchAsync(SteamGameInstall game)
    {
        if (!OperatingSystem.IsWindows())
        {
            throw new Launcher.Core.LauncherOperationException("Steam launching is currently supported on Windows only.");
        }

        var started = Process.Start(new ProcessStartInfo
        {
            FileName = $"steam://rungameid/{game.AppId}",
            UseShellExecute = true
        });
        if (started is null)
        {
            throw new Launcher.Core.LauncherOperationException("Steam could not open this game.");
        }

        started.Dispose();
        return Task.CompletedTask;
    }
}
