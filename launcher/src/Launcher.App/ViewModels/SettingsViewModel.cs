using System.Text.Json;
using Avalonia;
using Avalonia.Media;
using Avalonia.Media.Imaging;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using Launcher.App.Runtime;
using Launcher.Core;
using Launcher.Networking;
using Launcher.App;

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

    private Color _accentColorValue = Color.Parse("#1A9FFF");

    [ObservableProperty]
    private string _accountEmail = string.Empty;

    [ObservableProperty]
    private string _accountPassword = string.Empty;

    [ObservableProperty]
    private string _accountUsername = string.Empty;

    [ObservableProperty]
    private string _authStatus = "Not signed in.";

    [ObservableProperty]
    private bool _isSignedIn;

    [ObservableProperty]
    private bool _isSigningIn;

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

    [ObservableProperty]
    private string _steamId64 = string.Empty;

    [ObservableProperty]
    private string _steamPersonaName = string.Empty;

    [ObservableProperty]
    private bool _isSteamConnecting;

    [ObservableProperty]
    private string _steamConnectionStatus = "Not connected.";

    private SteamLibrarySnapshot _steamSnapshot = SteamLibrarySnapshot.Empty;
    private IReadOnlyList<SteamOwnedGame> _cachedSteamOwnedGames = [];

    public bool HasProfileImage => ProfileImage is not null;
    public bool ShowDefaultProfile => !HasProfileImage;
    public bool IsProfileSelected => SelectedSection == "Profile";
    public bool IsGeneralSelected => SelectedSection == "General";
    public bool IsDownloadsSelected => SelectedSection == "Downloads";
    public bool IsAppearanceSelected => SelectedSection == "Appearance";
    public bool IsAdvancedSelected => SelectedSection == "Advanced";
    public bool IsSteamSelected => SelectedSection == "Steam";
    public bool IsNotSignedIn => !IsSignedIn;
    public bool CanSubmitAuth => !IsSigningIn;
    public bool HasSteamAccount => !string.IsNullOrWhiteSpace(SteamId64);
    public bool CanConnectSteam => !IsSteamConnecting;
    public string SteamConnectButtonLabel => HasSteamAccount ? "Sync Steam library" : "Connect Steam account";
    public IReadOnlyList<SteamGameInstall> SteamGames => _steamSnapshot.Games;
    public IReadOnlyList<SteamOwnedGame> SteamOwnedGames => _cachedSteamOwnedGames;
    public IReadOnlyList<string> SteamLibraryRoots => _steamSnapshot.LibraryRoots;
    public bool HasSteamGames => SteamGames.Count > 0;
    public bool HasSteamOwnedGames => SteamOwnedGames.Count > 0;
    public string SteamStatus => HasSteamAccount
        ? _steamSnapshot.OwnedGamesError
            ?? $"Connected · {SteamOwnedGames.Count} owned, {SteamGames.Count} installed"
        : _steamSnapshot.Error
            ?? (!_steamSnapshot.IsDetected
                ? "Steam was not found on this PC."
                : SteamGames.Count == 0
                    ? "Steam detected · No installed games found."
                    : $"{SteamGames.Count} installed Steam game{(SteamGames.Count == 1 ? string.Empty : "s")} detected.");
    public string SteamLibrarySummary => HasSteamAccount
        ? SteamLibraryRoots.Count == 0
            ? $"Account {SteamId64} connected · local Steam install scan unavailable"
            : $"{SteamLibraryRoots.Count} local location{(SteamLibraryRoots.Count == 1 ? string.Empty : "s")} · account {SteamId64}"
        : SteamLibraryRoots.Count == 0
            ? "Steam library locations will appear here when Steam is installed."
            : $"{SteamLibraryRoots.Count} Steam library location{(SteamLibraryRoots.Count == 1 ? string.Empty : "s")} detected.";
    public Color AccentColorValue
    {
        get => _accentColorValue;
        set
        {
            if (_accentColorValue == value)
            {
                return;
            }

            _accentColorValue = value;
            AccentColor = ToHex(value);
            OnPropertyChanged();
        }
    }
    public IReadOnlyList<string> ThemeOptions { get; } = ["Slate", "Midnight", "Graphite"];

    private LauncherRuntime? _runtime;
    private IReadOnlyDictionary<string, string>? _trustedManifestKeysPem;
    private bool _requireTrustedManifestKeys;

    public SettingsViewModel(string? settingsPath = null)
    {
        _settingsPath = string.IsNullOrWhiteSpace(settingsPath) ? DefaultSettingsPath : settingsPath;
        Load();
    }

    public void AttachRuntime(LauncherRuntime runtime)
    {
        _runtime = runtime;
        ApplySteamSnapshot(runtime.Snapshot.Steam ?? SteamLibrarySnapshot.Empty);
    }

    public void ApplySteamSnapshot(SteamLibrarySnapshot snapshot)
    {
        _steamSnapshot = snapshot;
        if (snapshot.ConnectedAccount is { } account)
        {
            SteamId64 = account.SteamId64;
            SteamPersonaName = account.PersonaName ?? string.Empty;
            SteamConnectionStatus = string.IsNullOrWhiteSpace(account.PersonaName)
                ? $"Connected · {account.SteamId64}"
                : $"Connected as {account.PersonaName}";
        }
        else if (string.IsNullOrWhiteSpace(SteamId64))
        {
            SteamConnectionStatus = "Not connected.";
        }

        _cachedSteamOwnedGames = snapshot.OwnedGames ?? [];
        OnPropertyChanged(nameof(SteamGames));
        OnPropertyChanged(nameof(SteamOwnedGames));
        OnPropertyChanged(nameof(SteamLibraryRoots));
        OnPropertyChanged(nameof(HasSteamGames));
        OnPropertyChanged(nameof(HasSteamOwnedGames));
        OnPropertyChanged(nameof(HasSteamAccount));
        OnPropertyChanged(nameof(SteamConnectButtonLabel));
        OnPropertyChanged(nameof(SteamStatus));
        OnPropertyChanged(nameof(SteamLibrarySummary));
    }

    public void ApplyUser(LauncherUserProfile? user)
    {
        IsSignedIn = user is not null;
        AccountUsername = user?.Username ?? string.Empty;
        AccountEmail = user?.Email ?? string.Empty;
        if (user is null)
        {
            AuthStatus = "Not signed in.";
        }
    }

    [RelayCommand]
    private async Task SignIn()
    {
        if (_runtime is null)
        {
            AuthStatus = "The launcher is still connecting. Try again in a moment.";
            return;
        }

        if (string.IsNullOrWhiteSpace(AccountEmail) || !AccountEmail.Contains('@', StringComparison.Ordinal)
            || string.IsNullOrWhiteSpace(AccountPassword) || AccountPassword.Length < 8)
        {
            AuthStatus = "Enter a valid email and an 8-character password.";
            return;
        }

        IsSigningIn = true;
        AuthStatus = "Signing in…";
        try
        {
            var user = await _runtime.SignInAsync(AccountEmail.Trim(), AccountPassword);
            AccountUsername = user.Username ?? string.Empty;
            AccountEmail = user.Email ?? AccountEmail.Trim();
            AccountPassword = string.Empty;
            AuthStatus = "Signed in. Your library is connected.";
        }
        catch (Exception error) when (error is HttpRequestException or InvalidOperationException or TaskCanceledException)
        {
            AuthStatus = error.Message;
        }
        finally
        {
            IsSigningIn = false;
        }
    }

    [RelayCommand]
    private async Task SignOut()
    {
        if (_runtime is null)
        {
            ApplyUser(null);
            return;
        }

        IsSigningIn = true;
        AuthStatus = "Signing out…";
        try
        {
            await _runtime.SignOutAsync();
            ApplyUser(null);
        }
        catch (Exception error) when (error is HttpRequestException or TaskCanceledException)
        {
            AuthStatus = error.Message;
        }
        finally
        {
            IsSigningIn = false;
        }
    }

    [RelayCommand]
    private async Task ConnectSteam()
    {
        if (_runtime is null)
        {
            SteamConnectionStatus = "The launcher is still connecting. Try again in a moment.";
            return;
        }

        IsSteamConnecting = true;
        SteamConnectionStatus = "Waiting for Steam sign-in…";
        try
        {
            var library = await _runtime.ConnectSteamAsync().ConfigureAwait(true);
            SteamId64 = library.SteamId64;
            SteamPersonaName = library.PersonaName ?? string.Empty;
            _cachedSteamOwnedGames = library.Games;
            ApplySteamSnapshot(_runtime.Snapshot.Steam ?? SteamLibrarySnapshot.Empty);
            Save();
            SteamConnectionStatus = string.IsNullOrWhiteSpace(SteamPersonaName)
                ? $"Connected · {SteamId64}"
                : $"Connected as {SteamPersonaName}";
        }
        catch (Exception error) when (error is HttpRequestException or InvalidOperationException or InvalidDataException or IOException or LauncherOperationException or TaskCanceledException)
        {
            SteamConnectionStatus = error.Message;
        }
        finally
        {
            IsSteamConnecting = false;
        }
    }

    [RelayCommand]
    private async Task DisconnectSteam()
    {
        if (_runtime is null)
        {
            SteamId64 = string.Empty;
            SteamPersonaName = string.Empty;
            _cachedSteamOwnedGames = [];
            SteamConnectionStatus = "Not connected.";
            Save();
            return;
        }

        IsSteamConnecting = true;
        SteamConnectionStatus = "Disconnecting Steam…";
        try
        {
            await _runtime.DisconnectSteamAsync().ConfigureAwait(true);
            SteamId64 = string.Empty;
            SteamPersonaName = string.Empty;
            _cachedSteamOwnedGames = [];
            ApplySteamSnapshot(_runtime.Snapshot.Steam ?? SteamLibrarySnapshot.Empty);
            Save();
            SteamConnectionStatus = "Not connected.";
        }
        catch (Exception error) when (error is IOException or HttpRequestException or InvalidDataException or TaskCanceledException)
        {
            SteamConnectionStatus = error.Message;
        }
        finally
        {
            IsSteamConnecting = false;
        }
    }

    [RelayCommand]
    private async Task SaveUsername()
    {
        if (_runtime is null || !IsSignedIn)
        {
            AuthStatus = "Sign in before changing your username.";
            return;
        }

        var username = AccountUsername.Trim();
        if (!IsValidUsername(username))
        {
            AuthStatus = "Use 3–24 letters, numbers, or underscores.";
            return;
        }

        IsSigningIn = true;
        AuthStatus = "Saving username…";
        try
        {
            var user = await _runtime.UpdateUsernameAsync(username);
            ApplyUser(user);
            AuthStatus = "Username saved.";
        }
        catch (Exception error) when (error is HttpRequestException or InvalidOperationException or TaskCanceledException)
        {
            AuthStatus = error.Message;
        }
        finally
        {
            IsSigningIn = false;
        }
    }

    [RelayCommand]
    private void Save()
    {
        ConcurrentDownloads = Math.Clamp(ConcurrentDownloads, 1, 32);
        CacheSizeGigabytes = Math.Clamp(CacheSizeGigabytes, 1, 256);
        DownloadDirectory = string.IsNullOrWhiteSpace(DownloadDirectory)
            ? @"C:\Games\Launcher\Downloads"
            : DownloadDirectory.Trim();
        InstallDirectory = string.IsNullOrWhiteSpace(InstallDirectory)
            ? @"C:\Games"
            : InstallDirectory.Trim();
        ApiBaseUrl = string.IsNullOrWhiteSpace(ApiBaseUrl)
            ? DefaultApiBaseUrl
            : ApiBaseUrl.Trim().TrimEnd('/');

        if (!StartupRegistration.TrySetEnabled(LaunchOnStartup, out var startupError))
        {
            SaveStatus = $"Could not update startup setting: {startupError}";
            return;
        }

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
                CompactMode: CompactMode,
                SteamId64: SteamId64,
                SteamPersonaName: SteamPersonaName,
                SteamOwnedGames: _cachedSteamOwnedGames);
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
        SteamId64 = string.Empty;
        SteamPersonaName = string.Empty;
        _cachedSteamOwnedGames = [];
        SteamConnectionStatus = "Not connected.";
        _trustedManifestKeysPem = null;
        _requireTrustedManifestKeys = false;
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
            "Steam" => "Steam",
            _ => "Profile"
        };
    }

    [RelayCommand]
    private async Task RefreshSteam()
    {
        if (_runtime is null)
        {
            ApplySteamSnapshot(SteamLibraryDiscovery.Discover());
            return;
        }

        try
        {
            var snapshot = await _runtime.RefreshAsync().ConfigureAwait(true);
            ApplySteamSnapshot(snapshot.Steam ?? SteamLibrarySnapshot.Empty);
        }
        catch (Exception error) when (error is IOException or HttpRequestException or InvalidDataException or TaskCanceledException)
        {
            ApplySteamSnapshot(new SteamLibrarySnapshot([], [], error.Message));
        }
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
            SteamId64 = SteamLibraryDiscovery.IsValidSteamId64(snapshot.SteamId64) ? snapshot.SteamId64 : string.Empty;
            SteamPersonaName = string.IsNullOrWhiteSpace(snapshot.SteamPersonaName) ? string.Empty : snapshot.SteamPersonaName;
            _cachedSteamOwnedGames = snapshot.SteamOwnedGames ?? [];
            SteamConnectionStatus = string.IsNullOrWhiteSpace(SteamId64)
                ? "Not connected."
                : string.IsNullOrWhiteSpace(SteamPersonaName)
                    ? $"Connected · {SteamId64}"
                    : $"Connected as {SteamPersonaName}";
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
        catch (UnauthorizedAccessException)
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
        catch (UnauthorizedAccessException)
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
        OnPropertyChanged(nameof(IsSteamSelected));
    }

    partial void OnThemePresetChanged(string value) => ApplyAppearance();

    partial void OnAccentColorChanged(string value)
    {
        var parsed = ParseColorOrDefault(value, Color.Parse("#1A9FFF"));
        if (_accentColorValue != parsed)
        {
            _accentColorValue = parsed;
            OnPropertyChanged(nameof(AccentColorValue));
        }

        ApplyAppearance();
    }

    partial void OnCompactModeChanged(bool value) => ApplyAppearance();

    partial void OnIsSignedInChanged(bool value) => OnPropertyChanged(nameof(IsNotSignedIn));

    partial void OnIsSigningInChanged(bool value) => OnPropertyChanged(nameof(CanSubmitAuth));

    partial void OnIsSteamConnectingChanged(bool value)
    {
        OnPropertyChanged(nameof(CanConnectSteam));
    }

    partial void OnSteamId64Changed(string value)
    {
        OnPropertyChanged(nameof(HasSteamAccount));
        OnPropertyChanged(nameof(SteamConnectButtonLabel));
        OnPropertyChanged(nameof(SteamStatus));
        OnPropertyChanged(nameof(SteamLibrarySummary));
    }

    partial void OnSteamPersonaNameChanged(string value)
    {
        if (HasSteamAccount)
        {
            SteamConnectionStatus = string.IsNullOrWhiteSpace(value)
                ? $"Connected · {SteamId64}"
                : $"Connected as {value}";
        }
    }

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
        // Fluent controls (including ToggleSwitch) use the theme's system
        // accent resources rather than the launcher-specific accent brush.
        // Keep both resource families synchronized so checked controls follow
        // the color selected in Appearance immediately.
        resources["SystemAccentColor"] = accent;
        resources["SystemAccentColorLight1"] = ScaleColor(accent, 1.15);
        resources["SystemAccentColorLight2"] = ScaleColor(accent, 1.30);
        resources["SystemAccentColorLight3"] = ScaleColor(accent, 1.45);
        resources["SystemAccentColorDark1"] = ScaleColor(accent, 0.70);
        resources["SystemAccentColorDark2"] = ScaleColor(accent, 0.50);
        resources["SystemAccentColorDark3"] = ScaleColor(accent, 0.30);
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

    private static Color ScaleColor(Color color, double factor)
    {
        return Color.FromArgb(
            color.A,
            ScaleChannel(color.R, factor),
            ScaleChannel(color.G, factor),
            ScaleChannel(color.B, factor));
    }

    private static byte ScaleChannel(byte channel, double factor)
    {
        return (byte)Math.Clamp((int)Math.Round(channel * factor), 0, byte.MaxValue);
    }

    private static string ToHex(Color color) => $"#{color.R:X2}{color.G:X2}{color.B:X2}";

    private static bool IsValidUsername(string value)
    {
        return value.Length is >= 3 and <= 24
            && value.All(character => character is >= 'A' and <= 'Z'
                or >= 'a' and <= 'z'
                or >= '0' and <= '9'
                or '_');
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
