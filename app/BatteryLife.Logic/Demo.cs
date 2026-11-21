namespace BatteryLife.Logic;

/// <summary>Synthetic devices for `BatteryLife --demo` (screenshots, design review). Not real readings.</summary>
public static class Demo
{
    public static IReadOnlyList<DeviceReading> Devices(bool calm = false) =>
    [
        new() { Key = "demo:headset", Name = "Cloud III Wireless", Kind = "headset", Via = "2.4 GHz", Percent = calm ? 64 : 14, MinutesLeft = calm ? 1260 : 130 },
        new() { Key = "demo:mouse", Name = "PRO X2 SUPERSTRIKE", Kind = "mouse", Via = "Logitech receiver", Percent = 92, MinutesLeft = 3660 },
        new() { Key = "demo:keyboard", Name = "Keychron K2 HE", Kind = "keyboard", Via = "Bluetooth", Percent = 64, Charging = true },
        new() { Key = "demo:phone", Name = "X9 Ultra", Kind = "phone", Via = "Phone Link", Percent = 84 },
        new() { Key = "demo:pad", Name = "Xbox Wireless Controller", Kind = "gamepad", Via = "Xbox adapter", Percent = 71, Asleep = true },
    ];
}
