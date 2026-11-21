namespace BatteryLife.Logic;

/// <summary>
/// The devices that get their own tray icon next to the BatteryLife icon. A fixed kind order
/// (mouse, headset, keyboard, controller, phone, others) keeps each icon in the same place from
/// one reading to the next, so the user learns where the headset is.
/// </summary>
public static class TrayDevices
{
    /// <summary>More icons than this crowd the taskbar; the rest stay in the hub.</summary>
    public const int Max = 4;

    private static readonly string[] KindOrder = ["mouse", "headset", "keyboard", "gamepad", "phone"];

    public static IReadOnlyList<DeviceReading> Pick(IReadOnlyList<DeviceReading> devices) => devices
        .OrderBy(d => Rank(d.Kind))
        .ThenBy(d => d.Name, StringComparer.OrdinalIgnoreCase)
        .ThenBy(d => d.Key, StringComparer.Ordinal)
        .Take(Max)
        .ToList();

    private static int Rank(string kind)
    {
        int i = Array.IndexOf(KindOrder, kind);
        return i < 0 ? KindOrder.Length : i;
    }
}
