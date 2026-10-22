# Watches, on Windows, for windows that a background host's agents open: every top-level window shown while
# it runs (a WinEvent hook, so a console that flashes for a few milliseconds is still caught), and the process
# tree under the ai-remote-hostw processes whose command line contains -Marker (polled). Stops when -Stop
# exists, then writes a JSON report to -Out: the windows shown (pid, process, class, title, whether the
# process belongs to the tree), the tree's processes seen and the ones still alive.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/win-window-watch.ps1 -Marker <host home> -Stop <file> -Out <json>
#
# Run it in the interactive desktop session (not over SSH, whose session has no desktop).
param([Parameter(Mandatory)] [string]$Marker, [Parameter(Mandatory)] [string]$Stop, [Parameter(Mandatory)] [string]$Out)

Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
public static class WinWatch {
    delegate void WinEventProc(IntPtr hook, uint ev, IntPtr hwnd, int idObject, int idChild, uint thread, uint time);
    [DllImport("user32.dll")] static extern IntPtr SetWinEventHook(uint min, uint max, IntPtr mod, WinEventProc proc, uint pid, uint tid, uint flags);
    [DllImport("user32.dll")] static extern bool UnhookWinEvent(IntPtr hook);
    [DllImport("user32.dll")] static extern bool PeekMessage(out MSG msg, IntPtr hwnd, uint min, uint max, uint remove);
    [DllImport("user32.dll")] static extern bool TranslateMessage(ref MSG msg);
    [DllImport("user32.dll")] static extern IntPtr DispatchMessage(ref MSG msg);
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr hwnd, uint flags);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder s, int n);
    [DllImport("kernel32.dll")] static extern IntPtr CreateToolhelp32Snapshot(uint flags, uint pid);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern bool Process32FirstW(IntPtr snap, ref PROCESSENTRY32W e);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] static extern bool Process32NextW(IntPtr snap, ref PROCESSENTRY32W e);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr h);
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] public struct PROCESSENTRY32W {
        public uint dwSize; public uint cntUsage; public uint th32ProcessID; public IntPtr th32DefaultHeapID; public uint th32ModuleID;
        public uint cntThreads; public uint th32ParentProcessID; public int pcPriClassBase; public uint dwFlags;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)] public string szExeFile; }
    // "name(pid) <- parent(pid) <- ..." for the process that owns a window, read the moment it shows.
    static string Chain(uint pid) {
        var names = new Dictionary<uint, string>(); var parents = new Dictionary<uint, uint>();
        IntPtr snap = CreateToolhelp32Snapshot(2, 0);
        var e = new PROCESSENTRY32W(); e.dwSize = (uint)Marshal.SizeOf(typeof(PROCESSENTRY32W));
        if (Process32FirstW(snap, ref e)) do { names[e.th32ProcessID] = e.szExeFile; parents[e.th32ProcessID] = e.th32ParentProcessID; } while (Process32NextW(snap, ref e));
        CloseHandle(snap);
        var sb = new StringBuilder(); var seen = new HashSet<uint>();
        while (pid != 0 && seen.Add(pid) && sb.Length < 600) {
            string n; if (!names.TryGetValue(pid, out n)) { sb.Append("?(" + pid + ")"); break; }
            if (sb.Length > 0) sb.Append(" <- ");
            sb.Append(n + "(" + pid + ")");
            pid = parents[pid];
        }
        return sb.ToString();
    }
    [StructLayout(LayoutKind.Sequential)] public struct MSG { public IntPtr hwnd; public uint message; public IntPtr w; public IntPtr l; public uint time; public int x; public int y; }
    public class Shown { public long Ms; public uint Pid; public string Class; public string Title; public string Chain; }
    public static List<Shown> Seen = new List<Shown>();
    static WinEventProc keep;
    static volatile bool stop;
    static DateTime t0;
    static void On(IntPtr hook, uint ev, IntPtr hwnd, int idObject, int idChild, uint thread, uint time) {
        if (idObject != 0 || hwnd == IntPtr.Zero || GetAncestor(hwnd, 2) != hwnd || !IsWindowVisible(hwnd)) return;
        uint pid; GetWindowThreadProcessId(hwnd, out pid);
        var c = new StringBuilder(256); GetClassName(hwnd, c, 256);
        var t = new StringBuilder(256); GetWindowText(hwnd, t, 256);
        lock (Seen) Seen.Add(new Shown { Ms = (long)(DateTime.Now - t0).TotalMilliseconds, Pid = pid, Class = c.ToString(), Title = t.ToString(), Chain = Chain(pid) });
    }
    public static void Start() {
        t0 = DateTime.Now;
        var th = new Thread(() => {
            keep = On;
            // EVENT_OBJECT_SHOW (0x8002), out of context, skipping our own process.
            IntPtr hook = SetWinEventHook(0x8002, 0x8002, IntPtr.Zero, keep, 0, 0, 0x0000 | 0x0002);
            MSG m;
            while (!stop) {
                while (PeekMessage(out m, IntPtr.Zero, 0, 0, 1)) { TranslateMessage(ref m); DispatchMessage(ref m); }
                Thread.Sleep(5);
            }
            UnhookWinEvent(hook);
        });
        th.IsBackground = true; th.Start();
    }
    public static void End() { stop = true; Thread.Sleep(50); }
}
"@

[WinWatch]::Start()
$tree = @{}   # pid -> @{ name; cmd; first_ms }
$t0 = Get-Date
while (-not (Test-Path $Stop)) {
    $ms = [int]((Get-Date) - $t0).TotalMilliseconds
    $all = Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, Name, CommandLine
    foreach ($p in $all) {
        if (-not $tree.ContainsKey([int]$p.ProcessId) -and $p.Name -eq 'ai-remote-hostw.exe' -and "$($p.CommandLine)".Contains($Marker)) {
            $tree[[int]$p.ProcessId] = @{ name = $p.Name; cmd = "$($p.CommandLine)"; first_ms = $ms; root = $true }
        }
    }
    do {
        $added = $false
        foreach ($p in $all) {
            if (-not $tree.ContainsKey([int]$p.ProcessId) -and $tree.ContainsKey([int]$p.ParentProcessId)) {
                $tree[[int]$p.ProcessId] = @{ name = $p.Name; cmd = "$($p.CommandLine)"; first_ms = $ms; root = $false }
                $added = $true
            }
        }
    } while ($added)
    Start-Sleep -Milliseconds 150
}
Start-Sleep -Seconds 3   # let the host's stop finish before looking for survivors
[WinWatch]::End()
$alive = @{}
foreach ($p in Get-Process) { $alive[[int]$p.Id] = $p.ProcessName }
$procs = foreach ($k in $tree.Keys) {
    $v = $tree[$k]
    [pscustomobject]@{ pid = $k; name = $v.name; first_ms = $v.first_ms; alive = ($alive.ContainsKey($k) -and "$($alive[$k]).exe" -eq $v.name); cmd = $v.cmd.Substring(0, [Math]::Min(200, $v.cmd.Length)) }
}
$windows = foreach ($w in [WinWatch]::Seen) {
    [pscustomobject]@{ ms = $w.Ms; pid = $w.Pid; process = $alive[[int]$w.Pid]; in_tree = $tree.ContainsKey([int]$w.Pid); class = $w.Class; title = $w.Title; chain = $w.Chain }
}
[pscustomobject]@{
    windows = @($windows)
    console_windows = @($windows | Where-Object { $_.in_tree -or $_.class -in 'ConsoleWindowClass', 'CASCADIA_HOSTING_WINDOW_CLASS', 'PseudoConsoleWindow' })
    tree = @($procs | Sort-Object first_ms)
    survivors = @($procs | Where-Object { $_.alive })
} | ConvertTo-Json -Depth 4 | Set-Content -Encoding utf8 $Out
