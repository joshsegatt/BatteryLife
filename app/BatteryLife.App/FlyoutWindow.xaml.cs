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
        presenter.IsAlwaysOnTop = true; // like Quick Settings: never opens behind other windows
        AppWindow.SetPresenter(presenter);
        AppWindow.IsShownInSwitchers = false;
        AppWindow.Title = "BatteryLife";
        // Windows 11 frame: rounded corners and a hairline in the sign's rule color.
        var hwnd = Win32Interop.GetWindowFromWindowId(AppWindow.Id);
        int round = 2; // DWMWCP_ROUND
        DwmSetWindowAttribute(hwnd, 33, ref round, sizeof(int)); // DWMWA_WINDOW_CORNER_PREFERENCE
        if (!Palette.HighContrast) // a contrast theme keeps the system's own border color
        {
            int border = 0x002A2A2A; // COLORREF 0x00BBGGRR
            DwmSetWindowAttribute(hwnd, 34, ref border, sizeof(int)); // DWMWA_BORDER_COLOR
        }
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
        SlideIn();
        _refresh();
    }

    /// <summary>Rises from the taskbar like Quick Settings: a short slide up and fade in.</summary>
    private void SlideIn()
    {
        Root.Opacity = 1;
        Slide.Y = 0;
        if (!Palette.Animations) return;
        var ease = new ExponentialEase { EasingMode = EasingMode.EaseOut, Exponent = 6 };
        var rise = new DoubleAnimation { From = 14, To = 0, Duration = TimeSpan.FromMilliseconds(260), EasingFunction = ease };
        var fade = new DoubleAnimation { From = 0, To = 1, Duration = TimeSpan.FromMilliseconds(160) };
        Storyboard.SetTarget(rise, Slide);
        Storyboard.SetTargetProperty(rise, nameof(TranslateTransform.Y));
        Storyboard.SetTarget(fade, Root);
        Storyboard.SetTargetProperty(fade, nameof(UIElement.Opacity));
        var sb = new Storyboard();
        sb.Children.Add(rise);
        sb.Children.Add(fade);
        // Pin the end state as local values: the panel must never be left part-faded.
        sb.Completed += (_, _) =>
        {
            sb.Stop();
            Root.Opacity = 1;
            Slide.Y = 0;
        };
        sb.Begin();
    }

    private void Hide()
    {
        if (!AppWindow.IsVisible) return;
        AppWindow.Hide();
        _hiddenAt = DateTime.UtcNow;
    }

    public void Render(Board board, bool animate = true)
    {
        var headline = _text.Headline(board);
        // Everything on the plate changes together, at the fold of the flip, never before it.
        void Apply()
        {
            var action = board.State is BoardState.Action or BoardState.Critical;
            Band.Background = Palette.Brush(action ? "SignYellowBrush" : "SignRaisedBrush");
            Headline.Foreground = Figure.Foreground = Detail.Foreground = Palette.Brush(action ? "SignInkBrush" : "SignTextBrush");
            BandPictogram.Background = Palette.Brush(action ? "SignInkBrush" : "SignRuleBrush");
            BandIcon.Foreground = Palette.Brush("SignTextBrush"); // white pictogram on its black square, always
            BandIcon.Glyph = Pictograms.For(board.Lead?.Kind ?? "");

            Headline.Text = headline;
            var figure = _text.Figure(board);
            SetFigure(figure);
            Figure.Visibility = figure.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
            Detail.Text = _text.Detail(board);
            AutomationProperties.SetName(Band, $"{headline}. {figure}. {Detail.Text}");
        }
        if (animate && headline != _shownHeadline && IsOpen && Palette.Animations)
            FlipBand(Apply);
        else
            Apply();
        _shownHeadline = headline;

        Rows.Children.Clear();
        foreach (var d in board.Devices) Rows.Children.Add(RowView.Build(d, _text));
        if (IsOpen) PlaceAboveTray();
    }

    /// <summary>Sets "2 h 10 min" with the numbers at full size and the units smaller and lighter,
    /// so the number reads first, as on a departure board.</summary>
    private void SetFigure(string figure)
    {
        Figure.Inlines.Clear();
        foreach (System.Text.RegularExpressions.Match m in System.Text.RegularExpressions.Regex.Matches(figure, @"\d+|\D+"))
        {
            bool number = char.IsDigit(m.Value[0]);
            Figure.Inlines.Add(new Microsoft.UI.Xaml.Documents.Run
            {
                Text = m.Value,
                FontSize = number ? 40 : 24,
                FontWeight = number ? Microsoft.UI.Text.FontWeights.Bold : Microsoft.UI.Text.FontWeights.SemiBold,
            });
        }
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

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attribute, ref int value, int size);
}
