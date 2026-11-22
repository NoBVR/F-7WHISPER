using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Security;
using System.Text.Json;

namespace F7Whisper;

internal static class JsonFields
{
    internal static JsonElement Field(this JsonElement obj, string name) =>
        obj.ValueKind == JsonValueKind.Object && obj.TryGetProperty(name, out var value)
            ? value : default;

    internal static string? Text(this JsonElement value) =>
        value.ValueKind == JsonValueKind.String ? value.GetString() : null;

    internal static int? Integer(this JsonElement value) =>
        value.ValueKind == JsonValueKind.Number && value.TryGetInt32(out int number)
            ? number : null;

    internal static bool? Boolean(this JsonElement value) => value.ValueKind switch
    {
        JsonValueKind.True => true,
        JsonValueKind.False => false,
        _ => null
    };
}

internal sealed record KeyChoice(string Label, string Code, int Vk)
{
    internal static KeyChoice? Read(JsonElement value)
    {
        string? label = value.Field("label").Text();
        string? code = value.Field("code").Text();
        if (string.IsNullOrEmpty(label) || string.IsNullOrWhiteSpace(code) ||
            value.Field("vk").Integer() is not int vk || vk is <= 0 or >= 255)
            return null;

        if ((code == "MouseBack" && vk != 5) || (code == "MouseForward" && vk != 6))
            return null;

        if (code.StartsWith("Vk", StringComparison.Ordinal) &&
            code != "Vk" + vk.ToString(CultureInfo.InvariantCulture))
            return null;

        return new KeyChoice(label, code, vk);
    }
}

// Windows identifies a shortcut by its VK and modifier mask.
internal readonly record struct Chord(int Vk, uint Modifiers)
{
    internal bool IsMouse => Vk is 5 or 6;
}

internal sealed record Hotkey(string Label, string Code, int Vk,
                              bool Ctrl, bool Alt, bool Shift, bool Meta)
{
    internal uint Modifiers =>
        (Alt ? 1u : 0u) | (Ctrl ? 2u : 0u) | (Shift ? 4u : 0u) | (Meta ? 8u : 0u);

    internal Chord Chord => new(Vk, Modifiers);

    internal static Hotkey? Read(JsonElement value)
    {
        if (KeyChoice.Read(value) is not { } key ||
            value.Field("ctrl").Boolean() is not bool ctrl ||
            value.Field("alt").Boolean() is not bool alt ||
            value.Field("shift").Boolean() is not bool shift ||
            value.Field("meta").Boolean() is not bool meta)
            return null;

        return new Hotkey(key.Label, key.Code, key.Vk, ctrl, alt, shift, meta);
    }
}

internal sealed record HotkeyPair(Hotkey? Start, Hotkey? Stop)
{
    internal static readonly HotkeyPair Defaults = new(
        new Hotkey("F6", "F6", 117, false, false, false, false),
        new Hotkey("F7", "F7", 118, false, false, false, false));

    internal static HotkeyPair? Read(JsonElement value, bool fromFile)
    {
        if (value.ValueKind != JsonValueKind.Object)
            return fromFile ? Defaults : null;

        var startField = value.Field("start");
        var stopField = value.Field("stop");
        var start = Hotkey.Read(startField);
        var stop = Hotkey.Read(stopField);

        if (fromFile)
        {
            if (startField.ValueKind == JsonValueKind.Undefined) start = Defaults.Start;
            if (stopField.ValueKind == JsonValueKind.Undefined) stop = Defaults.Stop;
        }
        else if ((start is null && startField.ValueKind != JsonValueKind.Null) ||
                 (stop is null && stopField.ValueKind != JsonValueKind.Null))
        {
            return null;
        }

        if (start is not null && stop is not null && start.Chord == stop.Chord)
            start = null;

        return new HotkeyPair(start, stop);
    }

    internal bool Uses(Chord chord) => Start?.Chord == chord || Stop?.Chord == chord;
}

internal sealed record Trigger(Hotkey Hotkey, string Mode)
{
    internal bool Hold => Mode == "hold";

    internal static bool TryRead(JsonElement value, out Trigger? trigger)
    {
        trigger = null;
        if (value.ValueKind is JsonValueKind.Undefined or JsonValueKind.Null)
            return true;

        string? mode = value.Field("mode").Text();
        if (mode is not ("hold" or "toggle") || Hotkey.Read(value.Field("hotkey")) is not { } hotkey)
            return false;

        trigger = new Trigger(hotkey, mode);
        return true;
    }
}

internal sealed record ScreenPoint(int X, int Y);
internal sealed record SavedWindow(int X, int Y);

internal sealed record RowSpec(string Id, string Kind, string? Label, string? Code, int? Vk,
                               string? Button, ScreenPoint? At, string? App, int IntervalMs,
                               Trigger? Trigger)
{
    internal static RowSpec? Read(JsonElement input, string id, bool fromFile)
    {
        var intervalField = input.Field("intervalMs");
        if (intervalField.ValueKind != JsonValueKind.Number ||
            !intervalField.TryGetInt64(out long milliseconds))
            return null;

        // A saved row keeps its keys or clicks if only its trigger is unreadable.
        if (!Trigger.TryRead(input.Field("trigger"), out var trigger) && !fromFile)
            return null;

        int interval = (int)Math.Clamp(milliseconds, 20L, 86_400_000L);
        var appField = input.Field("app");
        string? app = null;
        if (appField.ValueKind == JsonValueKind.String)
        {
            app = appField.GetString()!.Trim();
            if (app.Length > 260) return null;
            if (app.Length == 0) app = null;
        }
        else if (appField.ValueKind != JsonValueKind.Null &&
                 !(fromFile && appField.ValueKind == JsonValueKind.Undefined))
        {
            return null;
        }

        var kindField = input.Field("kind");
        string? kind = fromFile && kindField.ValueKind == JsonValueKind.Undefined
            ? "key" : kindField.Text();

        if (kind == "key")
        {
            var key = KeyChoice.Read(fromFile ? input : input.Field("key"));
            return key is null ? null : new RowSpec(
                id, kind, key.Label, key.Code, key.Vk, null, null, app, interval, trigger);
        }

        string? button = input.Field("button").Text();
        if (kind != "click" || button is not ("left" or "right" or "middle" or "back" or "forward"))
            return null;

        var at = input.Field("at");
        ScreenPoint? point = null;
        if (at.ValueKind == JsonValueKind.Object)
        {
            if (at.Field("x").Integer() is not int x || x is < -100000 or > 100000 ||
                at.Field("y").Integer() is not int y || y is < -100000 or > 100000)
                return null;
            point = new ScreenPoint(x, y);
        }
        else if (at.ValueKind != JsonValueKind.Null &&
                 !(fromFile && at.ValueKind == JsonValueKind.Undefined))
        {
            return null;
        }

        return new RowSpec(id, kind, null, null, null, button, point, app, interval, trigger);
    }
}

internal sealed record Settings(RowSpec[] Rows, bool Pinned, SavedWindow? Window,
                                HotkeyPair Hotkeys, bool SwitchStop)
{
    internal static readonly JsonSerializerOptions Json = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase
    };

    private static readonly string Folder = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "F-7WHISPER");
    private static readonly string FileName = Path.Combine(Folder, "settings.json");

    internal static Settings Load()
    {
        try
        {
            using var stream = new FileStream(FileName, FileMode.Open, FileAccess.Read,
                                              FileShare.Read | FileShare.Delete);
            using var document = JsonDocument.Parse(stream);
            var root = document.RootElement;
            var rows = new List<RowSpec>();
            var seen = new HashSet<string>(StringComparer.Ordinal);
            var items = root.Field("rows");
            if (items.ValueKind == JsonValueKind.Array)
            {
                foreach (var item in items.EnumerateArray())
                {
                    string? id = item.Field("id").Text();
                    if (!string.IsNullOrWhiteSpace(id) &&
                        RowSpec.Read(item, id, fromFile: true) is { } row && seen.Add(id))
                        rows.Add(row);
                }
            }

            var window = root.Field("window");
            var position = window.Field("x").Integer() is int x &&
                           window.Field("y").Integer() is int y ? new SavedWindow(x, y) : null;

            return new Settings(rows.ToArray(), root.Field("pinned").Boolean() ?? false,
                position, HotkeyPair.Read(root.Field("hotkeys"), fromFile: true)!,
                root.Field("switchStop").Boolean() ?? true);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException
                                     or SecurityException or JsonException or InvalidOperationException)
        {
            Debug.WriteLine(ex.Message);
            return new Settings([], false, null, HotkeyPair.Defaults, true);
        }
    }

    internal void Save()
    {
        string temporary = Path.Combine(Folder, $"settings.{Environment.ProcessId}.tmp");
        try
        {
            Directory.CreateDirectory(Folder);
            using (var stream = new FileStream(temporary, FileMode.Create, FileAccess.Write,
                                               FileShare.None))
            {
                JsonSerializer.Serialize(stream, this, Json);
                stream.Flush(flushToDisk: true);
            }
            File.Move(temporary, FileName, overwrite: true);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or SecurityException)
        {
            Debug.WriteLine(ex.Message);
            try { File.Delete(temporary); }
            catch (Exception cleanup) when (cleanup is IOException or UnauthorizedAccessException
                                                       or SecurityException)
            {
                Debug.WriteLine(cleanup.Message);
            }
        }
    }
}
