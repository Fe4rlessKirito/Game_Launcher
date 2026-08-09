using CommunityToolkit.Mvvm.ComponentModel;
using Launcher.Core;

namespace Launcher.App.ViewModels;

public partial class LibraryViewModel : ObservableObject
{
    public IReadOnlyList<GameTile> Games { get; } =
    [
        new("Synthetic Game", "Installed", "1.0.0", "SG", GameState.Launchable),
        new("Build Playground", "Ready to install", "0.4.2", "BP", GameState.NotInstalled)
    ];
}

public sealed record GameTile(string Title, string Status, string Version, string Monogram, GameState State);
