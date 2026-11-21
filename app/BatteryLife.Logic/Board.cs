namespace BatteryLife.Logic;

public enum BoardState
{
    /// <summary>No battery device found.</summary>
    NoDevices,
    /// <summary>Nothing needs charging: the band stays black and calm.</summary>
    AllGood,
    /// <summary>A device is at or below its alert threshold: the band turns yellow.</summary>
    Action,
    /// <summary>A device is at 5% or less.</summary>
    Critical,
}

/// <summary>
/// What the hub shows: the single next decision, like an airport sign. Pure logic so the rules
/// are tested without a UI.
/// </summary>
public sealed record Board(BoardState State, DeviceReading? Lead, IReadOnlyList<DeviceReading> Devices)
{
    public const int DefaultThreshold = 15;
    public const int CriticalLevel = 5;

    /// <summary>
    /// Picks the lead device. Needs charging = level known, at or below the threshold, not charging.
    /// Among those (or, when all is good, among all discharging devices) the lead is the one that runs
    /// out first: by estimated time left when known, otherwise by level.
    /// </summary>
    public static Board From(IReadOnlyList<DeviceReading> devices, int threshold = DefaultThreshold)
    {
        if (devices.Count == 0) return new Board(BoardState.NoDevices, null, devices);

        var needs = devices
            .Where(d => d.Percent is int p && p <= threshold && !d.Charging)
            .OrderBy(RunsOutKey)
            .ToList();
        if (needs.Count > 0)
        {
            var lead = needs[0];
            var state = lead.Percent <= CriticalLevel ? BoardState.Critical : BoardState.Action;
            return new Board(state, lead, devices);
        }

        var next = devices
            .Where(d => d.Percent is not null && !d.Charging && !d.Full)
            .OrderBy(RunsOutKey)
            .FirstOrDefault();
        return new Board(BoardState.AllGood, next, devices);
    }

    /// <summary>Sorts by minutes left when known (always before unknown), then by level.</summary>
    private static (int, int, int) RunsOutKey(DeviceReading d) =>
        (d.MinutesLeft is null ? 1 : 0, d.MinutesLeft ?? 0, d.Percent ?? 101);
}
