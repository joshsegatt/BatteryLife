namespace BatteryLife.Logic;

/// <summary>
/// Decides when to show a low-battery notification. A device alerts once when a fresh reading
/// (connected, awake, not charging) reaches its threshold, and re-arms only after it charges or
/// climbs more than 10 points above the threshold, so a level hovering at the line does not nag.
/// Semantics follow BatteryHub's alert rules (GPL-3.0).
/// </summary>
public sealed class AlertRules
{
    public const int RearmMargin = 10;
    private readonly HashSet<string> _alerted = new();

    /// <summary>Devices that should notify now. Call once per poll.</summary>
    public IReadOnlyList<DeviceReading> Check(IEnumerable<DeviceReading> devices, int threshold = Board.DefaultThreshold)
    {
        var fire = new List<DeviceReading>();
        foreach (var d in devices)
        {
            if (d.Percent is not int p || d.Asleep) continue; // only fresh, numeric readings count
            if (d.Charging || p > threshold + RearmMargin)
            {
                _alerted.Remove(d.Key);
                continue;
            }
            if (p <= threshold && _alerted.Add(d.Key)) fire.Add(d);
        }
        return fire;
    }
}
