// FastPDF bench-app helper. Compiled at runtime by bench-app.ps1 through Add-Type.
// Kept C# 5 compatible so it builds under Windows PowerShell 5.1 and PowerShell 7.
//
// What it does (all scoped to the process tree that bench-app.ps1 launched):
//   * process-tree tracking (Toolhelp snapshot + creation-time guard against PID reuse)
//   * main-window discovery (visible, non-cloaked, largest top-level window of the tree)
//   * client-area capture with PrintWindow(PW_CLIENTONLY | PW_RENDERFULLCONTENT) into a GDI DIB
//   * frame-difference recorder (first non-blank frame, last visual change, quiet-period stability)
//   * input playback with PostMessage to windows of the tree only (wheel / keys, optional Ctrl via
//     AttachThreadInput + SetKeyboardState on the target thread's shared input state)
//   * memory / CPU / thread sampling summed over the tree
//   * minimal PNG writer (no System.Drawing dependency)
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.IO.Compression;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace FastPdfBench
{
    public static class Native
    {
        public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

        [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
        [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
        [StructLayout(LayoutKind.Sequential)]
        public struct MSG { public IntPtr hwnd; public uint message; public IntPtr wParam; public IntPtr lParam; public uint time; public POINT pt; }
        [StructLayout(LayoutKind.Sequential)]
        public struct GUITHREADINFO
        {
            public int cbSize; public int flags; public IntPtr hwndActive; public IntPtr hwndFocus; public IntPtr hwndCapture;
            public IntPtr hwndMenuOwner; public IntPtr hwndMoveSize; public IntPtr hwndCaret; public RECT rcCaret;
        }
        [StructLayout(LayoutKind.Sequential)]
        public struct BITMAPINFOHEADER
        {
            public int biSize; public int biWidth; public int biHeight; public short biPlanes; public short biBitCount;
            public int biCompression; public int biSizeImage; public int biXPelsPerMeter; public int biYPelsPerMeter;
            public int biClrUsed; public int biClrImportant;
        }
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        public struct PROCESSENTRY32W
        {
            public int dwSize; public int cntUsage; public int th32ProcessID; public IntPtr th32DefaultHeapID; public int th32ModuleID;
            public int cntThreads; public int th32ParentProcessID; public int pcPriClassBase; public int dwFlags;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string szExeFile;
        }
        [StructLayout(LayoutKind.Sequential)]
        public struct PROCESS_MEMORY_COUNTERS_EX
        {
            public int cb; public int PageFaultCount; public UIntPtr PeakWorkingSetSize; public UIntPtr WorkingSetSize;
            public UIntPtr QuotaPeakPagedPoolUsage; public UIntPtr QuotaPagedPoolUsage; public UIntPtr QuotaPeakNonPagedPoolUsage;
            public UIntPtr QuotaNonPagedPoolUsage; public UIntPtr PagefileUsage; public UIntPtr PeakPagefileUsage; public UIntPtr PrivateUsage;
        }

        [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr l);
        [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr parent, EnumWindowsProc cb, IntPtr l);
        [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
        [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
        [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
        [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
        [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
        [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder sb, int n);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, StringBuilder sb, int n);
        [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
        [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint msg, IntPtr w, IntPtr l);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr SendMessageW(IntPtr h, uint msg, IntPtr w, string l);
        [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
        [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
        [DllImport("user32.dll")] public static extern bool GetGUIThreadInfo(uint tid, ref GUITHREADINFO info);
        [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
        [DllImport("user32.dll")] public static extern bool GetKeyboardState(byte[] state);
        [DllImport("user32.dll")] public static extern bool SetKeyboardState(byte[] state);
        [DllImport("user32.dll")] public static extern bool PeekMessageW(out MSG msg, IntPtr h, uint min, uint max, uint remove);
        [DllImport("user32.dll")] public static extern uint MapVirtualKeyW(uint code, uint type);
        [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
        [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr dc);
        [DllImport("user32.dll")] public static extern int GetDlgCtrlID(IntPtr h);
        [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
        [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
        [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr ctx);
        [DllImport("gdi32.dll")] public static extern IntPtr CreateCompatibleDC(IntPtr dc);
        [DllImport("gdi32.dll")] public static extern IntPtr CreateDIBSection(IntPtr dc, ref BITMAPINFOHEADER bmi, uint usage, out IntPtr bits, IntPtr section, uint offset);
        [DllImport("gdi32.dll")] public static extern IntPtr SelectObject(IntPtr dc, IntPtr obj);
        [DllImport("gdi32.dll")] public static extern bool DeleteObject(IntPtr obj);
        [DllImport("gdi32.dll")] public static extern bool DeleteDC(IntPtr dc);
        [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out int value, int size);
        [DllImport("kernel32.dll", SetLastError = true)] public static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint pid);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] public static extern bool Process32FirstW(IntPtr snap, ref PROCESSENTRY32W e);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] public static extern bool Process32NextW(IntPtr snap, ref PROCESSENTRY32W e);
        [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
        [DllImport("kernel32.dll")] public static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
        [DllImport("kernel32.dll")] public static extern bool GetProcessTimes(IntPtr h, out long creation, out long exit, out long kernel, out long user);
        [DllImport("kernel32.dll")] public static extern bool GetProcessHandleCount(IntPtr h, out uint count);
        [DllImport("kernel32.dll")] public static extern bool GetExitCodeProcess(IntPtr h, out uint code);
        [DllImport("kernel32.dll")] public static extern bool K32GetProcessMemoryInfo(IntPtr h, out PROCESS_MEMORY_COUNTERS_EX c, int cb);
        [DllImport("kernel32.dll")] public static extern bool TerminateProcess(IntPtr h, uint code);
        [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();

        public const uint PW_CLIENTONLY = 1, PW_RENDERFULLCONTENT = 2;
        public const uint WM_CLOSE = 0x0010, WM_SETTEXT = 0x000C, WM_KEYDOWN = 0x0100, WM_KEYUP = 0x0101;
        public const uint WM_MOUSEMOVE = 0x0200, WM_LBUTTONDOWN = 0x0201, WM_LBUTTONUP = 0x0202, WM_MOUSEWHEEL = 0x020A;
        public const uint BM_CLICK = 0x00F5;
        public const uint PROCESS_TERMINATE = 0x0001, PROCESS_VM_READ = 0x0010, PROCESS_QUERY_LIMITED_INFORMATION = 0x1000;
        public const int DWMWA_CLOAKED = 14;
        public const uint STILL_ACTIVE = 259;

        public static string Text(IntPtr h) { var sb = new StringBuilder(512); GetWindowTextW(h, sb, 512); return sb.ToString(); }
        public static string Class(IntPtr h) { var sb = new StringBuilder(256); GetClassNameW(h, sb, 256); return sb.ToString(); }
        public static int[] ClientSize(IntPtr h) { RECT r; GetClientRect(h, out r); return new int[] { r.Right - r.Left, r.Bottom - r.Top }; }
        public static bool IsCloaked(IntPtr h)
        {
            int v = 0;
            try { if (DwmGetWindowAttribute(h, DWMWA_CLOAKED, out v, 4) != 0) return false; } catch { return false; }
            return v != 0;
        }
        public static void InitDpi()
        {
            try { SetThreadDpiAwarenessContext(new IntPtr(-4)); } catch { }
            try { SetProcessDPIAware(); } catch { }
        }
        public static IntPtr MakeLParam(int lo, int hi) { return new IntPtr(((hi & 0xFFFF) << 16) | (lo & 0xFFFF)); }
    }

    public sealed class ProcInfo { public int Pid; public int Ppid; public string Name; public int Threads; }

    public sealed class TreeSample
    {
        public double T; public int Count; public long WorkingSet; public long Private; public long PeakWorkingSetMax; public long PeakWorkingSetSum;
        public long Cpu100ns; public int Threads; public long Handles; public string Names; public string Unreadable;
    }

    public static class Procs
    {
        public static List<ProcInfo> Snapshot()
        {
            var list = new List<ProcInfo>();
            IntPtr snap = Native.CreateToolhelp32Snapshot(2 /*TH32CS_SNAPPROCESS*/, 0);
            if (snap == IntPtr.Zero || snap == new IntPtr(-1)) return list;
            try
            {
                var e = new Native.PROCESSENTRY32W();
                e.dwSize = Marshal.SizeOf(typeof(Native.PROCESSENTRY32W));
                if (Native.Process32FirstW(snap, ref e))
                {
                    do { list.Add(new ProcInfo { Pid = e.th32ProcessID, Ppid = e.th32ParentProcessID, Name = e.szExeFile, Threads = e.cntThreads }); }
                    while (Native.Process32NextW(snap, ref e));
                }
            }
            finally { Native.CloseHandle(snap); }
            return list;
        }

        /// FILETIME (UTC, 100 ns) of process creation, or -1 when the process cannot be opened.
        public static long CreationTime(int pid)
        {
            IntPtr h = Native.OpenProcess(Native.PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
            if (h == IntPtr.Zero) return -1;
            try { long c, x, k, u; return Native.GetProcessTimes(h, out c, out x, out k, out u) ? c : -1; }
            finally { Native.CloseHandle(h); }
        }

        public static bool IsAlive(int pid, long expectedCreation)
        {
            IntPtr h = Native.OpenProcess(Native.PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
            if (h == IntPtr.Zero) return false;
            try
            {
                long c, x, k, u; uint code;
                if (!Native.GetProcessTimes(h, out c, out x, out k, out u)) return false;
                if (expectedCreation > 0 && c != expectedCreation) return false;
                return Native.GetExitCodeProcess(h, out code) && code == Native.STILL_ACTIVE;
            }
            finally { Native.CloseHandle(h); }
        }
    }

    public sealed class WinInfo { public IntPtr Hwnd; public int Pid; public string Title; public string ClassName; public int ClientW; public int ClientH; public uint Dpi; }

    public sealed class FrameRec { public double T; public double CapMs; public double Diff; }

    public sealed class RecordOptions
    {
        public int StableFrames = 5;      // consecutive unchanged captures required
        public int QuietMs = 1500;        // and at least this long without a visual change
        public int TimeoutMs = 30000;
        public double Tol = 0.0005;       // fraction of sampled pixels that may differ and still count as "unchanged"
        public int PixelThr = 24;         // |dR|+|dG|+|dB| above this marks a sampled pixel as different
        public int IntervalMs = 15;       // minimum time between captures
        public int Step = 4;              // sampling grid (every Nth pixel in x and y)
        public int MinDistinct = 3;       // a frame with fewer distinct colours (on the grid) counts as blank
        public int WaitChangeMs = 3000;   // with a baseline: give up if nothing changes within this time
        public double NotBeforeMs = 0;    // stability cannot be declared before this clock time (inputs still pending)
        public int SampleEveryMs = 250;   // memory sampling period while recording (peak tracking)
    }

    public sealed class RecordResult
    {
        public List<FrameRec> Frames = new List<FrameRec>();
        public double TStart = -1, TFirstNonBlank = -1, TFirstChange = -1, TLastChange = -1, TBeforeLastChange = -1, TStable = -1;
        public bool Stable, TimedOut, WindowLost, NoEffect;
        public int ChangedFrames;
        public double MeanCapMs;
        public int W, H;
        public byte[] FirstNonBlank; public int FnbW, FnbH;
        public byte[] Final; public int FinW, FinH;
        public byte[] FinalSig;
        /// "t:diff" pairs (t relative to t0, ms) for frames that differed from the previous capture.
        public string ChangeTimeline(double t0, double tol)
        {
            var sb = new StringBuilder();
            foreach (var f in Frames)
                if (f.Diff > tol)
                {
                    if (sb.Length > 0) sb.Append(' ');
                    sb.Append((f.T - t0).ToString("0.0", System.Globalization.CultureInfo.InvariantCulture)).Append(':')
                      .Append(f.Diff.ToString("0.####", System.Globalization.CultureInfo.InvariantCulture));
                }
            return sb.ToString();
        }
    }

    public sealed class InputStep { public double AtMs; public IntPtr Hwnd; public uint Msg; public IntPtr WParam; public IntPtr LParam; }

    public sealed class InputPlayback
    {
        public List<double> SentAt = new List<double>();
        public double LastScheduledMs;
        public bool CtrlInjected; public string Error;
        public int Skipped;            // wheel messages not sent because the point was not over our own window
        public bool SafeWheel;         // check WindowFromPoint before every WM_MOUSEWHEEL
        public HashSet<int> OurPids;
        public Thread Worker;
        public void Join() { if (Worker != null) Worker.Join(); }
    }

    /// Capture of a window's client area into a reusable top-down 32-bit DIB.
    public sealed class Capturer : IDisposable
    {
        IntPtr screenDc, memDc, dib, bits, oldObj; int w, h;
        public byte[] Buffer = new byte[0];
        public int Width { get { return w; } }
        public int Height { get { return h; } }

        public bool Grab(IntPtr hwnd)
        {
            int[] cs = Native.ClientSize(hwnd);
            int cw = Math.Max(1, cs[0]), ch = Math.Max(1, cs[1]);
            if (cw != w || ch != h || dib == IntPtr.Zero) Allocate(cw, ch);
            bool ok = Native.PrintWindow(hwnd, memDc, Native.PW_CLIENTONLY | Native.PW_RENDERFULLCONTENT);
            if (Buffer.Length != w * h * 4) Buffer = new byte[w * h * 4];
            Marshal.Copy(bits, Buffer, 0, Buffer.Length);
            return ok;
        }

        void Allocate(int cw, int ch)
        {
            Release();
            w = cw; h = ch;
            screenDc = Native.GetDC(IntPtr.Zero);
            memDc = Native.CreateCompatibleDC(screenDc);
            var bmi = new Native.BITMAPINFOHEADER();
            bmi.biSize = Marshal.SizeOf(typeof(Native.BITMAPINFOHEADER));
            bmi.biWidth = w; bmi.biHeight = -h; bmi.biPlanes = 1; bmi.biBitCount = 32; bmi.biCompression = 0;
            dib = Native.CreateDIBSection(screenDc, ref bmi, 0, out bits, IntPtr.Zero, 0);
            oldObj = Native.SelectObject(memDc, dib);
        }

        void Release()
        {
            if (memDc != IntPtr.Zero) { Native.SelectObject(memDc, oldObj); Native.DeleteDC(memDc); memDc = IntPtr.Zero; }
            if (dib != IntPtr.Zero) { Native.DeleteObject(dib); dib = IntPtr.Zero; }
            if (screenDc != IntPtr.Zero) { Native.ReleaseDC(IntPtr.Zero, screenDc); screenDc = IntPtr.Zero; }
        }

        public void Dispose() { Release(); }
    }

    public static class Img
    {
        public static byte[] Signature(byte[] bgra, int w, int h, int step)
        {
            int sw = (w + step - 1) / step, sh = (h + step - 1) / step;
            var sig = new byte[sw * sh * 3];
            int k = 0;
            for (int y = 0; y < h; y += step)
            {
                int row = y * w * 4;
                for (int x = 0; x < w; x += step)
                {
                    int i = row + x * 4;
                    sig[k++] = bgra[i]; sig[k++] = bgra[i + 1]; sig[k++] = bgra[i + 2];
                }
            }
            return sig;
        }

        public static double Diff(byte[] a, byte[] b, int thr)
        {
            if (a == null || b == null || a.Length != b.Length) return 1.0;
            int n = a.Length / 3, d = 0;
            for (int i = 0; i < a.Length; i += 3)
            {
                int s = Math.Abs(a[i] - b[i]) + Math.Abs(a[i + 1] - b[i + 1]) + Math.Abs(a[i + 2] - b[i + 2]);
                if (s > thr) d++;
            }
            return n == 0 ? 0 : (double)d / n;
        }

        public static int Distinct(byte[] sig, int cap)
        {
            var set = new HashSet<int>();
            for (int i = 0; i < sig.Length; i += 3)
            {
                set.Add((sig[i] << 16) | (sig[i + 1] << 8) | sig[i + 2]);
                if (set.Count >= cap) break;
            }
            return set.Count;
        }

        /// Vertical run of non-background pixels on the centre column (middle half of the image).
        public static int[] CenterRun(byte[] bgra, int w, int h)
        {
            int x = w / 2, y0 = h / 4, y1 = h * 3 / 4;
            int bi = (y0 * w + x) * 4;
            int B = bgra[bi], G = bgra[bi + 1], R = bgra[bi + 2];
            int first = -1, last = -1;
            for (int y = y0; y < y1; y++)
            {
                int i = (y * w + x) * 4;
                int d = Math.Abs(bgra[i] - B) + Math.Abs(bgra[i + 1] - G) + Math.Abs(bgra[i + 2] - R);
                if (d > 30) { if (first < 0) first = y; last = y; }
            }
            return new int[] { x, first, last };
        }

        static uint[] crcTable;
        static uint Crc(byte[] data, uint crc)
        {
            if (crcTable == null)
            {
                crcTable = new uint[256];
                for (uint n = 0; n < 256; n++) { uint c = n; for (int k = 0; k < 8; k++) c = (c & 1) != 0 ? 0xEDB88320u ^ (c >> 1) : c >> 1; crcTable[n] = c; }
            }
            foreach (byte b in data) crc = crcTable[(crc ^ b) & 0xFF] ^ (crc >> 8);
            return crc;
        }
        static void Chunk(Stream s, string type, byte[] data)
        {
            byte[] t = Encoding.ASCII.GetBytes(type);
            byte[] len = { (byte)(data.Length >> 24), (byte)(data.Length >> 16), (byte)(data.Length >> 8), (byte)data.Length };
            s.Write(len, 0, 4); s.Write(t, 0, 4); s.Write(data, 0, data.Length);
            uint crc = Crc(data, Crc(t, 0xFFFFFFFFu)) ^ 0xFFFFFFFFu;
            byte[] c = { (byte)(crc >> 24), (byte)(crc >> 16), (byte)(crc >> 8), (byte)crc };
            s.Write(c, 0, 4);
        }

        /// Write a BGRA (top-down) buffer as an RGB PNG.
        public static void SavePng(string path, byte[] bgra, int w, int h)
        {
            var raw = new byte[(w * 3 + 1) * h];
            int p = 0;
            for (int y = 0; y < h; y++)
            {
                raw[p++] = 0;
                int row = y * w * 4;
                for (int x = 0; x < w; x++) { int i = row + x * 4; raw[p++] = bgra[i + 2]; raw[p++] = bgra[i + 1]; raw[p++] = bgra[i]; }
            }
            uint a = 1, b = 0;
            foreach (byte v in raw) { a = (a + v) % 65521; b = (b + a) % 65521; }
            uint adler = (b << 16) | a;
            byte[] z;
            using (var ms = new MemoryStream())
            {
                ms.WriteByte(0x78); ms.WriteByte(0x9C);
                using (var ds = new DeflateStream(ms, CompressionMode.Compress, true)) ds.Write(raw, 0, raw.Length);
                ms.WriteByte((byte)(adler >> 24)); ms.WriteByte((byte)(adler >> 16)); ms.WriteByte((byte)(adler >> 8)); ms.WriteByte((byte)adler);
                z = ms.ToArray();
            }
            var ihdr = new byte[] { (byte)(w >> 24), (byte)(w >> 16), (byte)(w >> 8), (byte)w, (byte)(h >> 24), (byte)(h >> 16), (byte)(h >> 8), (byte)h, 8, 2, 0, 0, 0 };
            using (var fs = File.Create(path))
            {
                fs.Write(new byte[] { 0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A }, 0, 8);
                Chunk(fs, "IHDR", ihdr); Chunk(fs, "IDAT", z); Chunk(fs, "IEND", new byte[0]);
            }
        }
    }

    /// Collects stdout lines (FASTPDF_BENCH=1 protocol) with host-side receive timestamps.
    public sealed class StdoutCollector
    {
        readonly object gate = new object();
        public readonly List<string> Lines = new List<string>();
        public readonly List<double> Times = new List<double>();
        public void Attach(Process p, Stopwatch clock)
        {
            p.OutputDataReceived += delegate (object s, DataReceivedEventArgs e)
            {
                if (e.Data == null) return;
                lock (gate) { Times.Add(clock.Elapsed.TotalMilliseconds); Lines.Add(e.Data.Length > 4000 ? e.Data.Substring(0, 4000) : e.Data); }
            };
            p.BeginOutputReadLine();
        }
        public int ErrLines; public readonly Queue<string> ErrTail = new Queue<string>();
        /// Drains stderr so the child can never block on a full pipe; keeps the line count and the last 20 lines.
        public void AttachStderr(Process p)
        {
            p.ErrorDataReceived += delegate (object s, DataReceivedEventArgs e)
            {
                if (e.Data == null) return;
                lock (gate) { ErrLines++; ErrTail.Enqueue(e.Data.Length > 300 ? e.Data.Substring(0, 300) : e.Data); while (ErrTail.Count > 20) ErrTail.Dequeue(); }
            };
            p.BeginErrorReadLine();
        }
        /// Drains stdout without keeping it (apps that are not using the FASTPDF_BENCH protocol).
        public void AttachDiscard(Process p)
        {
            p.OutputDataReceived += delegate (object s, DataReceivedEventArgs e) { if (e.Data != null) lock (gate) { DiscardedLines++; } };
            p.BeginOutputReadLine();
        }
        public int DiscardedLines;
        public string[] SnapshotErrTail() { lock (gate) { return ErrTail.ToArray(); } }
        public string[] SnapshotLines() { lock (gate) { return Lines.ToArray(); } }
        public double[] SnapshotTimes() { lock (gate) { return Times.ToArray(); } }
    }

    /// One launched application instance (the root process plus every descendant created after launch).
    public sealed class Session : IDisposable
    {
        public readonly Stopwatch Clock;
        public readonly int RootPid;
        public readonly long LaunchFileTime;
        public readonly Dictionary<int, long> Tracked = new Dictionary<int, long>();
        public readonly Dictionary<int, string> Names = new Dictionary<int, string>();
        public WinInfo Win;
        public long PeakPrivateSum, PeakWsSum;
        public int PeakProcessCount;
        readonly Capturer cap = new Capturer();
        readonly Dictionary<int, IntPtr> handles = new Dictionary<int, IntPtr>(); // held open so exited processes keep their final CPU time
        double lastSample = -1e9;

        public Session(int rootPid, Stopwatch clock, long launchFileTime)
        {
            RootPid = rootPid; Clock = clock; LaunchFileTime = launchFileTime;
            long c = Procs.CreationTime(rootPid);
            Tracked[rootPid] = c; Names[rootPid] = "root";
            EnsureHandle(rootPid);
        }

        void EnsureHandle(int pid)
        {
            if (handles.ContainsKey(pid)) return;
            IntPtr h = Native.OpenProcess(Native.PROCESS_QUERY_LIMITED_INFORMATION | Native.PROCESS_VM_READ, false, pid);
            if (h == IntPtr.Zero) h = Native.OpenProcess(Native.PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
            if (h == IntPtr.Zero) return;
            long c, x, k, u; long expected;
            Tracked.TryGetValue(pid, out expected);
            if (Native.GetProcessTimes(h, out c, out x, out k, out u) && (expected <= 0 || c == expected)) handles[pid] = h;
            else Native.CloseHandle(h);
        }

        public void Dispose()
        {
            cap.Dispose();
            foreach (var h in handles.Values) Native.CloseHandle(h);
            handles.Clear();
        }

        /// Adds descendants of the root that were created after launch (guards against PID reuse).
        public List<int> RefreshTree()
        {
            var snap = Procs.Snapshot();
            var byParent = new Dictionary<int, List<ProcInfo>>();
            foreach (var p in snap) { List<ProcInfo> l; if (!byParent.TryGetValue(p.Ppid, out l)) { l = new List<ProcInfo>(); byParent[p.Ppid] = l; } l.Add(p); }
            var alive = new List<int>();
            var queue = new Queue<int>(); queue.Enqueue(RootPid);
            var seen = new HashSet<int>();
            foreach (var p in snap) if (p.Pid == RootPid) Names[RootPid] = p.Name;
            while (queue.Count > 0)
            {
                int pid = queue.Dequeue();
                if (!seen.Add(pid)) continue;
                alive.Add(pid);
                List<ProcInfo> kids;
                if (!byParent.TryGetValue(pid, out kids)) continue;
                foreach (var k in kids)
                {
                    if (k.Pid == pid || seen.Contains(k.Pid)) continue;
                    long ct;
                    if (!Tracked.TryGetValue(k.Pid, out ct))
                    {
                        ct = Procs.CreationTime(k.Pid);
                        if (ct > 0 && ct < LaunchFileTime - 10000000L) continue; // older than our launch: PID reuse, not ours
                        Tracked[k.Pid] = ct; Names[k.Pid] = k.Name;
                        EnsureHandle(k.Pid);
                    }
                    queue.Enqueue(k.Pid);
                }
            }
            return alive;
        }

        public WinInfo FindMainWindow(int minW, int minH)
        {
            var pids = new HashSet<int>(RefreshTree());
            WinInfo best = null; long bestArea = -1;
            Native.EnumWindows(delegate (IntPtr h, IntPtr l)
            {
                uint wp; Native.GetWindowThreadProcessId(h, out wp);
                if (!pids.Contains((int)wp)) return true;
                if (!Native.IsWindowVisible(h) || Native.IsIconic(h) || Native.IsCloaked(h)) return true;
                int[] cs = Native.ClientSize(h);
                if (cs[0] < minW || cs[1] < minH) return true;
                long area = (long)cs[0] * cs[1];
                if (area > bestArea)
                {
                    bestArea = area;
                    best = new WinInfo { Hwnd = h, Pid = (int)wp, Title = Native.Text(h), ClassName = Native.Class(h), ClientW = cs[0], ClientH = cs[1] };
                }
                return true;
            }, IntPtr.Zero);
            if (best != null) { try { best.Dpi = Native.GetDpiForWindow(best.Hwnd); } catch { } }
            return best;
        }

        /// Polls until a main window exists. Returns the clock time (ms) or -1 on timeout / exit.
        public double WaitForWindow(int timeoutMs, int minW, int minH)
        {
            while (Clock.Elapsed.TotalMilliseconds < timeoutMs)
            {
                var w = FindMainWindow(minW, minH);
                if (w != null) { Win = w; return Clock.Elapsed.TotalMilliseconds; }
                if (!Procs.IsAlive(RootPid, Tracked[RootPid]) && RefreshTree().Count <= 1) { return -1; }
                Thread.Sleep(2);
            }
            return -1;
        }

        public IntPtr FindTopWindowByTitle(string title)
        {
            var pids = new HashSet<int>(RefreshTree());
            IntPtr found = IntPtr.Zero;
            Native.EnumWindows(delegate (IntPtr h, IntPtr l)
            {
                uint wp; Native.GetWindowThreadProcessId(h, out wp);
                if (pids.Contains((int)wp) && Native.IsWindowVisible(h) && Native.Text(h) == title) { found = h; return false; }
                return true;
            }, IntPtr.Zero);
            return found;
        }

        /// Visible top-level windows of the tree other than the main window (dialogs, prompts, popups).
        public string ListOtherWindows()
        {
            var pids = new HashSet<int>(RefreshTree());
            var sb = new StringBuilder();
            IntPtr main = Win != null ? Win.Hwnd : IntPtr.Zero;
            Native.EnumWindows(delegate (IntPtr h, IntPtr l)
            {
                uint wp; Native.GetWindowThreadProcessId(h, out wp);
                if (h == main || !pids.Contains((int)wp) || !Native.IsWindowVisible(h) || Native.IsCloaked(h)) return true;
                int[] cs = Native.ClientSize(h);
                if (cs[0] < 50 || cs[1] < 30) return true;
                if (sb.Length > 0) sb.Append(" | ");
                sb.Append(Native.Class(h)).Append(" '").Append(Native.Text(h)).Append("' ").Append(cs[0]).Append('x').Append(cs[1]);
                return true;
            }, IntPtr.Zero);
            return sb.ToString();
        }

        /// Memory/threads/handles of live tree processes; CPU = user+kernel of every tracked process
        /// including ones that already exited (their handles are held open), so deltas never go negative.
        public TreeSample Sample()
        {
            RefreshTree();
            var snap = Procs.Snapshot();
            var threads = new Dictionary<int, int>();
            foreach (var p in snap) threads[p.Pid] = p.Threads;
            var s = new TreeSample { T = Clock.Elapsed.TotalMilliseconds };
            var names = new Dictionary<string, int>();
            var bad = new StringBuilder();
            foreach (var kv in handles)
            {
                int pid = kv.Key; IntPtr h = kv.Value;
                long c, x, k, u;
                if (!Native.GetProcessTimes(h, out c, out x, out k, out u)) { bad.Append(pid).Append("(times) "); continue; }
                s.Cpu100ns += k + u;
                uint code;
                if (!Native.GetExitCodeProcess(h, out code) || code != Native.STILL_ACTIVE) continue;
                Native.PROCESS_MEMORY_COUNTERS_EX m;
                if (Native.K32GetProcessMemoryInfo(h, out m, Marshal.SizeOf(typeof(Native.PROCESS_MEMORY_COUNTERS_EX))))
                {
                    s.WorkingSet += (long)m.WorkingSetSize.ToUInt64();
                    s.Private += (long)m.PrivateUsage.ToUInt64();
                    long pk = (long)m.PeakWorkingSetSize.ToUInt64();
                    s.PeakWorkingSetSum += pk; if (pk > s.PeakWorkingSetMax) s.PeakWorkingSetMax = pk;
                }
                else bad.Append(pid).Append("(mem) ");
                uint hc; if (Native.GetProcessHandleCount(h, out hc)) s.Handles += hc;
                int tc; if (threads.TryGetValue(pid, out tc)) s.Threads += tc;
                s.Count++;
                string n; Names.TryGetValue(pid, out n); n = n ?? "?";
                int cnt; names.TryGetValue(n, out cnt); names[n] = cnt + 1;
            }
            foreach (var kv in Tracked) if (!handles.ContainsKey(kv.Key) && Procs.IsAlive(kv.Key, kv.Value)) bad.Append(kv.Key).Append("(no-handle) ");
            var nb = new StringBuilder();
            foreach (var kv in names) { if (nb.Length > 0) nb.Append(", "); nb.Append(kv.Key).Append(':').Append(kv.Value); }
            s.Names = nb.ToString(); s.Unreadable = bad.ToString().Trim();
            if (s.Private > PeakPrivateSum) PeakPrivateSum = s.Private;
            if (s.WorkingSet > PeakWsSum) PeakWsSum = s.WorkingSet;
            if (s.Count > PeakProcessCount) PeakProcessCount = s.Count;
            lastSample = s.T;
            return s;
        }

        public byte[] GrabFrame(out int w, out int h)
        {
            cap.Grab(Win.Hwnd);
            w = cap.Width; h = cap.Height;
            var copy = new byte[cap.Buffer.Length];
            System.Buffer.BlockCopy(cap.Buffer, 0, copy, 0, copy.Length);
            return copy;
        }

        public int[] CenterRun() { int w, h; var px = GrabFrame(out w, out h); return Img.CenterRun(px, w, h); }

        /// Records frames until the window is visually stable (see RecordOptions), optionally relative to a baseline.
        public RecordResult Record(byte[] baselineSig, RecordOptions o)
        {
            var r = new RecordResult();
            r.TStart = Clock.Elapsed.TotalMilliseconds;
            byte[] prev = baselineSig;
            bool nonBlank = baselineSig != null;
            bool changed = baselineSig == null;
            int same = 0;
            double capSum = 0;
            while (true)
            {
                double now = Clock.Elapsed.TotalMilliseconds;
                if (now - r.TStart > o.TimeoutMs) { r.TimedOut = true; break; }
                if (Win == null || !Native.IsWindow(Win.Hwnd) || !Native.IsWindowVisible(Win.Hwnd))
                {
                    var w2 = FindMainWindow(200, 150);
                    if (w2 == null) { r.WindowLost = true; break; }
                    Win = w2; prev = null;
                }
                double t0 = Clock.Elapsed.TotalMilliseconds;
                cap.Grab(Win.Hwnd);
                double t1 = Clock.Elapsed.TotalMilliseconds;
                capSum += t1 - t0;
                byte[] sig = Img.Signature(cap.Buffer, cap.Width, cap.Height, o.Step);
                double diff = prev == null ? 1.0 : Img.Diff(sig, prev, o.PixelThr);
                r.Frames.Add(new FrameRec { T = t1, CapMs = t1 - t0, Diff = diff });
                if (!nonBlank && Img.Distinct(sig, o.MinDistinct) >= o.MinDistinct)
                {
                    nonBlank = true; r.TFirstNonBlank = t1;
                    r.FirstNonBlank = (byte[])cap.Buffer.Clone(); r.FnbW = cap.Width; r.FnbH = cap.Height;
                }
                if (!changed && Img.Diff(sig, baselineSig, o.PixelThr) > o.Tol) { changed = true; r.TFirstChange = t1; }
                if (diff > o.Tol)
                {
                    r.TBeforeLastChange = r.Frames.Count >= 2 ? r.Frames[r.Frames.Count - 2].T : r.TStart;
                    r.TLastChange = t1; r.ChangedFrames++; same = 0;
                }
                else same++;
                prev = sig;
                if (t1 - lastSample >= o.SampleEveryMs) Sample();
                if (nonBlank && changed && same >= o.StableFrames && t1 >= o.NotBeforeMs &&
                    t1 - Math.Max(r.TLastChange, r.TStart) >= o.QuietMs)
                { r.Stable = true; r.TStable = t1; break; }
                if (baselineSig != null && !changed && t1 >= o.NotBeforeMs && t1 - r.TStart >= o.WaitChangeMs)
                { r.NoEffect = true; r.Stable = true; r.TStable = t1; break; }
                double elapsed = Clock.Elapsed.TotalMilliseconds - t0;
                if (elapsed < o.IntervalMs) Thread.Sleep((int)Math.Max(1, o.IntervalMs - elapsed));
            }
            r.MeanCapMs = r.Frames.Count > 0 ? capSum / r.Frames.Count : 0;
            r.W = cap.Width; r.H = cap.Height;
            r.Final = (byte[])cap.Buffer.Clone(); r.FinW = cap.Width; r.FinH = cap.Height;
            r.FinalSig = Img.Signature(r.Final, r.FinW, r.FinH, o.Step);
            return r;
        }

        // ---------------- input ----------------

        public IntPtr FocusWindow()
        {
            uint pid; uint tid = Native.GetWindowThreadProcessId(Win.Hwnd, out pid);
            var gi = new Native.GUITHREADINFO(); gi.cbSize = Marshal.SizeOf(typeof(Native.GUITHREADINFO));
            if (Native.GetGUIThreadInfo(tid, ref gi) && gi.hwndFocus != IntPtr.Zero)
            {
                uint fp; Native.GetWindowThreadProcessId(gi.hwndFocus, out fp);
                if (fp == pid) return gi.hwndFocus;
            }
            return Win.Hwnd;
        }

        /// Wheel notches at the client centre, posted to the main window (only for apps that do not
        /// re-route WM_MOUSEWHEEL by screen position; Chromium does, so use keys there).
        public List<InputStep> WheelSteps(int notches, int intervalMs, int delta, bool ctrl, double startMs)
        {
            var steps = new List<InputStep>();
            int[] cs = Native.ClientSize(Win.Hwnd);
            var pt = new Native.POINT { X = cs[0] / 2, Y = cs[1] / 2 };
            steps.Add(new InputStep { AtMs = startMs, Hwnd = Win.Hwnd, Msg = Native.WM_MOUSEMOVE, WParam = IntPtr.Zero, LParam = Native.MakeLParam(pt.X, pt.Y) });
            Native.ClientToScreen(Win.Hwnd, ref pt);
            int keys = ctrl ? 0x0008 /*MK_CONTROL*/ : 0;
            for (int i = 0; i < notches; i++)
                steps.Add(new InputStep { AtMs = startMs + 30 + i * intervalMs, Hwnd = Win.Hwnd, Msg = Native.WM_MOUSEWHEEL, WParam = new IntPtr(((delta & 0xFFFF) << 16) | keys), LParam = Native.MakeLParam(pt.X, pt.Y) });
            return steps;
        }

        public List<InputStep> KeySteps(uint vk, int count, int intervalMs, bool extended, double startMs)
        {
            var steps = new List<InputStep>();
            IntPtr target = FocusWindow();
            uint scan = Native.MapVirtualKeyW(vk, 0);
            long down = 1 | ((long)scan << 16) | (extended ? (1L << 24) : 0);
            long up = down | (1L << 30) | (1L << 31);
            for (int i = 0; i < count; i++)
            {
                steps.Add(new InputStep { AtMs = startMs + i * intervalMs, Hwnd = target, Msg = Native.WM_KEYDOWN, WParam = new IntPtr(vk), LParam = new IntPtr(down) });
                steps.Add(new InputStep { AtMs = startMs + i * intervalMs + 20, Hwnd = target, Msg = Native.WM_KEYUP, WParam = new IntPtr(vk), LParam = new IntPtr(unchecked((int)up)) });
            }
            return steps;
        }

        /// Plays the steps on a background thread. With ctrl=true the Control key is reported as held
        /// to the target GUI thread only (AttachThreadInput + SetKeyboardState), and released afterwards.
        public InputPlayback Play(List<InputStep> steps, bool ctrl, bool safeWheel)
        {
            var pb = new InputPlayback();
            pb.SafeWheel = safeWheel;
            pb.OurPids = new HashSet<int>(RefreshTree());
            if (steps.Count > 0) pb.LastScheduledMs = steps[steps.Count - 1].AtMs;
            uint pid; uint targetTid = Native.GetWindowThreadProcessId(Win.Hwnd, out pid);
            var clock = Clock;
            pb.Worker = new Thread(delegate ()
            {
                uint me = Native.GetCurrentThreadId();
                bool attached = false;
                byte[] state = new byte[256];
                try
                {
                    if (ctrl)
                    {
                        Native.MSG m; Native.PeekMessageW(out m, IntPtr.Zero, 0, 0, 0); // ensure this thread has a message queue
                        attached = Native.AttachThreadInput(me, targetTid, true);
                        if (attached && Native.GetKeyboardState(state))
                        {
                            state[0x11] |= 0x80; state[0xA2] |= 0x80;
                            pb.CtrlInjected = Native.SetKeyboardState(state);
                        }
                        if (!pb.CtrlInjected) pb.Error = "ctrl injection failed (AttachThreadInput/SetKeyboardState)";
                    }
                    foreach (var s in steps)
                    {
                        while (clock.Elapsed.TotalMilliseconds < s.AtMs) Thread.Sleep(1);
                        if (s.Msg == Native.WM_MOUSEWHEEL && pb.SafeWheel)
                        {
                            long lp = s.LParam.ToInt64();
                            var pt = new Native.POINT { X = (short)(lp & 0xFFFF), Y = (short)((lp >> 16) & 0xFFFF) };
                            IntPtr under = Native.WindowFromPoint(pt);
                            uint up2; Native.GetWindowThreadProcessId(under, out up2);
                            if (under == IntPtr.Zero || pb.OurPids == null || !pb.OurPids.Contains((int)up2)) { pb.Skipped++; continue; }
                        }
                        Native.PostMessageW(s.Hwnd, s.Msg, s.WParam, s.LParam);
                        lock (pb.SentAt) pb.SentAt.Add(clock.Elapsed.TotalMilliseconds);
                    }
                    if (ctrl) Thread.Sleep(400); // let the target consume the messages while Ctrl is still reported down
                }
                catch (Exception ex) { pb.Error = ex.Message; }
                finally
                {
                    if (attached)
                    {
                        if (Native.GetKeyboardState(state)) { state[0x11] &= 0x7F; state[0xA2] &= 0x7F; Native.SetKeyboardState(state); }
                        Native.AttachThreadInput(me, targetTid, false);
                    }
                }
            });
            pb.Worker.IsBackground = true;
            pb.Worker.Start();
            return pb;
        }

        /// True when the topmost window at the client point belongs to our process tree.
        public bool PointIsOurs(int x, int y)
        {
            var pt = new Native.POINT { X = x, Y = y };
            Native.ClientToScreen(Win.Hwnd, ref pt);
            IntPtr under = Native.WindowFromPoint(pt);
            uint pid; Native.GetWindowThreadProcessId(under, out pid);
            return under != IntPtr.Zero && new HashSet<int>(RefreshTree()).Contains((int)pid);
        }

        public void ClickClient(int x, int y)
        {
            IntPtr lp = Native.MakeLParam(x, y);
            Native.PostMessageW(Win.Hwnd, Native.WM_MOUSEMOVE, IntPtr.Zero, lp); Thread.Sleep(80);
            Native.PostMessageW(Win.Hwnd, Native.WM_LBUTTONDOWN, new IntPtr(1), lp); Thread.Sleep(60);
            Native.PostMessageW(Win.Hwnd, Native.WM_LBUTTONUP, IntPtr.Zero, lp);
        }

        static IntPtr FindChild(IntPtr parent, string cls, int ctrlId)
        {
            IntPtr found = IntPtr.Zero;
            Native.EnumChildWindows(parent, delegate (IntPtr h, IntPtr l)
            {
                if (Native.Class(h) == cls && (ctrlId < 0 || Native.GetDlgCtrlID(h) == ctrlId)) { found = h; return false; }
                return true;
            }, IntPtr.Zero);
            return found;
        }

        /// Common item dialog: file name box = ComboBoxEx32 id 0x47C -> Edit; Open button id 1 (IDOK).
        public static string OpenInCommonDialog(IntPtr dlg, string path)
        {
            IntPtr combo = FindChild(dlg, "ComboBoxEx32", 0x47C);
            IntPtr edit = combo != IntPtr.Zero ? FindChild(combo, "Edit", -1) : IntPtr.Zero;
            if (edit == IntPtr.Zero) edit = FindChild(dlg, "Edit", -1);
            if (edit == IntPtr.Zero) return "no-edit";
            Native.SendMessageW(edit, Native.WM_SETTEXT, IntPtr.Zero, path);
            IntPtr ok = FindChild(dlg, "Button", 1);
            if (ok == IntPtr.Zero) return "no-ok-button";
            Native.PostMessageW(ok, Native.BM_CLICK, IntPtr.Zero, IntPtr.Zero);
            return "ok";
        }

        // ---------------- shutdown ----------------

        public bool CloseGracefully(int waitMs)
        {
            if (Win != null && Native.IsWindow(Win.Hwnd)) Native.PostMessageW(Win.Hwnd, Native.WM_CLOSE, IntPtr.Zero, IntPtr.Zero);
            var sw = Stopwatch.StartNew();
            while (sw.ElapsedMilliseconds < waitMs)
            {
                if (!Procs.IsAlive(RootPid, Tracked[RootPid])) return true;
                Thread.Sleep(50);
            }
            return false;
        }

        /// Terminates every tracked process that is still alive with the same creation time. Returns the count killed.
        public int KillTree()
        {
            RefreshTree();
            int killed = 0;
            var pids = new List<int>();
            pids.Add(RootPid); // root first, then descendants
            foreach (int pid in Tracked.Keys) if (pid != RootPid) pids.Add(pid);
            foreach (int pid in pids)
            {
                long ct = Tracked[pid];
                if (!Procs.IsAlive(pid, ct)) continue;
                IntPtr h = Native.OpenProcess(Native.PROCESS_TERMINATE | Native.PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
                if (h == IntPtr.Zero) continue;
                try { if (Native.TerminateProcess(h, 1)) killed++; } finally { Native.CloseHandle(h); }
            }
            return killed;
        }

        public int AliveCount()
        {
            int n = 0;
            foreach (var kv in Tracked) if (Procs.IsAlive(kv.Key, kv.Value)) n++;
            return n;
        }

        public string TrackedNames()
        {
            var sb = new StringBuilder();
            foreach (var kv in Names) { if (sb.Length > 0) sb.Append(", "); sb.Append(kv.Value).Append('#').Append(kv.Key); }
            return sb.ToString();
        }
    }
}
