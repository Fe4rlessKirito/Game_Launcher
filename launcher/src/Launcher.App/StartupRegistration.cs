using Microsoft.Win32;

namespace Launcher.App;

internal static class StartupRegistration
{
    private const string RunKeyPath = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    private const string ValueName = "Vaultnode";

    public static bool TrySetEnabled(bool enabled, out string? error)
    {
        error = null;
        if (!OperatingSystem.IsWindows())
        {
            return true;
        }

        try
        {
            using var key = Registry.CurrentUser.CreateSubKey(RunKeyPath, writable: true);
            if (key is null)
            {
                error = "Windows did not allow access to the startup registry key.";
                return false;
            }

            if (!enabled)
            {
                key.DeleteValue(ValueName, throwOnMissingValue: false);
                return true;
            }

            var processPath = Environment.ProcessPath;
            if (string.IsNullOrWhiteSpace(processPath))
            {
                error = "The launcher executable path could not be determined.";
                return false;
            }

            var escapedPath = processPath.Replace("\"", "\\\"", StringComparison.Ordinal);
            key.SetValue(ValueName, $"\"{escapedPath}\"");
            return true;
        }
        catch (Exception exception) when (exception is UnauthorizedAccessException or IOException or System.Security.SecurityException)
        {
            error = exception.Message;
            return false;
        }
    }
}
