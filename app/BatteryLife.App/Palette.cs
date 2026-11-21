using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Windows.UI.ViewManagement;

namespace BatteryLife.App;

/// <summary>Sign brushes for code-built UI, from the contrast theme when Windows has one on.</summary>
public static class Palette
{
    private static readonly AccessibilitySettings Accessibility = new();

    /// <summary>Raises ColorValuesChanged (on a background thread) when Windows colors change,
    /// contrast themes included. Kept alive here so the event keeps firing.
    /// AccessibilitySettings.HighContrastChanged is not usable from a desktop app.</summary>
    public static readonly UISettings Ui = new();

    public static bool HighContrast => Accessibility.HighContrast;

    public static Brush Brush(string key)
    {
        var themes = Application.Current.Resources.ThemeDictionaries;
        var theme = (ResourceDictionary)themes[HighContrast ? "HighContrast" : "Default"];
        return (Brush)theme[key];
    }

    public static FontFamily Font(string key) => (FontFamily)Application.Current.Resources[key];

    /// <summary>Windows "Animation effects" setting; the sign swaps instantly when it is off.</summary>
    public static bool Animations => new UISettings().AnimationsEnabled;
}
