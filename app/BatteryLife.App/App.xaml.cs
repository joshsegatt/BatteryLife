using System.Globalization;
using System.Windows.Input;
using BatteryLife.Logic;
using H.NotifyIcon;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
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
    private CoreClient? _core;
    private readonly AlertRules _alerts = new();
    private bool _toasts;
    private bool _openOnFirstUpdate;

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
        _text = Text.For(CultureInfo.CurrentUICulture);
        _flyout = new FlyoutWindow(_text, () => _core?.Refresh(), Quit);

        var toggle = new Command(() => _flyout.Toggle());
        _tray = new TaskbarIcon
        {
            ToolTipText = "BatteryLife",
            NoLeftClickDelay = true,
            LeftClickCommand = toggle,
            RightClickCommand = toggle,
            IconSource = TrayIcon(Board.From([])),
        };
        _tray.ForceCreate(enablesEfficiencyMode: false);

        try
        {
            AppNotificationManager.Default.NotificationInvoked += (_, _) => _ui.TryEnqueue(() => _flyout.Toggle());
            AppNotificationManager.Default.Register();
            _toasts = true;
        }
        catch (Exception)
        {
            _toasts = false; // fall back to the tray balloon
        }

        var argv = Environment.GetCommandLineArgs();
        _openOnFirstUpdate = argv.Contains("--open");
        _flyout.Pinned = _openOnFirstUpdate;
        if (argv.Contains("--demo"))
        {
            // Synthetic devices for screenshots and design review; never mixed with real readings.
            OnDevices(Demo.Devices(argv.Contains("--calm")));
            return;
        }

        _core = new CoreClient(Path.Combine(AppContext.BaseDirectory, "batterylife-core.exe"));
        _core.Updated += devices => _ui.TryEnqueue(() => OnDevices(devices));
        _core.Failed += message => _ui.TryEnqueue(() =>
        {
            _tray.ToolTipText = $"BatteryLife: {_text.CoreMissing}";
            System.Diagnostics.Debug.WriteLine(message);
        });
        _core.Start();
    }

    private void OnDevices(IReadOnlyList<DeviceReading> devices)
    {
        var board = Board.From(devices);
        _flyout?.Render(board);
        if (_tray is not null)
        {
            _tray.IconSource = TrayIcon(board);
            _tray.ToolTipText = _text.Tooltip(devices);
        }
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
        string text = board.Lead?.Percent is int p ? (p >= 100 ? "F" : p.ToString(CultureInfo.InvariantCulture)) : "–";
        return new GeneratedIconSource
        {
            Text = text,
            FontFamily = new FontFamily("Segoe UI Variable Display"),
            FontWeight = FontWeights.Bold,
            FontSize = text.Length > 1 ? 34 : 40,
            Foreground = new SolidColorBrush(action ? Microsoft.UI.ColorHelper.FromArgb(255, 0x11, 0x11, 0x11) : Microsoft.UI.Colors.White),
            Background = new SolidColorBrush(action ? Microsoft.UI.ColorHelper.FromArgb(255, 0xFF, 0xD2, 0x00) : Microsoft.UI.ColorHelper.FromArgb(255, 0x1C, 0x1C, 0x1C)),
            CornerRadius = new CornerRadius(6),
            Size = 64,
        };
    }

    private void Notify(DeviceReading d)
    {
        string title = _text.NotificationTitle(d), body = _text.NotificationBody(d);
        if (_toasts)
        {
            try
            {
                AppNotificationManager.Default.Show(new AppNotificationBuilder().AddText(title).AddText(body).BuildNotification());
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
