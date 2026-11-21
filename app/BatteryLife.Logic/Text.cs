using System.Globalization;

namespace BatteryLife.Logic;

/// <summary>UI copy in Brazilian Portuguese and English, picked from the Windows display language.</summary>
public sealed class Text
{
    public bool Portuguese { get; }

    private Text(bool portuguese) => Portuguese = portuguese;

    public static Text For(CultureInfo culture) => new(culture.TwoLetterISOLanguageName == "pt");

    private string T(string pt, string en) => Portuguese ? pt : en;

    /// <summary>"48 min", "2 h 10 min", "3 dias".</summary>
    public string Duration(int minutes)
    {
        if (minutes < 60) return $"{Math.Max(minutes, 1)} min";
        if (minutes < 24 * 60)
        {
            int h = minutes / 60, m = minutes % 60;
            return m == 0 ? $"{h} h" : $"{h} h {m} min";
        }
        int days = (int)Math.Round(minutes / (24 * 60.0));
        return days == 1 ? T("1 dia", "1 day") : T($"{days} dias", $"{days} days");
    }

    public string KindName(string kind) => kind switch
    {
        "mouse" => "Mouse",
        "headset" => "Headset",
        "keyboard" => T("Teclado", "Keyboard"),
        "gamepad" => T("Controle", "Controller"),
        "phone" => T("Celular", "Phone"),
        _ => T("Aparelho", "Device"),
    };

    /// <summary>The kind name used inside a sentence: "Carregue o headset".</summary>
    private string KindInSentence(string kind) => kind switch
    {
        "mouse" => T("o mouse", "the mouse"),
        "headset" => T("o headset", "the headset"),
        "keyboard" => T("o teclado", "the keyboard"),
        "gamepad" => T("o controle", "the controller"),
        "phone" => T("o celular", "the phone"),
        _ => T("o aparelho", "the device"),
    };

    /// <summary>The band's instruction line.</summary>
    public string Headline(Board b) => b.State switch
    {
        BoardState.NoDevices => T("Nenhum aparelho sem fio encontrado", "No wireless devices found"),
        BoardState.Critical => T($"Carregue {KindInSentence(b.Lead!.Kind)} agora", $"Charge {KindInSentence(b.Lead!.Kind)} now"),
        BoardState.Action => T($"Carregue {KindInSentence(b.Lead!.Kind)} antes da próxima partida",
                               $"Charge {KindInSentence(b.Lead!.Kind)} before your next match"),
        _ => T("Nada para carregar agora", "Nothing to charge right now"),
    };

    /// <summary>The band's monumental figure: time left when estimated, otherwise the level.</summary>
    public string Figure(Board b) => b.Lead switch
    {
        null => b.State == BoardState.NoDevices ? "" : T("Tudo certo", "All good"),
        { MinutesLeft: int m } => Duration(m),
        { Percent: int p } => $"{p}%",
        _ => "",
    };

    /// <summary>The band's small line under the figure.</summary>
    public string Detail(Board b)
    {
        if (b.State == BoardState.NoDevices)
            return T("Ligue um mouse, headset ou teclado sem fio", "Turn on a wireless mouse, headset or keyboard");
        if (b.Lead is not { } d) return T("Nenhum aparelho descarregando", "Nothing is discharging");
        var parts = new List<string> { d.Name };
        if (d.Percent is int p) parts.Add($"{p}%");
        if (b.State == BoardState.AllGood)
            return d.MinutesLeft is null
                ? T($"Próximo a carregar: {string.Join(" · ", parts)}", $"Next to charge: {string.Join(" · ", parts)}")
                // The figure is a time: say what it counts down to.
                : T($"até descarregar {KindInSentence(d.Kind)} · {string.Join(" · ", parts)}",
                    $"until {KindInSentence(d.Kind)} runs out · {string.Join(" · ", parts)}");
        if (d.MinutesLeft is not null) parts.Add(T("até desligar", "until it shuts off"));
        return string.Join(" · ", parts);
    }

    /// <summary>Row's second line: "Mouse · Logitech receiver", "Teclado · carregando".</summary>
    public string RowSubtitle(DeviceReading d)
    {
        string state = d switch
        {
            { Full: true } => T("carregado", "charged"),
            { Charging: true } => T("carregando", "charging"),
            { Asleep: true } => T("dormindo · último valor", "asleep · last value"),
            _ => d.Via,
        };
        return $"{KindName(d.Kind)} · {state}";
    }

    /// <summary>Row's small line under the level: time left, or nothing.</summary>
    public string RowTime(DeviceReading d) => d.MinutesLeft is int m && !d.Charging ? Duration(m) : "";

    public string Level(DeviceReading d) => d.Percent is int p ? $"{p}%" : "—";

    public string AlertFooter(int threshold) => T($"Alerta em {threshold}%", $"Alert at {threshold}%");
    public string Refresh => T("Atualizar", "Refresh");
    public string StartWithWindows => T("Iniciar com o Windows", "Start with Windows");
    public string Exit => T("Sair", "Exit");
    public string Settings => T("Configurações", "Settings");
    public string CoreMissing => T("Não foi possível iniciar a leitura dos aparelhos", "Could not start reading devices");

    public string NotificationTitle(DeviceReading d) => T($"{d.Name} com {d.Percent}%", $"{d.Name} at {d.Percent}%");
    public string NotificationBody(DeviceReading d) => d.MinutesLeft is int m
        ? T($"Cerca de {Duration(m)} até desligar. Hora de carregar.", $"About {Duration(m)} until it shuts off. Time to charge.")
        : T("Hora de carregar.", "Time to charge.");

    /// <summary>Tray tooltip: one line per device.</summary>
    public string Tooltip(IReadOnlyList<DeviceReading> devices) => devices.Count == 0
        ? "BatteryLife"
        : string.Join("\n", devices.Select(d => $"{d.Name}: {Level(d)}{(d.Charging ? " ⚡" : "")}"));
}
