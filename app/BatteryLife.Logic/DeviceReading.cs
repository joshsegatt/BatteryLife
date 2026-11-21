using System.Text.Json;
using System.Text.Json.Serialization;

namespace BatteryLife.Logic;

/// <summary>One device as the core reports it (one element of a --watch JSON line).</summary>
public sealed record DeviceReading
{
    public required string Key { get; init; }
    public required string Name { get; init; }
    public string Kind { get; init; } = "other";
    public string Via { get; init; } = "";
    /// <summary>0..100, or null when unknown right now.</summary>
    public int? Percent { get; init; }
    public bool Charging { get; init; }
    public bool Full { get; init; }
    /// <summary>Last known value; the device is not answering right now.</summary>
    public bool Asleep { get; init; }
    public int? Millivolts { get; init; }
    public int? MinutesLeft { get; init; }
}

/// <summary>Parses the core's --watch output: one JSON array of devices per line.</summary>
public static class CoreProtocol
{
    private static readonly JsonSerializerOptions Options = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        UnmappedMemberHandling = JsonUnmappedMemberHandling.Skip,
    };

    /// <summary>The devices on one line, or null when the line is not a valid device array.</summary>
    public static IReadOnlyList<DeviceReading>? ParseLine(string? line)
    {
        if (string.IsNullOrWhiteSpace(line)) return null;
        try
        {
            var list = JsonSerializer.Deserialize<List<DeviceReading>>(line, Options);
            if (list is null) return null;
            // Clamp values from a newer or buggy core instead of trusting them blindly.
            return list
                .Where(d => !string.IsNullOrEmpty(d.Key))
                .Select(d => d with { Percent = d.Percent is int p ? Math.Clamp(p, 0, 100) : null })
                .ToList();
        }
        catch (JsonException)
        {
            return null;
        }
    }
}
