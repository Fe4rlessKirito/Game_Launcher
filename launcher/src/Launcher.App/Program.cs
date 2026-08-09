using Avalonia;

namespace Launcher.App;

internal static class Program
{
    [STAThread]
    public static void Main(string[] args)
    {
        if (!SingleInstanceCoordinator.TryAcquire("Launcher.Platform", out var coordinator)) return;
        using (coordinator)
        {
            App.InstanceCoordinator = coordinator;
            BuildAvaloniaApp().StartWithClassicDesktopLifetime(args);
        }
    }

    public static AppBuilder BuildAvaloniaApp() => AppBuilder.Configure<App>().UsePlatformDetect().WithInterFont().LogToTrace();
}
