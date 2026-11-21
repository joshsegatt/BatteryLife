using System.Globalization;
using System.Windows.Input;
using BatteryLife.Logic;
using H.NotifyIcon;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.Windows.AppNotifications;
using Microsoft.Windows.AppNotifications.Builder;

namespace BatteryLife.App;

/// <summary>
/// Tray app: no main window. The tray icon shows the most urgent level and opens the hub panel.
/// </summary>
public partial class App : Application
{
    private Mutex? _single;
    private DispatcherQueue? _ui;
    private Text _text = Text.For(CultureInfo.CurrentUICulture);
    private FlyoutWindow? _flyout;
    private TaskbarIcon? _tray;
    private DeviceTrayIcons? _deviceIcons;
    private CoreClient? _core;
    private readonly AlertRules _alerts = new();
    private bool _toasts;
    private bool _openOnFirstUpdate;
    private bool _demo;
    private Board? _lastBoard;

    public App() => InitializeComponent();

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        _single = new Mutex(true, @"Local\BatteryLife.SingleInstance", out bool first);
        if (!first)
        {
            Exit(); // already running: its tray icon is the way in
            return;
        }
        _ui = DispatcherQueue.GetForCurrentThread();
        // "--lang en" / "--lang pt-BR" overrides the Windows display language (screenshots, testing).
        var langArg = Environment.GetCommandLineArgs().SkipWhile(a => a != "--lang").Skip(1).FirstOrDefault();
        CultureInfo culture = CultureInfo.CurrentUICulture;
        if (langArg is not null)
        {
            try { culture = new CultureInfo(langArg); }
            catch (CultureNotFoundException) { }
        }
        _text = Text.For(culture);
        _flyout = new FlyoutWindow(_text, () => _core?.Refresh(), Quit);

        // Left click opens the hub; right click shows the native Windows menu, as tray users expect.
        var startup = new ToggleMenuFlyoutItem { Text = _text.StartWithWindows, IsChecked = Startup.IsEnabled };
        startup.Command = new Command(() => { Startup.Set(!Startup.IsEnabled); startup.IsChecked = Startup.IsEnabled; });
        var menu = new MenuFlyout();
        menu.Items.Add(new MenuFlyoutItem { Text = _text.Open, Command = new Command(() => _flyout.Toggle()) });
        menu.Items.Add(new MenuFlyoutItem { Text = _text.Refresh, Command = new Command(() => _core?.Refresh()) });
        menu.Items.Add(startup);
        menu.Items.Add(new MenuFlyoutSeparator());
        menu.Items.Add(new MenuFlyoutItem { Text = _text.Exit, Command = new Command(Quit) });
        _tray = new TaskbarIcon
        {
            ToolTipText = "BatteryLife",
            NoLeftClickDelay = true,
            LeftClickCommand = new Command(() => _flyout.Toggle()),
            ContextMenuMode = ContextMenuMode.PopupMenu,
            ContextFlyout = menu,
            IconSource = TrayIcon(Board.From([])),
        };
        _tray.ForceCreate(enablesEfficiencyMode: false);
        _deviceIcons = new DeviceTrayIcons(_text, _tray.LeftClickCommand, menu);

        try
        {
            AppNotificationManager.Default.NotificationInvoked += (sender, e) => _ui.TryEnqueue(() =>
            {
                if (e.Arguments.TryGetValue("action", out var action) && action == "pin")
                    _ = Windows.System.Launcher.LaunchUriAsync(new Uri("ms-settings:taskbar"));
                else
                    _flyout.Toggle();
            });
            AppNotificationManager.Default.Register();
            _toasts = true;
        }
        catch (Exception)
        {
            _toasts = false; // fall back to the tray balloon
        }

        // A contrast theme switched on while running repaints the code-built parts right away.
        Palette.Ui.ColorValuesChanged += (_, _) => _ui.TryEnqueue(() =>
        {
            if (_lastBoard is null) return;
            _flyout.Render(_lastBoard, animate: false);
            if (_tray is not null) _tray.IconSource = TrayIcon(_lastBoard);
            _deviceIcons?.Repaint();
        });

        var argv = Environment.GetCommandLineArgs();
        _openOnFirstUpdate = argv.Contains("--open");
        _flyout.Pinned = _openOnFirstUpdate;
        _demo = argv.Contains("--demo");
        if (_demo)
        {
            // Synthetic devices for screenshots and design review: no alerts, no first-run hint.
            OnDevices(Demo.Devices(argv.Contains("--calm")));
            return;
        }

        // Windows puts new tray icons in the ^ overflow and no app may pin itself: show the way once.
        if (Startup.IsFirstRun()) Show(_text.WelcomeTitle, _text.WelcomeBody, (_text.PinIcon, "pin"));

        _core = new CoreClient(Path.Combine(AppContext.BaseDirectory, "batterylife-core.exe"));
        _core.Updated += devices => _ui.TryEnqueue(() => OnDevices(devices));
        _core.Failed += message => _ui.TryEnqueue(() =>
        {
            // Say the reader is down instead of "no devices found", which would send the user off
            // switching devices on and off for nothing.
            _lastBoard = Board.ReaderDown();
            _flyout.Render(_lastBoard);
            _tray.IconSource = TrayIcon(_lastBoard);
            _tray.ToolTipText = $"BatteryLife: {_text.CoreMissing}";
            _deviceIcons?.Update([]); // levels are unknown now, not stale
            System.Diagnostics.Debug.WriteLine(message);
            if (_openOnFirstUpdate)
            {
                _openOnFirstUpdate = false;
                _flyout.Toggle();
            }
        });
        _core.Start();
    }

    private void OnDevices(IReadOnlyList<DeviceReading> devices)
    {
        var board = Board.From(devices);
        _lastBoard = board;
        _flyout?.Render(board);
        if (_tray is not null)
        {
            _tray.IconSource = TrayIcon(board);
            _tray.ToolTipText = _text.Tooltip(devices);
        }
        _deviceIcons?.Update(devices);
        if (!_demo)
            foreach (var d in _alerts.Check(devices)) Notify(d);
        if (_openOnFirstUpdate)
        {
            _openOnFirstUpdate = false;
            _flyout?.Toggle();
        }
    }

    /// <summary>The level of the device that runs out first, drawn into the icon. Yellow only when
    /// something needs charging, like the band.</summary>
    private static GeneratedIconSource TrayIcon(Board board)
    {
        bool action = board.State is BoardState.Action or BoardState.Critical;
        string text = board.State == BoardState.ReaderDown ? "!"
            : board.Lead?.Percent is int p ? p.ToString(CultureInfo.InvariantCulture) : "–";
        return new GeneratedIconSource
        {
            Text = text,
            FontFamily = new FontFamily("Segoe UI Variable Display"),
            FontWeight = FontWeights.Bold,
            FontSize = text.Length switch { >= 3 => 26, 2 => 34, _ => 40 },
            // Same sign brushes as the panel, so a contrast theme recolors the tray icon too.
            Foreground = Palette.Brush(action ? "SignInkBrush" : "SignTextBrush"),
            Background = Palette.Brush(action ? "SignYellowBrush" : "SignRaisedBrush"),
            CornerRadius = new CornerRadius(6),
            Size = 64,
        };
    }

    private void Notify(DeviceReading d) => Show(_text.NotificationTitle(d), _text.NotificationBody(d));

    private void Show(string title, string body, (string Label, string Action)? button = null)
    {
        if (_toasts)
        {
            try
            {
                var toast = new AppNotificationBuilder().AddText(title).AddText(body);
                if (button is { } b) toast.AddButton(new AppNotificationButton(b.Label).AddArgument("action", b.Action));
                AppNotificationManager.Default.Show(toast.BuildNotification());
                return;
            }
            catch (Exception)
            {
                _toasts = false;
            }
        }
        _tray?.ShowNotification(title, body);
    }

    private void Quit()
    {
        _core?.Dispose();
        if (_toasts) AppNotificationManager.Default.Unregister();
        _deviceIcons?.Dispose();
        _tray?.Dispose();
        _flyout?.Close();
        _single?.ReleaseMutex();
        Exit();
    }

    private sealed class Command(Action run) : ICommand
    {
        public event EventHandler? CanExecuteChanged { add { } remove { } }
        public bool CanExecute(object? parameter) => true;
        public void Execute(object? parameter) => run();
    }
}
