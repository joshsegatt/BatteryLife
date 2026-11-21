using BatteryLife.Logic;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace BatteryLife.App;

/// <summary>Builds one device row of the flyout: pictogram, name and state, level and time left.</summary>
public static class RowView
{
    private static T Res<T>(string key) => (T)Application.Current.Resources[key];

    public static UIElement Build(DeviceReading d, Text text)
    {
        var level = text.Level(d);
        var subtitle = text.RowSubtitle(d);
        var time = text.RowTime(d);

        var row = new Grid
        {
            Padding = new Thickness(18, 12, 18, 12),
            ColumnSpacing = 12,
            BorderThickness = new Thickness(0, 1, 0, 0),
            BorderBrush = Res<Brush>("SignRuleBrush"),
            Opacity = d.Asleep ? 0.55 : 1.0,
        };
        AutomationProperties.SetName(row, $"{d.Name}, {level}, {subtitle}{(time.Length > 0 ? ", " + time : "")}");
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(36) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        row.Children.Add(new Border
        {
            Width = 36,
            Height = 36,
            Background = Res<Brush>("SignRaisedBrush"),
            Child = Icon(Pictograms.For(d.Kind), 18),
        });

        var names = new StackPanel { VerticalAlignment = VerticalAlignment.Center, Spacing = 2 };
        names.Children.Add(Line(d.Name, 14, FontWeights.SemiBold, "SignTextBrush"));
        names.Children.Add(Line(subtitle, 12, FontWeights.Normal, "SignMutedBrush"));
        Grid.SetColumn(names, 1);
        row.Children.Add(names);

        var levelLine = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right, Spacing = 4 };
        if (d.Charging) levelLine.Children.Add(Icon("", 14));
        var levelText = Line(level, 22, FontWeights.SemiBold, "SignTextBrush");
        levelText.FontFamily = Res<FontFamily>("SignDisplayFont");
        levelLine.Children.Add(levelText);
        var right = new StackPanel { VerticalAlignment = VerticalAlignment.Center, HorizontalAlignment = HorizontalAlignment.Right };
        right.Children.Add(levelLine);
        if (time.Length > 0)
        {
            var t = Line(time, 11.5, FontWeights.Normal, "SignMutedBrush");
            t.HorizontalAlignment = HorizontalAlignment.Right;
            right.Children.Add(t);
        }
        Grid.SetColumn(right, 2);
        row.Children.Add(right);
        return row;
    }

    private static TextBlock Line(string s, double size, Windows.UI.Text.FontWeight weight, string brush) => new()
    {
        Text = s,
        FontSize = size,
        FontWeight = weight,
        Foreground = Res<Brush>(brush),
        TextTrimming = TextTrimming.CharacterEllipsis,
    };

    private static FontIcon Icon(string glyph, double size) => new()
    {
        Glyph = glyph,
        FontSize = size,
        FontFamily = Res<FontFamily>("PictogramFont"),
        Foreground = Res<Brush>("SignTextBrush"),
    };
}

/// <summary>Segoe Fluent Icons glyphs, white on a black square like airport pictograms.</summary>
public static class Pictograms
{
    public static string For(string kind) => kind switch
    {
        "mouse" => "",
        "headset" => "",
        "keyboard" => "",
        "gamepad" => "",
        "phone" => "",
        _ => "",
    };
}
