using Avalonia.Controls;
using Avalonia.Interactivity;
using Launcher.App.ViewModels;

namespace Launcher.App.Views;

public partial class LibraryView : UserControl
{
    public LibraryView() => InitializeComponent();

    private void OnGameContextMenuOpened(object? sender, RoutedEventArgs e)
    {
        if (sender is not ContextMenu menu
            || menu.PlacementTarget?.DataContext is not GameTile game
            || TopLevel.GetTopLevel(this) is not MainWindow window
            || window.DataContext is not ShellViewModel viewModel)
        {
            return;
        }

        menu.Items.Clear();
        var removeFromLibrary = new MenuItem { Header = "Remove from library" };
        removeFromLibrary.Click += async (_, _) => await viewModel.RemoveGameFromLibraryAsync(game);
        menu.Items.Add(removeFromLibrary);
    }
}
