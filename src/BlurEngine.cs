using System.Runtime.InteropServices;

namespace F7Whisper;

// blur_engine.dll: Blur AutoClicker's engine, one run per handle. Handles are never reused.
internal static class BlurEngine
{
    private const string Dll = "blur_engine";

    internal const int KindKey = 0;
    internal const int KindClick = 1;
    internal const int ButtonLeft = 1;
    internal const int ButtonRight = 2;
    internal const int ButtonMiddle = 3;

    // Returns 0 when the run could not be started.
    [DllImport(Dll, EntryPoint = "f7_blur_start", CallingConvention = CallingConvention.Cdecl)]
    internal static extern ulong Start(int kind, ushort vk, int button, int hasAt, int x, int y,
                                       double intervalMs);

    [DllImport(Dll, EntryPoint = "f7_blur_pause", CallingConvention = CallingConvention.Cdecl)]
    internal static extern void Pause(ulong handle, int paused);

    [DllImport(Dll, EntryPoint = "f7_blur_clicks", CallingConvention = CallingConvention.Cdecl)]
    internal static extern long Clicks(ulong handle);

    [DllImport(Dll, EntryPoint = "f7_blur_running", CallingConvention = CallingConvention.Cdecl)]
    internal static extern int Running(ulong handle);

    // Stops, joins and frees the run; returns its final count, or -1 for an unknown handle.
    [DllImport(Dll, EntryPoint = "f7_blur_stop", CallingConvention = CallingConvention.Cdecl)]
    internal static extern long Stop(ulong handle);
}
