using System.Text.Json;
using Avalonia;
using Avalonia.Media;
using Avalonia.Media.Imaging;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Launcher.Core;

namespace Launcher.App.ViewModels;

public partial class SettingsViewModel : ObservableObject
{
    private const string DefaultApiBaseUrl = "https://vaultnode.pp.ua";
    private const string LegacyLocalApiBaseUrl = "http://127.0.0.1:8080";
    private const string LegacyMantleIpApiBaseUrl = "http://5.231.32.191";
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web) { WriteIndented = true };
    private static readonly JsonSerializerOptions ReadOptions = new(JsonSerializerDefaults.Web);
    private static readonly string DefaultSettingsPath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "Vaultnode",
        "settings.json");
    private readonly string _settingsPath;

    [ObservableProperty]
    private bool _launchOnStartup;

    [ObservableProperty]
    private bool _minimizeOnClose = true;

    [ObservableProperty]
    private bool _reducedMotion;

    [ObservableProperty]
    private int _concurrentDownloads = 4;

    [ObservableProperty]
    private int _cacheSizeGigabytes = 20;

    [ObservableProperty]
    private string _themePreset = "Slate";

    [ObservableProperty]
    private string _accentColor = "#1A9FFF";

    [ObservableProperty]
    private bool _compactMode;

    [ObservableProperty]
    private string _downloadDirectory = @"C:\Games\Launcher\Downloads";

    [ObservableProperty]
    private string _installDirectory = @"C:\Games";

    [ObservableProperty]
    private string _apiBaseUrl = DefaultApiBaseUrl;

    [ObservableProperty]
    private string _profileImagePath = string.Empty;

    [ObservableProperty]
    private Bitmap? _profileImage;

    [ObservableProperty]
    private string _saveStatus = "Changes are stored locally.";

    [ObservableProperty]
    private string _selectedSection = "Profile";

    public bool HasProfileImage => ProfileImage is not null;
    public bool ShowDefaultProfile => !HasProfileImage;
    public bool IsProfileSelected => SelectedSection == "Profile";
    public bool IsGeneralSelected => SelectedSection == "General";
    public bool IsDownloadsSelected => SelectedSection == "Downloads";
    public bool IsAppearanceSelected => SelectedSection == "Appearance";
    public bool IsAdvancedSelected => SelectedSection == "Advanced";
    public IReadOnlyList<string> ThemeOptions { get; } = ["Slate", "Midnight", "Graphite"];

    private IReadOnlyDictionary<string, string>? _trustedManifestKeysPem;
    private bool _requireTrustedManifestKeys;

    public SettingsViewModel(string? settingsPath = null)
    {
        _settingsPath = string.IsNullOrWhiteSpace(settingsPath) ? DefaultSettingsPath : settingsPath;
        Load();
    }

    [RelayCommand]
    private void Save()
    {
        ConcurrentDownloads = Math.Clamp(ConcurrentDownloads, 1, 32);
        DownloadDirectory = string.IsNullOrWhiteSpace(DownloadDirectory)
            ? @"C:\Games\Launcher\Downloads"
            : DownloadDirectory.Trim();
        InstallDirectory = string.IsNullOrWhiteSpace(InstallDirectory)
            ? @"C:\Games"
            : InstallDirectory.Trim();
        ApiBaseUrl = string.IsNullOrWhiteSpace(ApiBaseUrl)
            ? DefaultApiBaseUrl
            : ApiBaseUrl.Trim().TrimEnd('/');

        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(_settingsPath)!);
            var snapshot = new LauncherSettings(
                LaunchOnStartup,
                MinimizeOnClose,
                DownloadDirectory,
                ConcurrentDownloads,
                CacheSizeBytes: (long)CacheSizeGigabytes * 1024L * 1024L * 1024L,
                DefaultGameDirectory: InstallDirectory,
                ReducedMotion: ReducedMotion,
                ApiBaseUrl: ApiBaseUrl,
                TrustedManifestKeysPem: _trustedManifestKeysPem,
                RequireTrustedManifestKeys: _requireTrustedManifestKeys,
                ProfileImagePath: ProfileImagePath,
                ThemePreset: ThemePreset,
                AccentColor: AccentColor,
                CompactMode: CompactMode);
            File.WriteAllText(_settingsPath, JsonSerializer.Serialize(snapshot, JsonOptions));
            SaveStatus = "Changes saved locally.";
        }
        catch (IOException)
        {
            SaveStatus = "Could not save settings locally.";
        }
        catch (UnauthorizedAccessException)
        {
            SaveStatus = "Could not save settings locally.";
        }
    }

    [RelayCommand]
    private void Reset()
    {
        LaunchOnStartup = false;
        MinimizeOnClose = true;
        ReducedMotion = false;
        ConcurrentDownloads = 4;
        CacheSizeGigabytes = 20;
        ThemePreset = "Slate";
        AccentColor = "#1A9FFF";
        CompactMode = false;
        DownloadDirectory = @"C:\Games\Launcher\Downloads";
        InstallDirectory = @"C:\Games";
        ApiBaseUrl = DefaultApiBaseUrl;
        ProfileImagePath = string.Empty;
        SaveStatus = "Defaults restored. Save to keep them.";
    }

    [RelayCommand]
    private void SelectSection(string? section)
    {
        SelectedSection = section switch
        {
            "General" => "General",
            "Downloads" => "Downloads",
            "Appearance" => "Appearance",
            "Advanced" => "Advanced",
            _ => "Profile"
        };
    }

    public void SetProfileImagePath(string? path)
    {
        ProfileImagePath = path?.Trim() ?? string.Empty;
        SaveStatus = string.IsNullOrWhiteSpace(ProfileImagePath)
            ? "Default profile picture selected. Save to keep it."
            : "Profile picture selected. Save to keep it.";
    }

    [RelayCommand]
    private void ClearProfileImage()
    {
        SetProfileImagePath(string.Empty);
    }

    private void Load()
    {
        try
        {
            if (!File.Exists(_settingsPath))
            {
                return;
            }

            var snapshot = JsonSerializer.Deserialize<LauncherSettings>(File.ReadAllText(_settingsPath), ReadOptions);
            if (snapshot is null)
            {
                return;
            }

            var migratedApiEndpoint = !string.IsNullOrWhiteSpace(snapshot.ApiBaseUrl)
                && IsLegacyApiBaseUrl(snapshot.ApiBaseUrl);
            LaunchOnStartup = snapshot.LaunchOnStartup;
            MinimizeOnClose = snapshot.MinimizeOnClose;
            ReducedMotion = snapshot.ReducedMotion;
            ConcurrentDownloads = Math.Clamp(snapshot.ConcurrentDownloads, 1, 32);
            CacheSizeGigabytes = (int)Math.Clamp(
                Math.Round(snapshot.CacheSizeBytes / (double)(1024L * 1024L * 1024L)),
                1,
                256);
            ThemePreset = ThemeOptions.Contains(snapshot.ThemePreset, StringComparer.OrdinalIgnoreCase)
                ? ThemeOptions.First(option => option.Equals(snapshot.ThemePreset, StringComparison.OrdinalIgnoreCase))
                : "Slate";
            AccentColor = string.IsNullOrWhiteSpace(snapshot.AccentColor) ? "#1A9FFF" : snapshot.AccentColor;
            CompactMode = snapshot.CompactMode;
            DownloadDirectory = string.IsNullOrWhiteSpace(snapshot.DownloadDirectory)
                ? @"C:\Games\Launcher\Downloads"
                : snapshot.DownloadDirectory;
            InstallDirectory = string.IsNullOrWhiteSpace(snapshot.DefaultGameDirectory)
                ? @"C:\Games"
                : snapshot.DefaultGameDirectory;
            ApiBaseUrl = string.IsNullOrWhiteSpace(snapshot.ApiBaseUrl)
                ? DefaultApiBaseUrl
                : IsLegacyApiBaseUrl(snapshot.ApiBaseUrl)
                    ? DefaultApiBaseUrl
                    : snapshot.ApiBaseUrl;
            ProfileImagePath = snapshot.ProfileImagePath ?? string.Empty;
            _trustedManifestKeysPem = snapshot.TrustedManifestKeysPem;
            _requireTrustedManifestKeys = snapshot.RequireTrustedManifestKeys;

            if (migratedApiEndpoint)
            {
                Save();
            }
        }
        catch (JsonException)
        {
            SaveStatus = "Using default settings.";
        }
        catch (IOException)
        {
            SaveStatus = "Using default settings.";
        }
    }

    partial void OnProfileImagePathChanged(string value)
    {
        ProfileImage?.Dispose();
        ProfileImage = null;
        if (string.IsNullOrWhiteSpace(value) || !File.Exists(value))
        {
            return;
        }

        try
        {
            ProfileImage = new Bitmap(value);
        }
        catch (ArgumentException)
        {
            SaveStatus = "That profile picture could not be loaded.";
        }
        catch (IOException)
        {
            SaveStatus = "That profile picture could not be loaded.";
        }
    }

    partial void OnSelectedSectionChanged(string value)
    {
        OnPropertyChanged(nameof(IsProfileSelected));
        OnPropertyChanged(nameof(IsGeneralSelected));
        OnPropertyChanged(nameof(IsDownloadsSelected));
        OnPropertyChanged(nameof(IsAppearanceSelected));
        OnPropertyChanged(nameof(IsAdvancedSelected));
    }

    partial void OnThemePresetChanged(string value) => ApplyAppearance();

    partial void OnAccentColorChanged(string value) => ApplyAppearance();

    partial void OnCompactModeChanged(bool value) => ApplyAppearance();

    partial void OnProfileImageChanged(Bitmap? value)
    {
        OnPropertyChanged(nameof(HasProfileImage));
        OnPropertyChanged(nameof(ShowDefaultProfile));
    }

    private static bool IsLegacyApiBaseUrl(string value)
    {
        var normalized = value.Trim().TrimEnd('/');
        return normalized.Equals(LegacyLocalApiBaseUrl, StringComparison.OrdinalIgnoreCase)
            || normalized.Equals(LegacyMantleIpApiBaseUrl, StringComparison.OrdinalIgnoreCase)
            || normalized.Equals("https://5.231.32.191", StringComparison.OrdinalIgnoreCase);
    }

    private void ApplyAppearance()
    {
        if (Application.Current is not { } application)
        {
            return;
        }

        var palette = ThemePreset switch
        {
            "Midnight" => ThemePalette.Midnight,
            "Graphite" => ThemePalette.Graphite,
            _ => ThemePalette.Slate
        };
        var resources = application.Resources;
        resources["BackgroundPrimaryColor"] = Color.Parse(palette.Background);
        resources["ChromeBarColor"] = Color.Parse(palette.ChromeBar);
        resources["SidebarBackgroundColor"] = Color.Parse(palette.Sidebar);
        resources["SurfacePrimaryColor"] = Color.Parse(palette.Surface);
        resources["SurfaceElevatedColor"] = Color.Parse(palette.Elevated);
        resources["BorderSubtleColor"] = Color.Parse(palette.Border);
        resources["TextPrimaryColor"] = Color.Parse(palette.TextPrimary);
        resources["TextSecondaryColor"] = Color.Parse(palette.TextSecondary);
        resources["TextMutedColor"] = Color.Parse(palette.TextMuted);
        resources["ArtworkPrimaryColor"] = Color.Parse(palette.ArtworkPrimary);
        resources["ArtworkSecondaryColor"] = Color.Parse(palette.ArtworkSecondary);
        resources["ArtworkTertiaryColor"] = Color.Parse(palette.ArtworkTertiary);
        resources["DownloadChartColor"] = Color.Parse(palette.DownloadChart);
        resources["DownloadPanelColor"] = Color.Parse(palette.DownloadPanel);

        var accent = ParseColorOrDefault(AccentColor, Color.Parse(palette.Accent));
        resources["AccentPrimaryColor"] = accent;
        resources["AccentSoftColor"] = Color.FromArgb(64, accent.R, accent.G, accent.B);
        resources["SidebarWidth"] = CompactMode ? 220d : 244d;
        resources["NavButtonPadding"] = CompactMode ? new Thickness(10, 8) : new Thickness(12, 11);
        resources["HomeButtonPadding"] = CompactMode ? new Thickness(8, 6) : new Thickness(10, 7);
        resources["GamesHeaderPadding"] = CompactMode ? new Thickness(8, 5) : new Thickness(10, 7);
        resources["SidebarGamePadding"] = CompactMode ? new Thickness(4, 2) : new Thickness(5, 4);
    }

    private static Color ParseColorOrDefault(string value, Color fallback)
    {
        if (Color.TryParse((value ?? string.Empty).AsSpan(), out var color))
        {
            return color;
        }

        return fallback;
    }

    private sealed record ThemePalette(
        string Background,
        string ChromeBar,
        string Sidebar,
        string Surface,
        string Elevated,
        string Border,
        string TextPrimary,
        string TextSecondary,
        string TextMuted,
        string Accent,
        string ArtworkPrimary,
        string ArtworkSecondary,
        string ArtworkTertiary,
        string DownloadChart,
        string DownloadPanel)
    {
        public static ThemePalette Slate { get; } = new(
            "#2A2F3A", "#171D25", "#2A2F3A", "#2A2F3A", "#3A4352", "#3D4A5A",
            "#D6D7D8", "#A4B3C2", "#6D7886", "#1A9FFF", "#1D4D78", "#4B3D5E", "#64483D", "#0E141B", "#2A2F3A");

        public static ThemePalette Midnight { get; } = new(
            "#171A21", "#101318", "#202631", "#20242D", "#303746", "#384252",
            "#EEF1F5", "#B7C1CF", "#7C8899", "#5EA7FF", "#214C76", "#4C4267", "#6A4D42", "#0B1017", "#20242D");

        public static ThemePalette Graphite { get; } = new(
            "#292929", "#1C1C1C", "#303030", "#292929", "#414141", "#4C4C4C",
            "#F0F0F0", "#C3C3C3", "#8F8F8F", "#D59B55", "#63513E", "#574E66", "#6C4D40", "#111111", "#292929");
    }

}
