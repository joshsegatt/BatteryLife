using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Text;
using System.Runtime.InteropServices;
using System.Windows.Input;
using BatteryLife.Logic;
using H.NotifyIcon;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace BatteryLife.App;

/// <summary>
/// One tray icon per main device, beside the BatteryLife icon: the device's pictogram over a level
/// bar, yellow only when it needs charging (the band's rule). A click opens the hub, a right click
/// shows the same menu as the main icon, and hovering names the device and its level.
/// </summary>
/// <param name="menu">Builds the right-click menu; each icon gets its own instance.</param>
public sealed class DeviceTrayIcons(Text text, ICommand toggleHub, Func<MenuFlyout> menu) : IDisposable
{
    private sealed record Slot(TaskbarIcon Tray, Icon Icon, string Look);

    private readonly Dictionary<string, Slot> _slots = [];
    // Kept per device so a device that comes back reuses its menu instead of building another.
    private readonly Dictionary<string, MenuFlyout> _menus = [];
    private IReadOnlyList<DeviceReading> _last = [];

    public void Update(IReadOnlyList<DeviceReading> devices)
    {
        _last = devices;
        var shown = TrayDevices.Pick(devices);
        var keep = shown.Select(d => d.Key).ToHashSet();
        foreach (var key in _slots.Keys.Where(k => !keep.Contains(k)).ToList()) Remove(key);

        int size = TrayIconSize();
        foreach (var d in shown)
        {
            try
            {
                Show(d, size);
            }
            catch (Exception e)
            {
                // One icon that cannot be drawn or added (shell not ready yet, GDI limits) must not
                // take down the hub and the main icon; the next reading tries again.
                System.Diagnostics.Debug.WriteLine($"device tray icon {d.Key}: {e.Message}");
            }
        }
    }

    private void Show(DeviceReading d, int size)
    {
        bool action = Text.NeedsCharge(d);
        // Redraw only when what the icon shows changes, not on every poll.
        string look = $"{size}|{d.Kind}|{d.Percent}|{action}|{d.Asleep}|{Palette.HighContrast}";
        string tip = text.DeviceLine(d);
        if (_slots.TryGetValue(d.Key, out var slot))
        {
            slot.Tray.ToolTipText = tip;
            if (slot.Look == look) return;
            var icon = Paint(d, action, size);
            slot.Tray.Icon = icon;
            slot.Icon.Dispose(); // the shell keeps its own copy of the previous icon
            _slots[d.Key] = slot with { Icon = icon, Look = look };
        }
        else
        {
            var icon = Paint(d, action, size);
            var tray = new TaskbarIcon
            {
                // Stable per device and install path, so Windows remembers where the user put it.
                Id = H.NotifyIcon.Core.TrayIcon.CreateUniqueGuidFromString($"{Environment.ProcessPath}|{d.Key}"),
                ToolTipText = tip,
                NoLeftClickDelay = true,
                LeftClickCommand = toggleHub,
                ContextMenuMode = ContextMenuMode.PopupMenu,
                ContextFlyout = MenuFor(d.Key),
                Icon = icon,
            };
            tray.ForceCreate(enablesEfficiencyMode: false);
            _slots[d.Key] = new Slot(tray, icon, look);
        }
    }

    private MenuFlyout MenuFor(string key)
    {
        if (!_menus.TryGetValue(key, out var m)) _menus[key] = m = menu();
        return m;
    }

    /// <summary>A contrast theme or a display scale change: draw every icon again.</summary>
    public void Repaint()
    {
        foreach (var key in _slots.Keys.ToList()) _slots[key] = _slots[key] with { Look = "" };
        Update(_last);
    }

    public void Dispose()
    {
        foreach (var key in _slots.Keys.ToList()) Remove(key);
    }

    private void Remove(string key)
    {
        var slot = _slots[key];
        slot.Tray.Dispose();
        slot.Icon.Dispose();
        _slots.Remove(key);
    }

    /// <summary>Pictogram on top, level bar at the bottom, on the same tile as the main tray icon
    /// (corner radius 10% of its size, see DESIGN.md).</summary>
    private static Icon Paint(DeviceReading d, bool action, int size)
    {
        var background = Color(action ? "SignYellowBrush" : "SignRaisedBrush");
        var ink = Color(action ? "SignInkBrush" : "SignTextBrush");
        // A sleeping device shows its last value, dimmed, like its row in the hub.
        if (d.Asleep && !action) ink = System.Drawing.Color.FromArgb(140, ink);

        using var bitmap = new Bitmap(size, size);
        using (var g = Graphics.FromImage(bitmap))
        {
            g.SmoothingMode = SmoothingMode.AntiAlias;
            g.TextRenderingHint = TextRenderingHint.AntiAliasGridFit;
            g.Clear(System.Drawing.Color.Transparent);

            using (var square = RoundedSquare(size, size * 0.1f))
            using (var fill = new SolidBrush(background))
                g.FillPath(fill, square);

            float barHeight = Math.Max(2, MathF.Round(size / 8f));
            float inset = MathF.Round(size * 0.19f);
            float barTop = size - inset * 0.8f - barHeight;
            using (var font = PictogramFont(size * 0.56f))
            using (var brush = new SolidBrush(ink))
            using (var centered = new StringFormat { Alignment = StringAlignment.Center, LineAlignment = StringAlignment.Center })
                g.DrawString(Pictograms.For(d.Kind), font, brush, new RectangleF(0, 0, size, barTop), centered);

            float barWidth = size - 2 * inset;
            using (var track = new SolidBrush(System.Drawing.Color.FromArgb(ink.A * 35 / 100, ink)))
                g.FillRectangle(track, inset, barTop, barWidth, barHeight);
            if (d.Percent is int p && p > 0)
            {
                using var level = new SolidBrush(ink);
                g.FillRectangle(level, inset, barTop, Math.Max(1, barWidth * p / 100f), barHeight);
            }
        }

        IntPtr handle = bitmap.GetHicon();
        try
        {
            using var borrowed = Icon.FromHandle(handle);
            return (Icon)borrowed.Clone(); // owns its own handle, so it can be disposed safely
        }
        finally
        {
            DestroyIcon(handle);
        }
    }

    private static GraphicsPath RoundedSquare(int size, float radius)
    {
        float d = radius * 2;
        var path = new GraphicsPath();
        path.AddArc(0, 0, d, d, 180, 90);
        path.AddArc(size - d - 1, 0, d, d, 270, 90);
        path.AddArc(size - d - 1, size - d - 1, d, d, 0, 90);
        path.AddArc(0, size - d - 1, d, d, 90, 90);
        path.CloseFigure();
        return path;
    }

    /// <summary>Segoe Fluent Icons on Windows 11; Windows 10 has the same glyphs in Segoe MDL2 Assets.</summary>
    private static Font PictogramFont(float pixels) => new(PictogramFamily.Value, pixels, GraphicsUnit.Pixel);

    private static readonly Lazy<string> PictogramFamily = new(() =>
    {
        using var installed = new InstalledFontCollection();
        return installed.Families.Any(f => f.Name == "Segoe Fluent Icons") ? "Segoe Fluent Icons" : "Segoe MDL2 Assets";
    });

    private static System.Drawing.Color Color(string brushKey)
    {
        var c = ((SolidColorBrush)Palette.Brush(brushKey)).Color;
        return System.Drawing.Color.FromArgb(c.A, c.R, c.G, c.B);
    }

    /// <summary>The tray's icon size in pixels at the current display scale (16 at 100%).</summary>
    private static int TrayIconSize()
    {
        int size = GetSystemMetricsForDpi(SmCxSmIcon, GetDpiForSystem());
        return size > 0 ? size : 16;
    }

    private const int SmCxSmIcon = 49;

    [DllImport("user32.dll")]
    private static extern uint GetDpiForSystem();

    [DllImport("user32.dll")]
    private static extern int GetSystemMetricsForDpi(int index, uint dpi);

    [DllImport("user32.dll")]
    private static extern bool DestroyIcon(IntPtr handle);
}
