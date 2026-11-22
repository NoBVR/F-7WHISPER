using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Threading;

namespace F7Whisper;

internal sealed class MouseHotkeys : IDisposable
{
    internal readonly record struct Edge(Chord Chord, int RegistrationId, bool Down);

    private const int WhMouseLl = 14;
    private const int WmXButtonDown = 0x020B;
    private const int WmXButtonUp = 0x020C;
    private const uint LlmhfInjected = 1;

    private readonly nint _window;
    private readonly HookProc _callback;
    private readonly Thread _thread;
    private readonly ManualResetEventSlim _started = new(false);
    private readonly ManualResetEvent _stop = new(false);
    private readonly ConcurrentQueue<Edge> _events = new();

    // Only the hook thread touches these slots. Each up belongs to the chord
    // captured on down, even when the user has since released a modifier.
    private readonly Edge[] _captured = new Edge[2];

    // The UI publishes a new dictionary. Published dictionaries are never modified.
    private volatile Dictionary<Chord, int> _bindings;
    private bool _installed;
    private bool _disposed;

    private MouseHotkeys(nint window, Dictionary<Chord, int> bindings)
    {
        _window = window;
        _bindings = bindings;
        _callback = OnMouse;
        _thread = new Thread(Run) { IsBackground = true, Name = "F-7WHISPER mouse hotkeys" };
    }

    internal static MouseHotkeys? Start(nint window, Dictionary<Chord, int> bindings)
    {
        var mouse = new MouseHotkeys(window, bindings);
        mouse._thread.Start();
        mouse._started.Wait();
        if (mouse._installed)
            return mouse;

        mouse.Dispose();
        return null;
    }

    internal void SetBindings(Dictionary<Chord, int> bindings) => _bindings = bindings;

    internal bool TryRead(out Edge edge) => _events.TryDequeue(out edge);

    private static uint Modifiers()
    {
        uint modifiers = 0;
        if (Native.KeyDown(0x12)) modifiers |= 1; // Alt
        if (Native.KeyDown(0x11)) modifiers |= 2; // Ctrl
        if (Native.KeyDown(0x10)) modifiers |= 4; // Shift
        if (Native.KeyDown(0x5B) || Native.KeyDown(0x5C)) modifiers |= 8;
        return modifiers;
    }

    private void Post(Edge edge)
    {
        _events.Enqueue(edge);
        // The UI timer also drains the queue if the window message queue was full.
        Native.PostMessageW(_window, Native.WmMouseEdge, 0, 0);
    }

    private nint OnMouse(int code, nint wParam, nint lParam)
    {
        int message = (int)wParam;
        if (code == 0 && message is WmXButtonDown or WmXButtonUp)
        {
            var input = Marshal.PtrToStructure<MouseHookInput>(lParam);
            uint button = input.MouseData >> 16;
            if ((input.Flags & LlmhfInjected) == 0 && button is 1 or 2)
            {
                int index = (int)button - 1;
                var captured = _captured[index];

                if (message == WmXButtonDown)
                {
                    if (captured.RegistrationId != 0)
                        return 1;

                    var chord = new Chord(5 + index, Modifiers());
                    if (_bindings.TryGetValue(chord, out int id))
                    {
                        var edge = new Edge(chord, id, true);
                        _captured[index] = edge;
                        Post(edge);
                        return 1;
                    }
                }
                else if (captured.RegistrationId != 0)
                {
                    _captured[index] = default;
                    Post(captured with { Down = false });
                    return 1;
                }
            }
        }

        return CallNextHookEx(0, code, wParam, lParam);
    }

    private void Run()
    {
        nint hook = SetWindowsHookExW(WhMouseLl, _callback, GetModuleHandleW(null), 0);
        _installed = hook != 0;
        _started.Set();
        if (!_installed)
            return;

        try
        {
            const uint qsAllInput = 0x04FF;
            const uint mwmoInputAvailable = 0x0004;
            const uint pmRemove = 0x0001;
            nint stop = _stop.SafeWaitHandle.DangerousGetHandle();

            // PeekMessage dispatches sent low-level hook callbacks. The stop event
            // wakes an idle pump without depending on a posted shutdown message.
            while (MsgWaitForMultipleObjectsEx(1, ref stop, uint.MaxValue,
                                               qsAllInput, mwmoInputAvailable) == 1)
            {
                if (PeekMessageW(out var message, 0, 0, 0, pmRemove))
                    DispatchMessageW(ref message);
            }
        }
        finally
        {
            UnhookWindowsHookEx(hook);
        }
    }

    public void Dispose()
    {
        if (_disposed)
            return;

        _disposed = true;
        _stop.Set();
        _thread.Join();
        _stop.Dispose();
        _started.Dispose();
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MouseHookInput
    {
        public int X, Y;
        public uint MouseData, Flags, Time;
        public nuint ExtraInfo;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct ThreadMessage
    {
        public nint Window;
        public uint Message;
        public nuint WParam;
        public nint LParam;
        public uint Time;
        public int X, Y;
        public uint Private;
    }

    [UnmanagedFunctionPointer(CallingConvention.Winapi)]
    private delegate nint HookProc(int code, nint wParam, nint lParam);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern nint GetModuleHandleW(string? moduleName);

    [DllImport("user32.dll", ExactSpelling = true, SetLastError = true)]
    private static extern nint SetWindowsHookExW(int type, HookProc callback, nint module, uint threadId);

    [DllImport("user32.dll")]
    private static extern bool UnhookWindowsHookEx(nint hook);

    [DllImport("user32.dll")]
    private static extern nint CallNextHookEx(nint hook, int code, nint wParam, nint lParam);

    [DllImport("user32.dll")]
    private static extern uint MsgWaitForMultipleObjectsEx(
        uint count, ref nint handles, uint timeout, uint wakeMask, uint flags);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern bool PeekMessageW(
        out ThreadMessage message, nint window, uint first, uint last, uint remove);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern nint DispatchMessageW(ref ThreadMessage message);
}
