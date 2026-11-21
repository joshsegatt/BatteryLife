using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Drawing.Text;
using System.Runtime.InteropServices;
using BatteryLife.Logic;
using Microsoft.Win32;

namespace BatteryLife.App;

/// <summary>
/// The battery strip inside the Windows taskbar, just left of the tray icons: each main device's
/// pictogram and level, with the yellow plate only on a device that needs charging.
///
/// Technique ported from BatteryHub's src/widget.cpp (commit fd2267d, GPL-3.0): a layered child
/// window of Shell_TrayWnd, drawn with per-pixel alpha, put back in place by a one-second timer and
/// recreated when Explorer restarts. It runs on a thread of its own with its own message loop:
/// Explorer waits on the thread that owns a taskbar child window, so that thread must never be the
/// WinUI thread (and never one that calls UI Automation).
/// </summary>
public sealed class TaskbarStrip : IDisposable
{
    /// <summary>One device as the strip draws it. Built on the UI thread, drawn on the strip thread.</summary>
    public sealed record Item(string Glyph, string Level, bool Action, bool Charging, bool Dim);

    private readonly Action _click;
    private readonly Action<Point> _rightClick;
    private readonly Thread _thread;
    private readonly object _lock = new();
    private IReadOnlyList<Item> _items = [];
    private string _tip = "";

    // Strip-thread state.
    private IntPtr _host, _strip, _taskbar, _tooltip;
    private WndProc? _hostProc, _stripProc; // kept alive: Windows calls them through function pointers
    private uint _taskbarCreated;
    private Size _size;
    private int _lastLeft = int.MinValue, _lastDpi;
    private bool _hover;
    private string _painted = ""; // what the strip shows now, so the timer redraws only on change
    private readonly TaskbarLayout _layout = new();
    private bool _fits; // false while there is no room for even one device

    /// <param name="click">Left click, raised on the strip thread.</param>
    /// <param name="rightClick">Right click with the cursor's screen position, raised on the strip thread.</param>
    public TaskbarStrip(Action click, Action<Point> rightClick)
    {
        _click = click;
        _rightClick = rightClick;
        _thread = new Thread(Run) { IsBackground = true, Name = "BatteryLife taskbar strip" };
        _thread.SetApartmentState(ApartmentState.STA);
        _thread.Start();
    }

    /// <summary>"Show in the taskbar" (on by default), kept with the app's other settings.</summary>
    public static bool Enabled
    {
        get
        {
            using var key = Registry.CurrentUser.OpenSubKey(@"Software\BatteryLife");
            return key?.GetValue("TaskbarStrip") is not int v || v != 0;
        }
        set
        {
            using var key = Registry.CurrentUser.CreateSubKey(@"Software\BatteryLife");
            key.SetValue("TaskbarStrip", value ? 1 : 0, RegistryValueKind.DWord);
        }
    }

    /// <summary>Thread-safe: applies a change of <see cref="Enabled"/> right away.</summary>
    public void Refresh()
    {
        if (_host != IntPtr.Zero) PostMessage(_host, WmRefresh, IntPtr.Zero, IntPtr.Zero);
    }

    /// <summary>Thread-safe: hands the strip what to show. An empty list hides it.</summary>
    public void Update(IReadOnlyList<Item> items, string tooltip)
    {
        lock (_lock)
        {
            _items = items;
            _tip = tooltip;
        }
        if (_host != IntPtr.Zero) PostMessage(_host, WmRefresh, IntPtr.Zero, IntPtr.Zero);
    }

    public void Dispose()
    {
        if (_host != IntPtr.Zero) PostMessage(_host, WmClose, IntPtr.Zero, IntPtr.Zero);
        _thread.Join(TimeSpan.FromSeconds(2));
        _layout.Dispose();
    }

    // ---- strip thread ------------------------------------------------------------------------

    private void Run()
    {
        // The taskbar is per-monitor DPI aware; the strip must measure in the same pixels.
        SetThreadDpiAwarenessContext(new IntPtr(-4));
        var instance = GetModuleHandle(null);
        _hostProc = HostProc;
        _stripProc = StripProc;
        RegisterClass("BatteryLife.StripHost", _hostProc, instance);
        RegisterClass("BatteryLife.Strip", _stripProc, instance);
        // A hidden top-level window, not message-only, so it receives TaskbarCreated broadcasts.
        _host = CreateWindowEx(0, "BatteryLife.StripHost", "", WsPopup, 0, 0, 0, 0, IntPtr.Zero, IntPtr.Zero, instance, IntPtr.Zero);
        _taskbarCreated = RegisterWindowMessage("TaskbarCreated");
        SetTimer(_host, 1, 1000, IntPtr.Zero);
        Reconcile();

        while (GetMessage(out var msg, IntPtr.Zero, 0, 0) > 0)
        {
            TranslateMessage(ref msg);
            DispatchMessage(ref msg);
        }
    }

    private IntPtr HostProc(IntPtr hwnd, uint msg, IntPtr w, IntPtr l)
    {
        if (msg == _taskbarCreated && msg != 0)
        {
            DestroyStrip(); // Explorer restarted: the old taskbar and our child are gone
            Reconcile();
            return IntPtr.Zero;
        }
        switch (msg)
        {
            case WmTimer:
            case WmRefresh:
                Reconcile();
                return IntPtr.Zero;
            case WmClose:
                DestroyStrip();
                DestroyWindow(hwnd);
                return IntPtr.Zero;
            case WmDestroy:
                PostQuitMessage(0);
                return IntPtr.Zero;
        }
        return DefWindowProc(hwnd, msg, w, l);
    }

    /// <summary>Creates, moves, repaints or hides the strip to match the taskbar and the data.</summary>
    private void Reconcile()
    {
        IReadOnlyList<Item> items;
        string tip;
        lock (_lock)
        {
            items = _items;
            tip = _tip;
        }
        if (items.Count == 0 || !Enabled)
        {
            DestroyStrip();
            return;
        }
        if (_strip == IntPtr.Zero || !IsWindow(_strip) || !IsWindow(_taskbar))
        {
            DestroyStrip();
            if (!CreateStrip()) return; // no taskbar right now (Explorer starting): try next second
        }
        bool light = TaskbarIsLight();
        int dpi = (int)GetDpiForWindow(_taskbar);
        if (dpi == 0) dpi = 96;
        if (!Bounds(dpi, out int right, out int room)) return;
        GetClientRect(_taskbar, out var bar);
        string look = $"{string.Join(";", items)}|{dpi}|{light}|{_hover}|{HighContrast()}|{bar.Bottom - bar.Top}|{room}";
        if (look != _painted)
        {
            _fits = Paint(items, dpi, light, room);
            if (_fits) SetTooltip(tip);
            _painted = look;
            _lastLeft = int.MinValue;
        }
        if (!_fits)
        {
            ShowWindow(_strip, SwHide); // the app buttons fill the taskbar: never cover them
            return;
        }
        Place(right, dpi);
    }

    /// <summary>Screen x where the strip ends (left of the tray, or of the Widgets button when a
    /// left-aligned taskbar puts it there) and the room it has before the last app button.</summary>
    private bool Bounds(int dpi, out int right, out int room)
    {
        right = room = 0;
        var tray = FindWindowEx(_taskbar, IntPtr.Zero, "TrayNotifyWnd", null);
        if (tray == IntPtr.Zero || !GetWindowRect(tray, out var trayRect) || !GetWindowRect(_taskbar, out var barRect)) return false;
        right = trayRect.Left;
        int left = barRect.Left;
        if (_layout.TasksRight is int tasks) left = tasks + Scale(8, dpi);
        var widgets = _layout.Widgets;
        if (widgets.Right > widgets.Left && widgets.Left >= left && widgets.Left < right)
            right = widgets.Left; // weather button between the apps and the tray
        room = right - Scale(4, dpi) - left;
        return true;
    }

    private bool CreateStrip()
    {
        _taskbar = FindWindowEx(IntPtr.Zero, IntPtr.Zero, "Shell_TrayWnd", null);
        if (_taskbar == IntPtr.Zero || FindWindowEx(_taskbar, IntPtr.Zero, "TrayNotifyWnd", null) == IntPtr.Zero) return false;
        _strip = CreateWindowEx(WsExLayered | WsExNoActivate, "BatteryLife.Strip", "", WsChild | WsClipSiblings,
            0, 0, 1, 1, _taskbar, IntPtr.Zero, GetModuleHandle(null), IntPtr.Zero);
        if (_strip == IntPtr.Zero) return false;
        _lastLeft = int.MinValue;
        _lastDpi = 0;
        _painted = "";
        _layout.Watch(_taskbar);

        _tooltip = CreateWindowEx(WsExTopmost, "tooltips_class32", null, TtsAlwaysTip | TtsNoPrefix,
            0, 0, 0, 0, _host, IntPtr.Zero, GetModuleHandle(null), IntPtr.Zero);
        if (_tooltip != IntPtr.Zero)
        {
            var info = ToolInfo(_tip);
            SendMessage(_tooltip, TtmAddTool, IntPtr.Zero, ref info);
            SendMessage(_tooltip, TtmSetMaxTipWidth, IntPtr.Zero, new IntPtr(400)); // allows one line per device
        }
        return true;
    }

    private void DestroyStrip()
    {
        if (_tooltip != IntPtr.Zero) DestroyWindow(_tooltip);
        if (_strip != IntPtr.Zero && IsWindow(_strip)) DestroyWindow(_strip);
        _tooltip = _strip = IntPtr.Zero;
        _layout.Watch(IntPtr.Zero);
    }

    /// <summary>Ends at <paramref name="right"/>, full taskbar height, above the taskbar's other content.</summary>
    private void Place(int right, int dpi)
    {
        if (!GetClientRect(_taskbar, out var bar)) return;
        var point = new PointStruct { X = right - _size.Width - Scale(4, dpi), Y = 0 };
        ScreenToClient(_taskbar, ref point);
        int left = Math.Max(0, point.X);
        int height = bar.Bottom - bar.Top;
        bool covered = GetWindow(_strip, GwHwndPrev) != IntPtr.Zero; // something of the taskbar's sits above us
        if (left == _lastLeft && dpi == _lastDpi && !covered && IsWindowVisible(_strip)) return;
        _lastLeft = left;
        _lastDpi = dpi;
        SetWindowPos(_strip, HwndTop, left, 0, _size.Width, height, SwpNoActivate | SwpShowWindow);
    }

    private static int Scale(int px, int dpi) => px * dpi / 96;

    // ---- drawing -----------------------------------------------------------------------------

    /// <summary>Draws the devices that fit in <paramref name="room"/> pixels; false when none does.</summary>
    private bool Paint(IReadOnlyList<Item> all, int dpi, bool light, int room)
    {
        if (!GetClientRect(_taskbar, out var bar)) return false;
        int height = Math.Max(1, bar.Bottom - bar.Top);
        var text = light ? System.Drawing.Color.FromArgb(0x11, 0x11, 0x11) : System.Drawing.Color.White;
        var yellow = System.Drawing.Color.FromArgb(0xFF, 0xD2, 0x00);
        var ink = System.Drawing.Color.FromArgb(0x11, 0x11, 0x11);
        if (HighContrast())
        {
            text = System.Drawing.Color.FromKnownColor(KnownColor.WindowText);
            yellow = System.Drawing.Color.FromKnownColor(KnownColor.Highlight);
            ink = System.Drawing.Color.FromKnownColor(KnownColor.HighlightText);
        }

        using var glyphFont = new Font(PictogramFamily.Value, Scale(16, dpi), GraphicsUnit.Pixel);
        using var boltFont = new Font(PictogramFamily.Value, Scale(11, dpi), GraphicsUnit.Pixel);
        using var levelFont = new Font(TextFamily.Value, Scale(14, dpi), FontStyle.Bold, GraphicsUnit.Pixel);
        int pad = Scale(6, dpi), gap = Scale(4, dpi), between = Scale(2, dpi);

        // Measure first: each item is pad, pictogram, gap, [bolt], level, pad.
        using var measure = new Bitmap(1, 1);
        using var mg = Graphics.FromImage(measure);
        mg.TextRenderingHint = TextRenderingHint.AntiAliasGridFit;
        var allWidths = all.Select(i =>
        {
            float w = pad + Scale(16, dpi) + gap + Measure(mg, i.Level, levelFont) + pad;
            if (i.Charging) w += Scale(11, dpi);
            return (int)MathF.Ceiling(w);
        }).ToList();
        // Too many devices for the room left by the app buttons: a device that needs charging is
        // kept first, then the others in their fixed order; the hub still lists every one.
        int edge = 2 * Scale(2, dpi), used = edge;
        var kept = new SortedSet<int>();
        foreach (int i in Enumerable.Range(0, all.Count).OrderBy(i => all[i].Action ? 0 : 1).ThenBy(i => i))
        {
            int add = allWidths[i] + (kept.Count > 0 ? between : 0);
            if (used + add > room) continue;
            kept.Add(i);
            used += add;
        }
        if (kept.Count == 0) return false;
        var items = kept.Select(i => all[i]).ToList();
        var widths = kept.Select(i => allWidths[i]).ToList();
        int width = used;

        using var bitmap = new Bitmap(width, height, PixelFormat.Format32bppPArgb);
        using (var g = Graphics.FromImage(bitmap))
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.TextRenderingHint = TextRenderingHint.AntiAliasGridFit;
            // Alpha 1 everywhere: invisible, but the whole strip still takes clicks and hover.
            g.Clear(System.Drawing.Color.FromArgb(1, 0, 0, 0));
            if (_hover)
            {
                using var hover = new SolidBrush(System.Drawing.Color.FromArgb(light ? 20 : 26, text));
                using var path = Rounded(new RectangleF(0, Scale(4, dpi), width, height - 2 * Scale(4, dpi)), Scale(4, dpi));
                g.FillPath(hover, path);
            }
            var plateHeight = Scale(28, dpi);
            float plateTop = (height - plateHeight) / 2f;
            float x = Scale(2, dpi);
            for (int n = 0; n < items.Count; n++)
            {
                var item = items[n];
                var color = item.Action ? ink : text;
                if (item.Dim && !item.Action) color = System.Drawing.Color.FromArgb(140, color); // asleep: 55%, like its row
                if (item.Action)
                {
                    // Square plate, like every sign plate: the one yellow thing on the taskbar.
                    using var plate = new SolidBrush(yellow);
                    g.FillRectangle(plate, x, plateTop, widths[n], plateHeight);
                }
                using var brush = new SolidBrush(color);
                float cx = x + pad;
                DrawCentered(g, item.Glyph, glyphFont, brush, cx, height);
                cx += Scale(16, dpi) + gap;
                if (item.Charging)
                {
                    DrawCentered(g, "", boltFont, brush, cx - Scale(2, dpi), height);
                    cx += Scale(11, dpi);
                }
                DrawCentered(g, item.Level, levelFont, brush, cx, height);
                x += widths[n] + between;
            }
        }
        _size = new Size(width, height);
        Push(bitmap);
        return true;
    }

    private static float Measure(Graphics g, string s, Font f) =>
        g.MeasureString(s, f, PointF.Empty, StringFormat.GenericTypographic).Width;

    private static void DrawCentered(Graphics g, string s, Font f, Brush b, float x, int height)
    {
        var size = g.MeasureString(s, f, PointF.Empty, StringFormat.GenericTypographic);
        g.DrawString(s, f, b, x, (height - size.Height) / 2f, StringFormat.GenericTypographic);
    }

    private static GraphicsPath Rounded(RectangleF r, float radius)
    {
        float d = radius * 2;
        var path = new GraphicsPath();
        path.AddArc(r.Left, r.Top, d, d, 180, 90);
        path.AddArc(r.Right - d, r.Top, d, d, 270, 90);
        path.AddArc(r.Right - d, r.Bottom - d, d, d, 0, 90);
        path.AddArc(r.Left, r.Bottom - d, d, d, 90, 90);
        path.CloseFigure();
        return path;
    }

    /// <summary>Hands the premultiplied bitmap to the layered window.</summary>
    private void Push(Bitmap bitmap)
    {
        var screen = GetDC(IntPtr.Zero);
        var memory = CreateCompatibleDC(screen);
        var hbitmap = bitmap.GetHbitmap(System.Drawing.Color.FromArgb(0)); // keeps the alpha channel
        var old = SelectObject(memory, hbitmap);
        try
        {
            var size = new SizeStruct { Cx = bitmap.Width, Cy = bitmap.Height };
            var source = new PointStruct();
            var blend = new BlendFunction { BlendOp = 0, SourceConstantAlpha = 255, AlphaFormat = 1 };
            UpdateLayeredWindow(_strip, screen, IntPtr.Zero, ref size, memory, ref source, 0, ref blend, UlwAlpha);
        }
        finally
        {
            SelectObject(memory, old);
            DeleteObject(hbitmap);
            DeleteDC(memory);
            ReleaseDC(IntPtr.Zero, screen);
        }
    }

    private static readonly Lazy<string> PictogramFamily = new(() =>
        Installed("Segoe Fluent Icons") ? "Segoe Fluent Icons" : "Segoe MDL2 Assets");

    private static readonly Lazy<string> TextFamily = new(() =>
        Installed("Segoe UI Variable Text") ? "Segoe UI Variable Text" : "Segoe UI");

    private static bool Installed(string family)
    {
        using var fonts = new InstalledFontCollection();
        return fonts.Families.Any(f => f.Name == family);
    }

    private static bool TaskbarIsLight()
    {
        using var key = Registry.CurrentUser.OpenSubKey(@"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        return key?.GetValue("SystemUsesLightTheme") is int v && v != 0;
    }

    private static bool HighContrast()
    {
        var hc = new HighContrastInfo { Size = Marshal.SizeOf<HighContrastInfo>() };
        return SystemParametersInfo(SpiGetHighContrast, hc.Size, ref hc, 0) && (hc.Flags & HcfHighContrastOn) != 0;
    }

    // ---- input -------------------------------------------------------------------------------

    private IntPtr StripProc(IntPtr hwnd, uint msg, IntPtr w, IntPtr l)
    {
        switch (msg)
        {
            case WmMouseMove when !_hover:
                _hover = true;
                var track = new TrackMouseEventStruct { Size = Marshal.SizeOf<TrackMouseEventStruct>(), Flags = TmeLeave, Track = hwnd };
                TrackMouseEvent(ref track);
                Reconcile();
                return IntPtr.Zero;
            case WmMouseLeave:
                _hover = false;
                Reconcile();
                return IntPtr.Zero;
            case WmLButtonUp:
                _click();
                return IntPtr.Zero;
            case WmRButtonUp:
                GetCursorPos(out var cursor);
                _rightClick(new Point(cursor.X, cursor.Y));
                return IntPtr.Zero;
            case WmMouseActivate:
                return new IntPtr(MaNoActivate); // never take focus from the user's window
        }
        return DefWindowProc(hwnd, msg, w, l);
    }

    private void SetTooltip(string text)
    {
        if (_tooltip == IntPtr.Zero) return;
        var info = ToolInfo(text);
        SendMessage(_tooltip, TtmUpdateTipText, IntPtr.Zero, ref info);
    }

    private ToolInfoStruct ToolInfo(string text) => new()
    {
        Size = Marshal.SizeOf<ToolInfoStruct>(),
        Flags = TtfIdIsHwnd | TtfSubclass,
        Hwnd = _host,
        Id = _strip,
        Text = text,
    };

    // ---- what else is on the taskbar ---------------------------------------------------------

    /// <summary>
    /// Where the taskbar's own XAML content sits, which only UI Automation can tell: the right edge
    /// of the last app button (so the strip never covers one, even with many pinned apps) and the
    /// Widgets (weather) button, which a left-aligned taskbar puts right next to the tray.
    /// The lookup runs on a thread of its own every few seconds: UI Automation waits on Explorer,
    /// and Explorer waits on the strip's thread, so calling it from the strip thread would deadlock
    /// both (the note in BatteryHub's widget.cpp describes that hang).
    /// </summary>
    private sealed class TaskbarLayout : IDisposable
    {
        private readonly object _gate = new();
        private readonly AutoResetEvent _wake = new(false);
        private IntPtr _taskbar;
        private Rect _widgets;
        private int? _tasksRight;
        private Thread? _thread;
        private volatile bool _stop;

        /// <summary>Screen rectangle of the Widgets button, or empty.</summary>
        public Rect Widgets
        {
            get { lock (_gate) return _widgets; }
        }

        /// <summary>Screen x of the right edge of the rightmost app button left of the tray, if known.</summary>
        public int? TasksRight
        {
            get { lock (_gate) return _tasksRight; }
        }

        public void Watch(IntPtr taskbar)
        {
            lock (_gate)
            {
                _taskbar = taskbar;
                _widgets = default;
                _tasksRight = null;
            }
            if (_thread is null && taskbar != IntPtr.Zero)
            {
                _thread = new Thread(Loop) { IsBackground = true, Name = "BatteryLife taskbar layout" };
                _thread.SetApartmentState(ApartmentState.MTA);
                _thread.Start();
            }
            _wake.Set();
        }

        public void Dispose()
        {
            _stop = true;
            _wake.Set();
        }

        private void Loop()
        {
            IUIAutomation? uia = null;
            while (!_stop)
            {
                IntPtr bar;
                lock (_gate) bar = _taskbar;
                if (bar == IntPtr.Zero) { _wake.WaitOne(); continue; }
                Rect widgets = default;
                int? tasksRight = null;
                try
                {
                    uia ??= (IUIAutomation)new CUIAutomation();
                    var root = uia.ElementFromHandle(bar);
                    var tray = FindWindowEx(bar, IntPtr.Zero, "TrayNotifyWnd", null);
                    GetWindowRect(tray, out var trayRect);
                    GetWindowRect(bar, out var barRect);
                    bool InBar(Rect r) => r.Right > r.Left && r.Left >= barRect.Left && r.Right <= barRect.Right;

                    var button = root.FindFirst(TreeScopeDescendants, uia.CreatePropertyCondition(UiaAutomationIdPropertyId, "WidgetsButton"));
                    if (button is not null && InBar(button.CurrentBoundingRectangle)) widgets = button.CurrentBoundingRectangle;

                    var tasks = root.FindAll(TreeScopeDescendants, uia.CreatePropertyCondition(UiaClassNamePropertyId, "Taskbar.TaskListButtonAutomationPeer"));
                    for (int i = 0; tasks is not null && i < tasks.Length; i++)
                    {
                        var r = tasks.GetElement(i).CurrentBoundingRectangle;
                        if (!InBar(r) || r.Right > trayRect.Left) continue;
                        if (widgets.Right > widgets.Left && r.Left >= widgets.Right) continue; // right of the weather button
                        if (tasksRight is not int t || r.Right > t) tasksRight = r.Right;
                    }
                }
                catch (Exception e) when (e is COMException or InvalidCastException)
                {
                    // Explorer restarting, elements changing, or no UI Automation: the strip keeps
                    // its default place until the next round.
                }
                lock (_gate)
                {
                    if (_taskbar == bar)
                    {
                        _widgets = widgets;
                        _tasksRight = tasksRight;
                    }
                }
                _wake.WaitOne(3000);
            }
        }

        private const int UiaAutomationIdPropertyId = 30011;
        private const int UiaClassNamePropertyId = 30012;
        private const int TreeScopeDescendants = 4;

        [ComImport, Guid("ff48dba4-60ef-4201-aa87-54103eef594e")]
        private class CUIAutomation;

        // Only the slots up to the methods used are declared; the placeholders keep the vtable order.
        [ComImport, Guid("30cbe57d-d9d0-452a-ab13-7ac5ac4825ee"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        private interface IUIAutomation
        {
            void CompareElements();
            void CompareRuntimeIds();
            void GetRootElement();
            IUIAutomationElement ElementFromHandle(IntPtr hwnd);
            void ElementFromPoint();
            void GetFocusedElement();
            void GetRootElementBuildCache();
            void ElementFromHandleBuildCache();
            void ElementFromPointBuildCache();
            void GetFocusedElementBuildCache();
            void CreateTreeWalker();
            void ControlViewWalker();
            void ContentViewWalker();
            void RawViewWalker();
            void RawViewCondition();
            void ControlViewCondition();
            void ContentViewCondition();
            void CreateCacheRequest();
            void CreateTrueCondition();
            void CreateFalseCondition();
            [return: MarshalAs(UnmanagedType.Interface)]
            object CreatePropertyCondition(int propertyId, [MarshalAs(UnmanagedType.Struct)] object value);
        }

        [ComImport, Guid("14314595-b4bc-4055-95f2-58f2e42c9855"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        private interface IUIAutomationElementArray
        {
            int Length { get; }
            IUIAutomationElement GetElement(int index);
        }

        [ComImport, Guid("d22108aa-8ac5-49a5-837b-37bbb3d7591e"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
        private interface IUIAutomationElement
        {
            void SetFocus();
            void GetRuntimeId();
            IUIAutomationElement? FindFirst(int scope, [MarshalAs(UnmanagedType.Interface)] object condition);
            IUIAutomationElementArray? FindAll(int scope, [MarshalAs(UnmanagedType.Interface)] object condition);
            void FindFirstBuildCache();
            void FindAllBuildCache();
            void BuildUpdatedCache();
            void GetCurrentPropertyValue();
            void GetCurrentPropertyValueEx();
            void GetCachedPropertyValue();
            void GetCachedPropertyValueEx();
            void GetCurrentPatternAs();
            void GetCachedPatternAs();
            void GetCurrentPattern();
            void GetCachedPattern();
            void GetCachedParent();
            void GetCachedChildren();
            void CurrentProcessId();
            void CurrentControlType();
            void CurrentLocalizedControlType();
            void CurrentName();
            void CurrentAcceleratorKey();
            void CurrentAccessKey();
            void CurrentHasKeyboardFocus();
            void CurrentIsKeyboardFocusable();
            void CurrentIsEnabled();
            void CurrentAutomationId();
            void CurrentClassName();
            void CurrentHelpText();
            void CurrentCulture();
            void CurrentIsControlElement();
            void CurrentIsContentElement();
            void CurrentIsPassword();
            void CurrentNativeWindowHandle();
            void CurrentItemType();
            void CurrentIsOffscreen();
            void CurrentOrientation();
            void CurrentFrameworkId();
            void CurrentIsRequiredForForm();
            void CurrentItemStatus();
            Rect CurrentBoundingRectangle { get; }
        }
    }

    // ---- Win32 -------------------------------------------------------------------------------

    private delegate IntPtr WndProc(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);

    private const uint WsPopup = 0x80000000, WsChild = 0x40000000, WsClipSiblings = 0x04000000;
    private const uint WsExLayered = 0x00080000, WsExNoActivate = 0x08000000, WsExTopmost = 0x00000008;
    private const uint WmDestroy = 0x0002, WmClose = 0x0010, WmTimer = 0x0113, WmMouseMove = 0x0200,
        WmLButtonUp = 0x0202, WmRButtonUp = 0x0205, WmMouseLeave = 0x02A3, WmMouseActivate = 0x0021,
        WmRefresh = 0x8001; // WM_APP + 1
    private const int SwHide = 0, MaNoActivate = 3, TmeLeave = 2, GwHwndPrev = 3, UlwAlpha = 2;
    private const uint SwpNoActivate = 0x0010, SwpShowWindow = 0x0040;
    private static readonly IntPtr HwndTop = IntPtr.Zero;
    private const uint TtsAlwaysTip = 0x01, TtsNoPrefix = 0x02;
    private const int TtfIdIsHwnd = 0x01, TtfSubclass = 0x10;
    private const uint TtmAddTool = 0x0432, TtmUpdateTipText = 0x0439, TtmSetMaxTipWidth = 0x0418; // W versions
    private const uint SpiGetHighContrast = 0x0042;
    private const int HcfHighContrastOn = 0x01;

    [StructLayout(LayoutKind.Sequential)] private struct Rect { public int Left, Top, Right, Bottom; public readonly int Width => Right - Left; }
    [StructLayout(LayoutKind.Sequential)] private struct PointStruct { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] private struct SizeStruct { public int Cx, Cy; }
    [StructLayout(LayoutKind.Sequential)] private struct BlendFunction { public byte BlendOp, BlendFlags, SourceConstantAlpha, AlphaFormat; }
    [StructLayout(LayoutKind.Sequential)] private struct Msg { public IntPtr Hwnd; public uint Message; public IntPtr W, L; public uint Time; public PointStruct Pt; }
    [StructLayout(LayoutKind.Sequential)] private struct TrackMouseEventStruct { public int Size, Flags; public IntPtr Track; public int HoverTime; }
    [StructLayout(LayoutKind.Sequential)] private struct HighContrastInfo { public int Size, Flags; public IntPtr Scheme; }

    /// <summary>TOOLINFOW without lpReserved, the size every comctl32 version accepts.</summary>
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct ToolInfoStruct
    {
        public int Size, Flags;
        public IntPtr Hwnd, Id;
        public Rect Rect;
        public IntPtr Instance;
        [MarshalAs(UnmanagedType.LPWStr)] public string Text;
        public IntPtr Param;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WndClass
    {
        public uint Style;
        public IntPtr Proc;
        public int ClassExtra, WindowExtra;
        public IntPtr Instance, Icon, Cursor, Background;
        public string? MenuName, ClassName;
    }

    private static void RegisterClass(string name, WndProc proc, IntPtr instance)
    {
        var wc = new WndClass
        {
            Proc = Marshal.GetFunctionPointerForDelegate(proc),
            Instance = instance,
            Cursor = LoadCursor(IntPtr.Zero, new IntPtr(32512)), // arrow
            ClassName = name,
        };
        RegisterClassW(ref wc);
    }

    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern ushort RegisterClassW(ref WndClass wc);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateWindowEx(uint exStyle, string cls, string? name, uint style, int x, int y, int w, int h, IntPtr parent, IntPtr menu, IntPtr instance, IntPtr param);
    [DllImport("user32.dll")] private static extern bool DestroyWindow(IntPtr h);
    [DllImport("user32.dll")] private static extern IntPtr DefWindowProc(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] private static extern int GetMessage(out Msg msg, IntPtr h, uint min, uint max);
    [DllImport("user32.dll")] private static extern bool TranslateMessage(ref Msg msg);
    [DllImport("user32.dll")] private static extern IntPtr DispatchMessage(ref Msg msg);
    [DllImport("user32.dll")] private static extern void PostQuitMessage(int code);
    [DllImport("user32.dll")] private static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, ref ToolInfoStruct l);
    [DllImport("user32.dll")] private static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern uint RegisterWindowMessage(string name);
    [DllImport("user32.dll")] private static extern IntPtr SetTimer(IntPtr h, IntPtr id, uint ms, IntPtr proc);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string cls, string? name);
    [DllImport("user32.dll")] private static extern bool IsWindow(IntPtr h);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] private static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr h, out Rect r);
    [DllImport("user32.dll")] private static extern bool GetClientRect(IntPtr h, out Rect r);
    [DllImport("user32.dll")] private static extern bool ScreenToClient(IntPtr h, ref PointStruct p);
    [DllImport("user32.dll")] private static extern IntPtr GetWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] private static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int cy, uint flags);
    [DllImport("user32.dll")] private static extern uint GetDpiForWindow(IntPtr h);
    [DllImport("user32.dll")] private static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] private static extern bool TrackMouseEvent(ref TrackMouseEventStruct t);
    [DllImport("user32.dll")] private static extern bool GetCursorPos(out PointStruct p);
    [DllImport("user32.dll")] private static extern IntPtr LoadCursor(IntPtr instance, IntPtr id);
    [DllImport("user32.dll")] private static extern bool SystemParametersInfo(uint action, int param, ref HighContrastInfo info, uint winIni);
    [DllImport("user32.dll")] private static extern IntPtr GetDC(IntPtr h);
    [DllImport("user32.dll")] private static extern int ReleaseDC(IntPtr h, IntPtr dc);
    [DllImport("user32.dll")] private static extern bool UpdateLayeredWindow(IntPtr h, IntPtr dst, IntPtr pos, ref SizeStruct size, IntPtr src, ref PointStruct srcPos, int key, ref BlendFunction blend, int flags);
    [DllImport("gdi32.dll")] private static extern IntPtr CreateCompatibleDC(IntPtr dc);
    [DllImport("gdi32.dll")] private static extern bool DeleteDC(IntPtr dc);
    [DllImport("gdi32.dll")] private static extern IntPtr SelectObject(IntPtr dc, IntPtr obj);
    [DllImport("gdi32.dll")] private static extern bool DeleteObject(IntPtr obj);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] private static extern IntPtr GetModuleHandle(string? name);
}
