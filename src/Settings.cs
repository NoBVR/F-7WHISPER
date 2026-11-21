using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Security;
using System.Text.Json;

namespace F7Whisper;

internal sealed record KeyChoice(string Label, string Code, int Vk)
{
    internal static bool IsValid(string? label, string? code, int vk) =>
        !string.IsNullOrEmpty(label) && !string.IsNullOrWhiteSpace(code) && vk is > 0 and < 255;
}

internal sealed record RowSpec(string Id, string Label, string Code, int Vk, int IntervalMs);
internal sealed record SavedWindow(int X, int Y, int Width, int Height, bool Maximized);

internal sealed record Settings(RowSpec[] Rows, bool Pinned, SavedWindow? Window)
{
    internal static readonly JsonSerializerOptions Json = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase
    };

    private static readonly string Folder = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "F-7WHISPER");
    private static readonly string FileName = Path.Combine(Folder, "settings.json");

    internal static int ClampInterval(long ms) => (int)Math.Clamp(ms, 20L, 86_400_000L);

    internal static Settings Load()
    {
        try
        {
            using var stream = File.OpenRead(FileName);
            if (JsonSerializer.Deserialize<Settings>(stream, Json) is { Rows: not null } settings)
            {
                var seen = new HashSet<string>(StringComparer.Ordinal);
                var rows = settings.Rows
                    .Where(row => row is not null && !string.IsNullOrWhiteSpace(row.Id) &&
                                  KeyChoice.IsValid(row.Label, row.Code, row.Vk) && seen.Add(row.Id))
                    .Select(row => row with { IntervalMs = ClampInterval(row.IntervalMs) })
                    .ToArray();
                return settings with { Rows = rows };
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException
                                     or SecurityException or JsonException)
        {
            Debug.WriteLine(ex.Message);
        }

        return new Settings([], false, null);
    }

    internal void Save()
    {
        try
        {
            Directory.CreateDirectory(Folder);
            string temporary = FileName + ".tmp";
            using (var stream = new FileStream(temporary, FileMode.Create, FileAccess.Write,
                                               FileShare.None))
            {
                JsonSerializer.Serialize(stream, this, Json);
                stream.Flush(flushToDisk: true);
            }

            // Both paths are on the same volume. Readers see a complete settings file.
            File.Move(temporary, FileName, overwrite: true);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or SecurityException)
        {
            Debug.WriteLine(ex.Message);
        }
    }
}
