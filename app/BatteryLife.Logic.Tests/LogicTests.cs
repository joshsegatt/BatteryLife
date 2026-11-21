using System.Globalization;
using BatteryLife.Logic;

namespace BatteryLife.Logic.Tests;

public class LogicTests
{
    private static DeviceReading Dev(string key, int? pct, string kind = "mouse", bool charging = false,
        bool asleep = false, int? minutes = null) =>
        new() { Key = key, Name = key, Kind = kind, Percent = pct, Charging = charging, Asleep = asleep, MinutesLeft = minutes };

    [Fact]
    public void Parses_core_line_and_clamps_levels()
    {
        var line = """[{"key":"logi:unit:1","name":"PRO X2","kind":"mouse","via":"Logitech receiver","percent":140,"charging":false,"full":false,"asleep":false,"millivolts":null,"minutes_left":125,"future_field":1}]""";
        var list = CoreProtocol.ParseLine(line)!;
        Assert.Single(list);
        Assert.Equal(100, list[0].Percent);
        Assert.Equal(125, list[0].MinutesLeft);
        Assert.Equal("Logitech receiver", list[0].Via);
    }

    [Theory]
    [InlineData("")]
    [InlineData("not json")]
    [InlineData("{\"key\":\"x\"}")]
    public void Rejects_malformed_lines(string line) => Assert.Null(CoreProtocol.ParseLine(line));

    [Fact]
    public void Empty_list_is_no_devices()
    {
        var b = Board.From(CoreProtocol.ParseLine("[]")!);
        Assert.Equal(BoardState.NoDevices, b.State);
    }

    [Fact]
    public void Lowest_device_below_threshold_leads()
    {
        var b = Board.From([Dev("mouse", 93), Dev("headset", 12, "headset"), Dev("pad", 14, "gamepad")]);
        Assert.Equal(BoardState.Action, b.State);
        Assert.Equal("headset", b.Lead!.Key);
    }

    [Fact]
    public void Time_left_beats_level_when_known()
    {
        var b = Board.From([Dev("a", 10, minutes: 300), Dev("b", 14, minutes: 40)]);
        Assert.Equal("b", b.Lead!.Key);
    }

    [Fact]
    public void Charging_device_never_needs_action()
    {
        var b = Board.From([Dev("kbd", 3, "keyboard", charging: true), Dev("mouse", 80)]);
        Assert.Equal(BoardState.AllGood, b.State);
        Assert.Equal("mouse", b.Lead!.Key);
    }

    [Fact]
    public void Five_percent_is_critical()
    {
        Assert.Equal(BoardState.Critical, Board.From([Dev("m", 5)]).State);
        Assert.Equal(BoardState.Action, Board.From([Dev("m", 6)]).State);
        Assert.Equal(BoardState.AllGood, Board.From([Dev("m", 16)]).State);
    }

    [Fact]
    public void Alert_fires_once_and_rearms_after_recovery()
    {
        var rules = new AlertRules();
        Assert.Single(rules.Check([Dev("m", 15)]));
        Assert.Empty(rules.Check([Dev("m", 14)]));          // still low: no repeat
        Assert.Empty(rules.Check([Dev("m", 24)]));          // within the margin: still armed off
        Assert.Empty(rules.Check([Dev("m", 15)]));
        Assert.Empty(rules.Check([Dev("m", 26)]));          // climbed past 25: re-armed
        Assert.Single(rules.Check([Dev("m", 15)]));
    }

    [Fact]
    public void Alert_rearms_on_charging_and_ignores_stale_readings()
    {
        var rules = new AlertRules();
        Assert.Empty(rules.Check([Dev("m", 10, asleep: true)]));
        Assert.Single(rules.Check([Dev("m", 10)]));
        Assert.Empty(rules.Check([Dev("m", 12, charging: true)]));
        Assert.Single(rules.Check([Dev("m", 12)]));
    }

    [Theory]
    [InlineData(0, "1 min")]
    [InlineData(48, "48 min")]
    [InlineData(60, "1 h")]
    [InlineData(130, "2 h 10 min")]
    [InlineData(1440, "1 dia")]
    [InlineData(3700, "3 dias")]
    public void Formats_durations_in_portuguese(int minutes, string expected) =>
        Assert.Equal(expected, Text.For(new CultureInfo("pt-BR")).Duration(minutes));

    [Fact]
    public void Headline_names_the_device_kind()
    {
        var pt = Text.For(new CultureInfo("pt-BR"));
        var en = Text.For(new CultureInfo("en-US"));
        var b = Board.From([Dev("h", 18 - 5, "headset", minutes: 130)]);
        Assert.Equal("Carregue o headset antes de jogar", pt.Headline(b));
        Assert.Equal("Charge the headset before you play", en.Headline(b));
        Assert.Equal("2 h 10 min", pt.Figure(b));
    }

    [Fact]
    public void Calm_band_says_what_the_time_counts_down_to()
    {
        var pt = Text.For(new CultureInfo("pt-BR"));
        var b = Board.From([Dev("Cloud III", 64, "headset", minutes: 1260), Dev("mouse", 92, minutes: 3660)]);
        Assert.Equal(BoardState.AllGood, b.State);
        Assert.Equal("Nada para carregar agora", pt.Headline(b));
        Assert.Equal("21 h", pt.Figure(b));
        Assert.Equal("Cloud III · 64% · até descarregar", pt.Detail(b));
    }

    [Fact]
    public void Low_row_says_charge_now_unless_charging()
    {
        var pt = Text.For(new CultureInfo("pt-BR"));
        Assert.Equal("Headset · carregar agora", pt.RowSubtitle(Dev("h", 14, "headset")));
        Assert.True(Text.NeedsCharge(Dev("h", 15)));
        Assert.False(Text.NeedsCharge(Dev("h", 14, charging: true)));
        Assert.False(Text.NeedsCharge(Dev("h", 16)));
        Assert.Equal("Headset · carregando", pt.RowSubtitle(Dev("h", 14, "headset", charging: true)));
    }

    [Fact]
    public void Figure_falls_back_to_level_without_estimate()
    {
        var pt = Text.For(new CultureInfo("pt-BR"));
        Assert.Equal("12%", pt.Figure(Board.From([Dev("h", 12)])));
    }
}
