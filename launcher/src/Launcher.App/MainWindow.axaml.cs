using System;
using System.Linq;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Interactivity;
using Avalonia.Threading;

namespace Launcher.App;

public partial class MainWindow : Window
{
    private const string GameDragDataPrefix = "vaultnode-game:";
    private readonly DispatcherTimer _dragSnapTimer;
    private PixelPoint _dragStartPosition;
    private bool _dragMoved;
    private int _dragSnapTicks;
    private Point _gameDragStartPoint;
    private PointerPressedEventArgs? _gameDragPointerPressed;
    private ViewModels.SidebarGame? _draggedGame;
    private ViewModels.SidebarCategory? _activeDropCategory;

    public MainWindow()
    {
        InitializeComponent();
        AddHandler(InputElement.PointerPressedEvent, OnSidebarGamePointerPressed, RoutingStrategies.Bubble, handledEventsToo: true);
        AddHandler(InputElement.PointerMovedEvent, OnSidebarGamePointerMoved, RoutingStrategies.Bubble, handledEventsToo: true);
        AddHandler(InputElement.PointerReleasedEvent, OnSidebarGamePointerReleased, RoutingStrategies.Bubble, handledEventsToo: true);
        _dragSnapTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(50) };
        _dragSnapTimer.Tick += OnDragSnapTick;
    }

    private void OnTitleBarPointerPressed(object? sender, PointerPressedEventArgs e)
    {
        if (e.Source is Button || e.Handled)
        {
            return;
        }

        if (e.ClickCount == 2)
        {
            ToggleWindowState();
            e.Handled = true;
            return;
        }

        if (e.GetCurrentPoint(this).Properties.PointerUpdateKind == PointerUpdateKind.LeftButtonPressed)
        {
            _dragStartPosition = Position;
            _dragMoved = false;
            _dragSnapTicks = 0;
            _dragSnapTimer.Start();

            BeginMoveDrag(e);
            TryMaximizeAtTopEdge();
        }
    }

    private void OnMinimizeClick(object? sender, Avalonia.Interactivity.RoutedEventArgs e) => WindowState = WindowState.Minimized;

    private void OnCategoryInputKeyDown(object? sender, KeyEventArgs e)
    {
        if (DataContext is not ViewModels.ShellViewModel viewModel)
        {
            return;
        }

        if (e.Key == Key.Enter)
        {
            viewModel.CommitCategoryCommand.Execute(null);
            e.Handled = true;
        }
        else if (e.Key == Key.Escape)
        {
            viewModel.CancelCategoryCommand.Execute(null);
            e.Handled = true;
        }
    }

    private void OnAddGameInputKeyDown(object? sender, KeyEventArgs e)
    {
        if (DataContext is not ViewModels.ShellViewModel viewModel)
        {
            return;
        }

        if (e.Key == Key.Enter)
        {
            viewModel.CommitAddGameCommand.Execute(null);
            e.Handled = true;
        }
        else if (e.Key == Key.Escape)
        {
            viewModel.CancelAddGameCommand.Execute(null);
            e.Handled = true;
        }
    }

    private void OnCategoryToggleClick(object? sender, RoutedEventArgs e)
    {
        if (sender is Button { DataContext: ViewModels.SidebarCategory category }
            && DataContext is ViewModels.ShellViewModel viewModel)
        {
            viewModel.ToggleCategoryCommand.Execute(category);
            e.Handled = true;
        }
    }

    private void OnSidebarGamePointerPressed(object? sender, PointerPressedEventArgs e)
    {
        var source = FindSidebarGameControl(e.Source);
        if (source?.DataContext is not ViewModels.SidebarGame game
            || e.Properties.PointerUpdateKind != PointerUpdateKind.LeftButtonPressed)
        {
            return;
        }

        _draggedGame = game;
        _gameDragStartPoint = e.GetPosition(this);
        _gameDragPointerPressed = e;
        e.Pointer.Capture(source);
    }

    private async void OnSidebarGamePointerMoved(object? sender, PointerEventArgs e)
    {
        if (_draggedGame is null
            || _gameDragPointerPressed is null
            || !e.GetCurrentPoint(this).Properties.IsLeftButtonPressed)
        {
            return;
        }

        var position = e.GetPosition(this);
        if (Math.Abs(position.X - _gameDragStartPoint.X) < 6
            && Math.Abs(position.Y - _gameDragStartPoint.Y) < 6)
        {
            return;
        }

        var game = _draggedGame;
        var pointerPressed = _gameDragPointerPressed;
        _draggedGame = null;
        _gameDragPointerPressed = null;
        e.Pointer.Capture(null);
        e.Handled = true;

        try
        {
            var transfer = new DataTransfer();
            transfer.Add(DataTransferItem.CreateText(GameDragDataPrefix + game.OpenKey));
            await DragDrop.DoDragDropAsync(pointerPressed, transfer, DragDropEffects.Move);
        }
        finally
        {
            SetActiveDropCategory(null);
        }
    }

    private void OnSidebarGamePointerReleased(object? sender, PointerReleasedEventArgs e)
    {
        e.Pointer.Capture(null);
        _draggedGame = null;
        _gameDragPointerPressed = null;
    }

    private static Control? FindSidebarGameControl(object? source)
    {
        for (var control = source as Control; control is not null; control = control.Parent as Control)
        {
            if (control.DataContext is ViewModels.SidebarGame)
            {
                return control;
            }
        }

        return null;
    }

    private void OnCategoryDragOver(object? sender, DragEventArgs e)
    {
        var category = (sender as Control)?.DataContext as ViewModels.SidebarCategory;
        var viewModel = DataContext as ViewModels.ShellViewModel;
        var canDrop = category is not null
            && viewModel is not null
            && TryGetDraggedGameId(e.DataTransfer, out var gameId)
            && viewModel.CanMoveGameToCategory(gameId, category);
        e.DragEffects = canDrop ? DragDropEffects.Move : DragDropEffects.None;
        SetActiveDropCategory(canDrop ? category : null);
        e.Handled = true;
    }

    private void OnCategoryDragLeave(object? sender, DragEventArgs e)
    {
        if (sender is Control { DataContext: ViewModels.SidebarCategory category }
            && ReferenceEquals(_activeDropCategory, category))
        {
            SetActiveDropCategory(null);
        }
    }

    private void OnCategoryDrop(object? sender, DragEventArgs e)
    {
        var category = (sender as Control)?.DataContext as ViewModels.SidebarCategory;
        var viewModel = DataContext as ViewModels.ShellViewModel;
        var moved = category is not null
            && viewModel is not null
            && TryGetDraggedGameId(e.DataTransfer, out var gameId)
            && viewModel.MoveGameToCategory(gameId, category);
        e.DragEffects = moved ? DragDropEffects.Move : DragDropEffects.None;
        e.Handled = true;
        SetActiveDropCategory(null);
    }

    private static bool TryGetDraggedGameId(IDataTransfer dataTransfer, out string gameId)
    {
        var text = dataTransfer.TryGetText();
        if (text is not null && text.StartsWith(GameDragDataPrefix, StringComparison.Ordinal)
            && text.Length > GameDragDataPrefix.Length)
        {
            gameId = text[GameDragDataPrefix.Length..];
            return true;
        }

        gameId = string.Empty;
        return false;
    }

    private void SetActiveDropCategory(ViewModels.SidebarCategory? category)
    {
        if (ReferenceEquals(_activeDropCategory, category))
        {
            return;
        }

        if (_activeDropCategory is not null)
        {
            _activeDropCategory.IsDropTarget = false;
        }

        _activeDropCategory = category;
        if (_activeDropCategory is not null)
        {
            _activeDropCategory.IsDropTarget = true;
        }
    }

    private void OnGameContextMenuOpened(object? sender, Avalonia.Interactivity.RoutedEventArgs e)
    {
        if (sender is not ContextMenu menu || DataContext is not ViewModels.ShellViewModel viewModel)
        {
            return;
        }

        var sidebarGame = menu.PlacementTarget?.DataContext as ViewModels.SidebarGame;
        var tileGame = menu.PlacementTarget?.DataContext as ViewModels.GameTile;
        var game = sidebarGame ?? (tileGame is null
            ? null
            : new ViewModels.SidebarGame(tileGame.Title, tileGame.Monogram, tileGame.Status, 0, tileGame.GameId, ArtworkSource: tileGame.ArtworkSource, IsSteamGame: tileGame.IsSteamGame));
        if (game is null)
        {
            return;
        }

        menu.Items.Clear();
        foreach (var category in viewModel.SidebarCategories.Where(category => category.IsUserCreated))
        {
            var item = new MenuItem { Header = $"Add to {category.Name}" };
            item.Click += (_, _) => viewModel.AddGameToCategory(game, category);
            menu.Items.Add(item);
        }

        if (menu.Items.Count > 0)
        {
            menu.Items.Add(new Separator());
        }

        var removeFromLibrary = new MenuItem { Header = "Remove from library" };
        removeFromLibrary.Click += async (_, _) =>
        {
            if (tileGame is not null)
            {
                await viewModel.RemoveGameFromLibraryAsync(tileGame);
            }
            else
            {
                await viewModel.RemoveGameFromLibraryAsync(game);
            }
        };
        menu.Items.Add(removeFromLibrary);

        menu.Items.Add(new Separator());

        var createCategory = new MenuItem { Header = "+ New category..." };
        createCategory.Click += (_, _) => viewModel.BeginAddCategoryCommand.Execute(null);
        menu.Items.Add(createCategory);
    }

    private void OnMaximizeClick(object? sender, Avalonia.Interactivity.RoutedEventArgs e) => ToggleWindowState();

    private void OnCloseClick(object? sender, Avalonia.Interactivity.RoutedEventArgs e) => Close();

    private void ToggleWindowState() => WindowState = WindowState == WindowState.Maximized ? WindowState.Normal : WindowState.Maximized;

    private void OnDragSnapTick(object? sender, EventArgs e)
    {
        _dragSnapTicks++;
        if (_dragSnapTicks > 60 || WindowState == WindowState.Maximized)
        {
            _dragSnapTimer.Stop();
            return;
        }

        if (Position != _dragStartPosition)
        {
            _dragMoved = true;
        }

        TryMaximizeAtTopEdge();
    }

    private void TryMaximizeAtTopEdge()
    {
        if (!_dragMoved)
        {
            return;
        }

        var screen = Screens.ScreenFromWindow(this);
        if (screen is not null && Position.Y <= screen.WorkingArea.Y + 2)
        {
            _dragSnapTimer.Stop();
            WindowState = WindowState.Maximized;
        }
    }
}
