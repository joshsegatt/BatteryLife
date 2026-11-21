using System.Runtime.InteropServices;
using BatteryLife.Logic;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Animation;
using Windows.Graphics;

namespace BatteryLife.App;

/// <summary>
/// The hub panel that opens from the tray icon, above the taskbar like Quick Settings. It hides as
/// soon as it loses focus.
/// </summary>
public sealed partial class FlyoutWindow : Window
{
    private const int WidthDip = 380;
    private const int MarginDip = 12;

    private readonly Text _text;
    private readonly Action _refresh;
    private readonly Action _exit;
    private string _shownHeadline = "";
    private DateTime _hiddenAt = DateTime.MinValue;

    public FlyoutWindow(Text text, Action refresh, Action exit)
    {
        _text = text;
        _refresh = refresh;
        _exit = exit;
        InitializeComponent();

        var presenter = OverlappedPresenter.CreateForContextMenu();
        AppWindow.SetPresenter(presenter);
        AppWindow.IsShownInSwitchers = false;
        AppWindow.Title = "BatteryLife";
        Activated += (_, e) =>
        {
            if (e.WindowActivationState == WindowActivationState.Deactivated && !Pinned) Hide();
        };
        Root.KeyDown += (_, e) =>
        {
            if (e.Key == Windows.System.VirtualKey.Escape) Hide();
        };

        StartupItem.Text = text.StartWithWindows;
        ExitItem.Text = text.Exit;
        ToolTipService.SetToolTip(RefreshButton, text.Refresh);
        AutomationProperties.SetName(RefreshButton, text.Refresh);
        ToolTipService.SetToolTip(SettingsButton, text.Settings);
        AutomationProperties.SetName(SettingsButton, text.Settings);
        Footer.Text = text.AlertFooter(Board.DefaultThreshold);
        Render(Board.From([]), animate: false);
    }

    public bool IsOpen => AppWindow.IsVisible;

    /// <summary>Stays open when focus moves away (--open, for screenshots and review).</summary>
    public bool Pinned { get; set; }

    /// <summary>Opens the panel; a click on the tray icon right after it closed (the same click
    /// that took focus away) should not reopen it.</summary>
    public void Toggle()
    {
        if (IsOpen) { Hide(); return; }
        if (DateTime.UtcNow - _hiddenAt < TimeSpan.FromMilliseconds(250)) return;
        StartupItem.IsChecked = Startup.IsEnabled;
        PlaceAboveTray();
        AppWindow.Show();
        Activate();
        _refresh();
    }

    private void Hide()
    {
        if (!AppWindow.IsVisible) return;
        AppWindow.Hide();
        _hiddenAt = DateTime.UtcNow;
    }

    public void Render(Board board, bool animate = true)
    {
        var action = board.State is BoardState.Action or BoardState.Critical;
        var fg = (SolidColorBrush)Application.Current.Resources[action ? "SignInkBrush" : "SignTextBrush"];
        Band.Background = (Brush)Application.Current.Resources[action ? "SignYellowBrush" : "SignRaisedBrush"];
        Headline.Foreground = Figure.Foreground = Detail.Foreground = fg;
        BandPictogram.Background = (Brush)Application.Current.Resources[action ? "SignInkBrush" : "SignRuleBrush"];
        BandIcon.Glyph = board.Lead is { } lead ? Pictograms.For(lead.Kind) : "";

        var headline = _text.Headline(board);
        void Apply()
        {
            Headline.Text = headline;
            Figure.Text = _text.Figure(board);
            Figure.Visibility = Figure.Text.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
            Detail.Text = _text.Detail(board);
            AutomationProperties.SetName(Band, $"{Headline.Text}. {Figure.Text}. {Detail.Text}");
        }
        if (animate && headline != _shownHeadline && IsOpen && new Windows.UI.ViewManagement.UISettings().AnimationsEnabled)
            FlipBand(Apply);
        else
            Apply();
        _shownHeadline = headline;

        Rows.Children.Clear();
        foreach (var d in board.Devices) Rows.Children.Add(RowView.Build(d, _text));
        if (IsOpen) PlaceAboveTray();
    }

    /// <summary>The signature move: the band turns over like a split-flap sign when the next
    /// decision changes. Skipped when Windows animations are off.</summary>
    private void FlipBand(Action swap)
    {
        var down = new DoubleAnimation
        {
            To = -90, Duration = TimeSpan.FromMilliseconds(110),
            EasingFunction = new ExponentialEase { EasingMode = EasingMode.EaseIn, Exponent = 4 },
        };
        Storyboard.SetTarget(down, Flap);
        Storyboard.SetTargetProperty(down, nameof(PlaneProjection.RotationX));
        var first = new Storyboard();
        first.Children.Add(down);
        first.Completed += (_, _) =>
        {
            swap();
            Flap.RotationX = 90;
            var up = new DoubleAnimation
            {
                To = 0, Duration = TimeSpan.FromMilliseconds(220),
                EasingFunction = new ExponentialEase { EasingMode = EasingMode.EaseOut, Exponent = 5 },
            };
            Storyboard.SetTarget(up, Flap);
            Storyboard.SetTargetProperty(up, nameof(PlaneProjection.RotationX));
            var second = new Storyboard();
            second.Children.Add(up);
            second.Begin();
        };
        first.Begin();
    }

    /// <summary>Bottom-right of the work area of the monitor under the cursor, sized to content.</summary>
    private void PlaceAboveTray()
    {
        GetCursorPos(out var cursor);
        var area = DisplayArea.GetFromPoint(new PointInt32(cursor.X, cursor.Y), DisplayAreaFallback.Primary).WorkArea;
        var scale = GetDpiForWindow(Win32Interop.GetWindowFromWindowId(AppWindow.Id)) / 96.0;
        if (scale <= 0) scale = 1;

        Root.Measure(new Windows.Foundation.Size(WidthDip, double.PositiveInfinity));
        int w = (int)Math.Ceiling(WidthDip * scale);
        int h = (int)Math.Ceiling(Root.DesiredSize.Height * scale);
        int m = (int)(MarginDip * scale);
        AppWindow.MoveAndResize(new RectInt32(area.X + area.Width - w - m, area.Y + area.Height - h - m, w, h));
    }

    private void OnRefresh(object sender, RoutedEventArgs e) => _refresh();

    private void OnStartupToggle(object sender, RoutedEventArgs e) => Startup.Set(StartupItem.IsChecked);

    private void OnExit(object sender, RoutedEventArgs e) => _exit();

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT { public int X, Y; }

    [DllImport("user32.dll")]
    private static extern bool GetCursorPos(out POINT p);

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hwnd);
}
