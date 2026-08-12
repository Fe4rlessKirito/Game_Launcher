using System;
using System.Linq;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Threading;

namespace Launcher.App;

public partial class MainWindow : Window
{
    private readonly DispatcherTimer _dragSnapTimer;
    private PixelPoint _dragStartPosition;
    private bool _dragMoved;
    private int _dragSnapTicks;

    public MainWindow()
    {
        InitializeComponent();
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

    private void OnGameContextMenuOpened(object? sender, Avalonia.Interactivity.RoutedEventArgs e)
    {
        if (sender is not ContextMenu menu ||
            DataContext is not ViewModels.ShellViewModel viewModel ||
            menu.PlacementTarget?.DataContext is not ViewModels.SidebarGame game)
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
