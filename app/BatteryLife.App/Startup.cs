using Microsoft.Win32;

namespace BatteryLife.App;

/// <summary>"Start with Windows": the current user's Run key, no admin rights needed.</summary>
public static class Startup
{
    private const string RunKey = @"Software\Microsoft\Windows\CurrentVersion\Run";
    private const string Name = "BatteryLife";

    private static string Command => $"\"{Environment.ProcessPath}\"";

    public static bool IsEnabled
    {
        get
        {
            using var key = Registry.CurrentUser.OpenSubKey(RunKey);
            return string.Equals(key?.GetValue(Name) as string, Command, StringComparison.OrdinalIgnoreCase);
        }
    }

    public static void Set(bool enabled)
    {
        using var key = Registry.CurrentUser.CreateSubKey(RunKey);
        if (enabled) key.SetValue(Name, Command);
        else key.DeleteValue(Name, throwOnMissingValue: false);
    }

    /// <summary>True only the first time it is asked on this Windows account.</summary>
    public static bool IsFirstRun()
    {
        using var key = Registry.CurrentUser.CreateSubKey(@"Software\BatteryLife");
        if (key.GetValue("FirstRunDone") is not null) return false;
        key.SetValue("FirstRunDone", 1, RegistryValueKind.DWord);
        return true;
    }
}
