using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using Microsoft.Win32.SafeHandles;

namespace F7Whisper;

internal static class Native
{
    internal const int WmMove = 0x0003;
    internal const int WmSize = 0x0005;
    internal const int WmGetMinMaxInfo = 0x0024;
    internal const int WmNcCalcSize = 0x0083;
    internal const int WmNcHitTest = 0x0084;
    internal const int WmSysCommand = 0x0112;
    internal const int WmDpiChanged = 0x02E0;
    internal const int WmHotkey = 0x0312;
    internal const uint ModNoRepeat = 0x4000;

    internal static void ApplyFrame(nint hwnd)
    {
        SetDwmValue(hwnd, 20, 1);   // DWMWA_USE_IMMERSIVE_DARK_MODE
        SetDwmValue(hwnd, 33, 2);   // DWMWCP_ROUND
        SetDwmValue(hwnd, 34, 0x002E2E2E);  // DWMWA_BORDER_COLOR: 1 px #2e2e2e (COLORREF is 0x00BBGGRR)

        var margins = new Margins { Top = 1 };
        DwmExtendFrameIntoClientArea(hwnd, ref margins);
    }

    private static void SetDwmValue(nint hwnd, int attribute, int value) =>
        DwmSetWindowAttribute(hwnd, attribute, ref value, sizeof(int));

    internal static void FixTracking(nint address, Size size)
    {
        if (address == 0 || size.Width <= 0 || size.Height <= 0)
            return;

        var info = Marshal.PtrToStructure<MinMaxInfo>(address);
        var point = new WinPoint { X = size.Width, Y = size.Height };
        info.MinTrack = point;
        info.MaxTrack = point;
        info.MaxSize = point;
        Marshal.StructureToPtr(info, address, false);
    }

    internal static void RestoreWindow(nint hwnd) => ShowWindow(hwnd, 9); // SW_RESTORE

    internal static bool LeftButtonDown => GetAsyncKeyState(1) < 0;

    internal static void BeginDrag(nint hwnd)
    {
        if (!LeftButtonDown || !GetCursorPos(out var point))
            return;

        int coords = unchecked((point.X & 0xffff) | ((point.Y & 0xffff) << 16));
        ReleaseCapture();
        SendMessageW(hwnd, 0x00A1, 2, coords); // WM_NCLBUTTONDOWN, HTCAPTION
    }

    private static bool BelongsToHost(nint hwnd, nint owner, uint pid) =>
        pid == (uint)Environment.ProcessId || hwnd == owner ||
        GetAncestor(hwnd, 2) == owner || GetAncestor(hwnd, 3) == owner;

    internal static uint ForegroundPid(nint owner)
    {
        nint hwnd = GetForegroundWindow();
        if (owner == 0 || hwnd == 0 || GetWindowThreadProcessId(hwnd, out uint pid) == 0)
            return 0;

        return BelongsToHost(hwnd, owner, pid) ? (uint)Environment.ProcessId : pid;
    }

    internal static bool TryGetOutsidePoint(nint owner, out ScreenPoint? point)
    {
        point = null;
        if (!GetCursorPos(out var cursor))
            return false;

        nint hwnd = WindowFromPoint(cursor);
        if (hwnd == 0 || GetWindowThreadProcessId(hwnd, out uint pid) == 0 ||
            BelongsToHost(hwnd, owner, pid))
            return false;

        point = new ScreenPoint(cursor.X, cursor.Y);
        return true;
    }

    internal static string? ExecutableName(uint pid)
    {
        const uint queryLimitedInformation = 0x1000;
        using var process = OpenProcess(queryLimitedInformation, false, pid);
        if (process.IsInvalid)
            return null;

        var path = new StringBuilder(512);
        int length = path.Capacity;
        if (!QueryFullProcessImageNameW(process, 0, path, ref length))
        {
            if (Marshal.GetLastWin32Error() != 122) // ERROR_INSUFFICIENT_BUFFER
                return null;

            path = new StringBuilder(32768);
            length = path.Capacity;
            if (!QueryFullProcessImageNameW(process, 0, path, ref length))
                return null;
        }

        return Path.GetFileName(path.ToString());
    }

    internal static string[] ListApps(nint owner)
    {
        var names = new SortedSet<string>(StringComparer.OrdinalIgnoreCase);
        var processes = new HashSet<uint>();
        EnumWindows((hwnd, unused) =>
        {
            if (!IsWindowVisible(hwnd) || GetWindowTextLengthW(hwnd) == 0 ||
                (GetWindowLongW(hwnd, -20) & 0x80) != 0) // WS_EX_TOOLWINDOW
                return true;

            if (DwmGetWindowAttribute(hwnd, 14, out int cloaked, sizeof(int)) == 0 && cloaked != 0)
                return true;

            if (GetWindowThreadProcessId(hwnd, out uint pid) != 0 &&
                !BelongsToHost(hwnd, owner, pid) && processes.Add(pid) &&
                ExecutableName(pid) is { Length: > 0 } name)
                names.Add(name);

            return true;
        }, 0);

        var result = new string[names.Count];
        names.CopyTo(result);
        return result;
    }

    internal readonly record struct Stroke(ushort Vk, ushort Scan, bool Extended, uint MouseDown);

    internal static Stroke MapStroke(RowSpec spec)
    {
        if (spec.Kind == "click")
        {
            uint down = spec.Button switch
            {
                "left" => 0x0002u,
                "right" => 0x0008u,
                "middle" => 0x0020u,
                _ => 0u
            };
            return new Stroke(0, 0, false, down);
        }

        string code = spec.Code!;
        int mappedVk = code switch
        {
            "ShiftLeft" => 0xA0,   "ShiftRight" => 0xA1,
            "ControlLeft" => 0xA2, "ControlRight" => 0xA3,
            "AltLeft" => 0xA4,     "AltRight" => 0xA5,
            "MetaLeft" => 0x5B,    "MetaRight" => 0x5C,
            "NumpadEnter" => 0x0D, "NumpadDivide" => 0x6F,
            "NumpadDecimal" => 0x6E,
            _ when code.Length == 7 &&
                   code.StartsWith("Numpad", StringComparison.Ordinal) &&
                   code[6] is >= '0' and <= '9' => 0x60 + code[6] - '0',
            _ => spec.Vk!.Value
        };

        uint scan = MapVirtualKeyW((uint)mappedVk, 4); // MAPVK_VK_TO_VSC_EX
        bool extended = (scan >> 8) is 0xE0 or 0xE1 ||
            code is "ArrowUp" or "ArrowDown" or "ArrowLeft" or "ArrowRight"
                or "Insert" or "Delete" or "Home" or "End" or "PageUp" or "PageDown"
                or "NumpadEnter" or "NumpadDivide" or "ControlRight" or "AltRight"
                or "MetaLeft" or "MetaRight" or "ContextMenu";

        return new Stroke((ushort)mappedVk, (ushort)(scan & 0xff), extended, 0);
    }

    private static readonly int InputSize = Marshal.SizeOf<Input>();

    private static Input Mouse(uint flags, int dx = 0, int dy = 0) => new()
    {
        Type = 0,
        Data = new InputUnion { Mouse = new MouseInput { Dx = dx, Dy = dy, Flags = flags } }
    };

    private static int Normalize(int coordinate, int origin, int extent)
    {
        long offset = Math.Clamp((long)coordinate - origin, 0, extent - 1L);
        if (offset == 0) return 0;
        if (offset == extent - 1L) return 65535;
        // Aim at the middle of the pixel's bin in the 16-bit absolute coordinate space.
        return (int)(((2 * offset + 1) * 32768) / extent);
    }

    internal static bool SendStroke(Stroke stroke, bool up, ScreenPoint? at = null)
    {
        Input input;
        if (stroke.MouseDown != 0)
        {
            input = Mouse(up ? stroke.MouseDown << 1 : stroke.MouseDown);
            if (!up && at is not null)
            {
                int width = GetSystemMetrics(78);  // SM_CXVIRTUALSCREEN
                int height = GetSystemMetrics(79); // SM_CYVIRTUALSCREEN
                if (width <= 0 || height <= 0)
                    return false;

                int x = Normalize(at.X, GetSystemMetrics(76), width);
                int y = Normalize(at.Y, GetSystemMetrics(77), height);
                var move = Mouse(0x0001 | 0x8000 | 0x4000, x, y);
                // If only the move is inserted, no button-up is owed.
                return SendInputBatch(2, [move, input], InputSize) == 2;
            }
        }
        else
        {
            if (stroke.Scan == 0)
                return false;

            input = new Input
            {
                Type = 1,
                Data = new InputUnion
                {
                    Keyboard = new KeyboardInput
                    {
                        Vk = stroke.Vk,
                        Scan = stroke.Scan,
                        Flags = 0x0008u | (stroke.Extended ? 0x0001u : 0u) | (up ? 0x0002u : 0u)
                    }
                }
            };
        }

        return SendInput(1, ref input, InputSize) == 1;
    }

    internal sealed class SchedulerTimer : WaitHandle
    {
        internal SchedulerTimer()
        {
            const uint highResolution = 0x00000002;
            const uint modifyAndWait = 0x00100002;
            SafeWaitHandle = CreateWaitableTimerExW(0, null, highResolution, modifyAndWait);
        }

        internal bool Arm(long stopwatchTicks)
        {
            if (SafeWaitHandle.IsInvalid)
                return false;

            long due = -Math.Max(1L, (long)Math.Ceiling(
                stopwatchTicks * (10_000_000.0 / Stopwatch.Frequency)));
            return SetWaitableTimer(SafeWaitHandle, ref due, 0, 0, 0, false);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WinPoint { public int X, Y; }

    [StructLayout(LayoutKind.Sequential)]
    private struct Margins { public int Left, Right, Top, Bottom; }

    [StructLayout(LayoutKind.Sequential)]
    private struct MinMaxInfo
    {
        public WinPoint Reserved, MaxSize, MaxPosition, MinTrack, MaxTrack;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct KeyboardInput
    {
        public ushort Vk, Scan;
        public uint Flags, Time;
        public nuint ExtraInfo;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MouseInput
    {
        public int Dx, Dy;
        public uint MouseData, Flags, Time;
        public nuint ExtraInfo;
    }

    [StructLayout(LayoutKind.Explicit)]
    private struct InputUnion
    {
        [FieldOffset(0)] public KeyboardInput Keyboard;
        [FieldOffset(0)] public MouseInput Mouse;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Input
    {
        public uint Type;
        public InputUnion Data;
    }

    private delegate bool EnumWindowsProc(nint hwnd, nint argument);

    [DllImport("user32.dll")]
    internal static extern uint GetDpiForSystem();

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(nint hwnd, int attribute, ref int value, int size);

    [DllImport("dwmapi.dll")]
    private static extern int DwmGetWindowAttribute(nint hwnd, int attribute, out int value, int size);

    [DllImport("dwmapi.dll")]
    private static extern int DwmExtendFrameIntoClientArea(nint hwnd, ref Margins margins);

    [DllImport("user32.dll")]
    private static extern bool ShowWindow(nint hwnd, int command);

    [DllImport("user32.dll")]
    private static extern short GetAsyncKeyState(int vk);

    [DllImport("user32.dll")]
    private static extern bool GetCursorPos(out WinPoint point);

    [DllImport("user32.dll")]
    private static extern nint WindowFromPoint(WinPoint point);

    [DllImport("user32.dll")]
    private static extern bool ReleaseCapture();

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern nint SendMessageW(nint hwnd, uint message, nint wParam, nint lParam);

    [DllImport("user32.dll", SetLastError = true)]
    internal static extern bool RegisterHotKey(nint hwnd, int id, uint modifiers, uint vk);

    [DllImport("user32.dll", SetLastError = true)]
    internal static extern bool UnregisterHotKey(nint hwnd, int id);

    [DllImport("user32.dll")]
    private static extern nint GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern nint GetAncestor(nint hwnd, uint flags);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint hwnd, out uint pid);

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowsProc callback, nint argument);

    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(nint hwnd);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern int GetWindowTextLengthW(nint hwnd);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern int GetWindowLongW(nint hwnd, int index);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern SafeProcessHandle OpenProcess(uint access, bool inheritHandle, uint pid);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, ExactSpelling = true, SetLastError = true)]
    private static extern bool QueryFullProcessImageNameW(
        SafeProcessHandle process, uint flags, StringBuilder path, ref int length);

    [DllImport("user32.dll")]
    private static extern int GetSystemMetrics(int index);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern uint MapVirtualKeyW(uint code, uint mapType);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint SendInput(uint count, ref Input inputs, int size);

    [DllImport("user32.dll", EntryPoint = "SendInput", ExactSpelling = true, SetLastError = true)]
    private static extern uint SendInputBatch(uint count, [In] Input[] inputs, int size);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, ExactSpelling = true, SetLastError = true)]
    private static extern SafeWaitHandle CreateWaitableTimerExW(
        nint attributes, string? name, uint flags, uint desiredAccess);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetWaitableTimer(
        SafeWaitHandle timer, ref long due, int period, nint callback, nint argument, bool resume);

    [DllImport("winmm.dll", EntryPoint = "timeBeginPeriod")]
    internal static extern uint TimeBeginPeriod(uint period);

    [DllImport("winmm.dll", EntryPoint = "timeEndPeriod")]
    internal static extern uint TimeEndPeriod(uint period);
}
