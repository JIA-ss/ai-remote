"""Windows-only containment and client-area evidence for the desktop CI harness.

No shell is involved and no process names are killed. The caller must supply a private log
path; neither child output nor command/environment/path strings are returned or printed.
This module is importable on other platforms, but does not emulate the Windows operations.

Win32 contracts (Microsoft documentation):
https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw
https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute
https://learn.microsoft.com/windows/win32/api/jobapi2/nf-jobapi2-assignprocesstojobobject
https://learn.microsoft.com/windows/win32/api/processthreadsapi/nf-processthreadsapi-resumethread
https://learn.microsoft.com/windows/win32/procthread/job-objects
https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-getwindowrect
https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-clienttoscreen
https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-setthreaddpiawarenesscontext
"""

from __future__ import annotations

import ctypes as C
import math
import os
from pathlib import Path
import shutil
import subprocess
import time


# Fixed-width Win32 fields also make the declarations inspectable on a non-Windows host.
DWORD = C.c_uint32
LONG = C.c_int32
BOOL = C.c_int32
HANDLE = C.c_void_p
SIZE_T = C.c_size_t
ULONG_PTR = C.c_size_t


class _SecurityAttributes(C.Structure):
    _fields_ = [("nLength", DWORD), ("lpSecurityDescriptor", C.c_void_p), ("bInheritHandle", BOOL)]


class _StartupInfo(C.Structure):
    _fields_ = [
        ("cb", DWORD), ("lpReserved", C.c_wchar_p), ("lpDesktop", C.c_wchar_p),
        ("lpTitle", C.c_wchar_p), ("dwX", DWORD), ("dwY", DWORD), ("dwXSize", DWORD),
        ("dwYSize", DWORD), ("dwXCountChars", DWORD), ("dwYCountChars", DWORD),
        ("dwFillAttribute", DWORD), ("dwFlags", DWORD), ("wShowWindow", C.c_uint16),
        ("cbReserved2", C.c_uint16), ("lpReserved2", C.c_void_p),
        ("hStdInput", HANDLE), ("hStdOutput", HANDLE), ("hStdError", HANDLE),
    ]


class _StartupInfoEx(C.Structure):
    _fields_ = [("StartupInfo", _StartupInfo), ("lpAttributeList", C.c_void_p)]


class _ProcessInfo(C.Structure):
    _fields_ = [("hProcess", HANDLE), ("hThread", HANDLE), ("dwProcessId", DWORD), ("dwThreadId", DWORD)]


class _BasicLimits(C.Structure):
    _fields_ = [
        ("PerProcessUserTimeLimit", C.c_int64), ("PerJobUserTimeLimit", C.c_int64),
        ("LimitFlags", DWORD), ("MinimumWorkingSetSize", SIZE_T), ("MaximumWorkingSetSize", SIZE_T),
        ("ActiveProcessLimit", DWORD), ("Affinity", ULONG_PTR), ("PriorityClass", DWORD),
        ("SchedulingClass", DWORD),
    ]


class _IoCounters(C.Structure):
    _fields_ = [(name, C.c_uint64) for name in (
        "ReadOperationCount", "WriteOperationCount", "OtherOperationCount", "ReadTransferCount",
        "WriteTransferCount", "OtherTransferCount",
    )]


class _ExtendedLimits(C.Structure):
    _fields_ = [
        ("BasicLimitInformation", _BasicLimits), ("IoInfo", _IoCounters),
        ("ProcessMemoryLimit", SIZE_T), ("JobMemoryLimit", SIZE_T),
        ("PeakProcessMemoryUsed", SIZE_T), ("PeakJobMemoryUsed", SIZE_T),
    ]


class _Accounting(C.Structure):
    _fields_ = [
        ("TotalUserTime", C.c_int64), ("TotalKernelTime", C.c_int64),
        ("ThisPeriodTotalUserTime", C.c_int64), ("ThisPeriodTotalKernelTime", C.c_int64),
        ("TotalPageFaultCount", DWORD), ("TotalProcesses", DWORD),
        ("ActiveProcesses", DWORD), ("TotalTerminatedProcesses", DWORD),
    ]


class _Rect(C.Structure):
    _fields_ = [(name, LONG) for name in ("left", "top", "right", "bottom")]


class _Point(C.Structure):
    _fields_ = [("x", LONG), ("y", LONG)]


class _WinFailure(Exception):
    def __init__(self, stage: str, winerror: int | None = None):
        super().__init__(stage)
        self.stage = stage
        self.winerror = winerror


def _kernel():
    if os.name != "nt":
        raise OSError("the desktop CI containment helper requires Windows")
    k = C.WinDLL("kernel32", use_last_error=True)
    signatures = {
        "CreateJobObjectW": ([C.c_void_p, C.c_wchar_p], HANDLE),
        "SetInformationJobObject": ([HANDLE, C.c_int, C.c_void_p, DWORD], BOOL),
        "QueryInformationJobObject": ([HANDLE, C.c_int, C.c_void_p, DWORD, C.POINTER(DWORD)], BOOL),
        "AssignProcessToJobObject": ([HANDLE, HANDLE], BOOL),
        "TerminateJobObject": ([HANDLE, DWORD], BOOL),
        "CreateFileW": ([C.c_wchar_p, DWORD, DWORD, C.POINTER(_SecurityAttributes), DWORD, DWORD, HANDLE], HANDLE),
        "InitializeProcThreadAttributeList": ([C.c_void_p, DWORD, DWORD, C.POINTER(SIZE_T)], BOOL),
        "UpdateProcThreadAttribute": ([C.c_void_p, DWORD, ULONG_PTR, C.c_void_p, SIZE_T, C.c_void_p, C.c_void_p], BOOL),
        "DeleteProcThreadAttributeList": ([C.c_void_p], None),
        "CreateProcessW": ([C.c_wchar_p, C.c_wchar_p, C.c_void_p, C.c_void_p, BOOL, DWORD,
                            C.c_void_p, C.c_wchar_p, C.POINTER(_StartupInfo), C.POINTER(_ProcessInfo)], BOOL),
        "ResumeThread": ([HANDLE], DWORD),
        "WaitForSingleObject": ([HANDLE, DWORD], DWORD),
        "GetExitCodeProcess": ([HANDLE, C.POINTER(DWORD)], BOOL),
        "TerminateProcess": ([HANDLE, DWORD], BOOL),
        "CloseHandle": ([HANDLE], BOOL),
    }
    for name, (args, result) in signatures.items():
        fn = getattr(k, name)
        fn.argtypes, fn.restype = args, result
    return k


def _require(ok, stage: str):
    if not ok:
        raise _WinFailure(stage, C.get_last_error())


def _job_empty(k, job, seconds: float = 10.0) -> bool:
    deadline = time.monotonic() + seconds
    while True:
        accounting = _Accounting()
        if not k.QueryInformationJobObject(job, 1, C.byref(accounting), C.sizeof(accounting), None):
            return False
        if accounting.ActiveProcesses == 0:
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.05)


def run_scoped(command, cwd: Path, env: dict, log: Path, timeout_seconds: float) -> dict:
    """Run a native executable in a private, non-breakaway Job Object.

    ``command`` is an argv sequence (typically ``[sys.executable, harness, ...]``), never a shell
    command string. ``env`` replaces the environment. stdout/stderr share the supplied private
    log and stdin is NUL. The process is suspended until assignment succeeds. On every exit we
    end remaining members of this job and verify ActiveProcesses == 0 before closing the job.

    Returns returncode (None if unavailable), timed_out, cleanup_ok, plus a static error stage
    and numeric winerror on helper failure. No raw exception text or child output is exposed.
    A true cleanup_ok verifies only this process scope, not deletion of profiles or log files.
    """
    if os.name != "nt":
        raise OSError("the desktop CI containment helper requires Windows")
    if isinstance(command, (str, bytes)) or not command:
        raise ValueError("command must be a nonempty argv sequence")
    if not math.isfinite(timeout_seconds) or timeout_seconds <= 0:
        raise ValueError("timeout_seconds must be finite and positive")
    argv = [os.fspath(arg) for arg in command]
    if any(not isinstance(arg, str) or "\0" in arg for arg in argv):
        raise ValueError("argv entries must be Unicode strings without NUL")
    if any(not isinstance(key, str) or not key or "=" in key or "\0" in key
           or not isinstance(value, str) or "\0" in value for key, value in env.items()):
        raise ValueError("environment entries must be valid Unicode key/value strings")
    if len({key.upper() for key in env}) != len(env):
        raise ValueError("environment keys must be unique without regard to case")

    result = {"returncode": None, "timed_out": False, "cleanup_ok": False, "error": None, "winerror": None}
    k = _kernel()
    handles = []
    job = None
    process = _ProcessInfo()
    attributes = None
    attributes_initialized = False
    assigned = False
    deadline = time.monotonic() + timeout_seconds
    try:
        cwd, log = Path(cwd).resolve(), Path(log).resolve()
        program = Path(argv[0])
        if not program.is_absolute():
            if any(sep in argv[0] for sep in ("/", "\\")):
                program = cwd / program
            else:
                path = next((value for key, value in env.items() if key.upper() == "PATH"), "")
                found = shutil.which(argv[0], path=path)
                if not found:
                    raise _WinFailure("executable_not_found")
                program = Path(found)
        program = program.resolve()
        if program.suffix.lower() in (".cmd", ".bat"):
            raise _WinFailure("shell_script_not_supported")
        argv[0] = str(program)
        log.parent.mkdir(parents=True, exist_ok=True)
        job = k.CreateJobObjectW(None, None)  # Unnamed and noninheritable: this supervisor owns it.
        _require(job, "create_job")
        limits = _ExtendedLimits()
        limits.BasicLimitInformation.LimitFlags = 0x00002000  # JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        _require(k.SetInformationJobObject(job, 9, C.byref(limits), C.sizeof(limits)), "set_job_limits")

        security = _SecurityAttributes(C.sizeof(_SecurityAttributes), None, True)
        invalid = C.c_void_p(-1).value
        for name, access, disposition in ((str(log), 0x40000000, 2), ("NUL", 0x80000000, 3)):
            handle = k.CreateFileW(name, access, 3, C.byref(security), disposition, 0x80, None)
            if handle in (None, invalid):
                raise _WinFailure("open_private_stdio", C.get_last_error())
            handles.append(handle)
        output, input_null = handles

        size = SIZE_T()
        k.InitializeProcThreadAttributeList(None, 1, 0, C.byref(size))  # Expected size-query failure.
        if not size.value:
            raise _WinFailure("attribute_list_size", C.get_last_error())
        attributes = C.create_string_buffer(size.value)
        _require(k.InitializeProcThreadAttributeList(attributes, 1, 0, C.byref(size)), "initialize_attributes")
        attributes_initialized = True
        inherited = (HANDLE * 2)(output, input_null)
        _require(k.UpdateProcThreadAttribute(attributes, 0, 0x00020002, inherited, C.sizeof(inherited), None, None),
                 "restrict_inherited_handles")  # PROC_THREAD_ATTRIBUTE_HANDLE_LIST
        startup = _StartupInfoEx()
        startup.StartupInfo.cb = C.sizeof(startup)
        startup.StartupInfo.dwFlags = 0x00000100  # STARTF_USESTDHANDLES
        startup.StartupInfo.hStdInput = input_null
        startup.StartupInfo.hStdOutput = output
        startup.StartupInfo.hStdError = output
        startup.lpAttributeList = C.cast(attributes, C.c_void_p)
        command_line = C.create_unicode_buffer(subprocess.list2cmdline(argv))
        environment = C.create_unicode_buffer("\0".join(
            f"{key}={value}" for key, value in sorted(env.items(), key=lambda item: item[0].upper())
        ) + "\0\0")
        flags = 0x00000004 | 0x00000400 | 0x00080000 | 0x08000000
        # CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW
        _require(k.CreateProcessW(str(program), command_line, None, None, True, flags, environment, str(cwd),
                                  C.cast(C.byref(startup), C.POINTER(_StartupInfo)), C.byref(process)),
                 "create_suspended_process")
        _require(k.AssignProcessToJobObject(job, process.hProcess), "assign_process_to_job")
        assigned = True
        previous_count = k.ResumeThread(process.hThread)
        if previous_count != 1:
            raise _WinFailure("resume_primary_thread", C.get_last_error() if previous_count == 0xFFFFFFFF else None)
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                result["timed_out"] = True
                break
            waited = k.WaitForSingleObject(process.hProcess, min(200, max(1, math.ceil(remaining * 1000))))
            if waited == 0:  # WAIT_OBJECT_0
                exit_code = DWORD()
                _require(k.GetExitCodeProcess(process.hProcess, C.byref(exit_code)), "read_process_exit")
                result["returncode"] = int(exit_code.value)
                break
            if waited != 0x00000102:  # WAIT_TIMEOUT
                raise _WinFailure("wait_process", C.get_last_error())
    except _WinFailure as error:
        result.update(error=error.stage, winerror=error.winerror)
    except OSError as error:
        result.update(error="filesystem_or_launch_setup", winerror=getattr(error, "winerror", None))
    finally:
        cleanup_ok = True
        try:
            if assigned:
                # Also stop descendants after a successful root exit: controller agents are detached.
                k.TerminateJobObject(job, 124 if result["timed_out"] else 1)
                cleanup_ok = _job_empty(k, job)
            elif process.hProcess:
                # Assignment failed while still suspended: only our just-created process can exist.
                k.TerminateProcess(process.hProcess, 1)
            if process.hProcess:
                stopped = k.WaitForSingleObject(process.hProcess, 10000) == 0
                cleanup_ok = cleanup_ok and stopped
                if stopped and result["returncode"] is None:
                    exit_code = DWORD()
                    if k.GetExitCodeProcess(process.hProcess, C.byref(exit_code)):
                        result["returncode"] = int(exit_code.value)
            if attributes_initialized:
                k.DeleteProcThreadAttributeList(attributes)
        finally:
            # Closing the sole job handle is a final fail-safe, even if verification failed.
            for handle in [process.hThread, process.hProcess, *handles, job]:
                if handle and not k.CloseHandle(handle):
                    cleanup_ok = False
            result["cleanup_ok"] = cleanup_ok
    return result


def client_box(pid: int, geometry: dict) -> dict | None:
    """Return a reliable physical-pixel client rectangle inside a PrintWindow image, or None.

    The sole eligible window must be visible, unowned, not minimized, belong to ``pid``, and
    exactly match geometry['window'] (x/y/width/height). No titles or other processes' content
    are read. DPI awareness is changed only on this thread and restored before returning.
    ``original_size`` is [outer_width, outer_height]; ``client_box`` is [left, top, right, bottom]
    relative to that outer image, with exclusive right/bottom coordinates (Pillow box format).
    """
    if os.name != "nt" or type(pid) is not int or not 0 < pid <= 0xFFFFFFFF or not isinstance(geometry, dict):
        return None
    expected = geometry.get("window")
    if not isinstance(expected, dict) or any(type(expected.get(key)) is not int for key in ("x", "y", "width", "height")):
        return None
    if expected["width"] <= 0 or expected["height"] <= 0:
        return None
    try:
        u = C.WinDLL("user32", use_last_error=True)
        callback_type = C.WINFUNCTYPE(BOOL, HANDLE, C.c_ssize_t)
        signatures = {
            "EnumWindows": ([callback_type, C.c_ssize_t], BOOL),
            "GetWindowThreadProcessId": ([HANDLE, C.POINTER(DWORD)], DWORD),
            "GetWindowRect": ([HANDLE, C.POINTER(_Rect)], BOOL),
            "GetClientRect": ([HANDLE, C.POINTER(_Rect)], BOOL),
            "ClientToScreen": ([HANDLE, C.POINTER(_Point)], BOOL),
            "GetWindow": ([HANDLE, DWORD], HANDLE),
            "IsWindowVisible": ([HANDLE], BOOL),
            "IsIconic": ([HANDLE], BOOL),
            "SetThreadDpiAwarenessContext": ([HANDLE], HANDLE),
        }
        for name, (args, restype) in signatures.items():
            fn = getattr(u, name)
            fn.argtypes, fn.restype = args, restype
        old_dpi = u.SetThreadDpiAwarenessContext(C.c_void_p(-4))  # PER_MONITOR_AWARE_V2
        if not old_dpi:
            return None
    except (AttributeError, OSError):
        return None
    result = None
    try:
        matches = []
        callback_failed = False

        def inspect(hwnd, _):
            nonlocal callback_failed
            try:
                owner_pid = DWORD()
                if not u.GetWindowThreadProcessId(hwnd, C.byref(owner_pid)) or owner_pid.value != pid:
                    return True
                if not u.IsWindowVisible(hwnd) or u.IsIconic(hwnd) or u.GetWindow(hwnd, 4):  # GW_OWNER
                    return True
                outer, client = _Rect(), _Rect()
                if not u.GetWindowRect(hwnd, C.byref(outer)):
                    return True
                actual = {"x": outer.left, "y": outer.top,
                          "width": outer.right - outer.left, "height": outer.bottom - outer.top}
                if actual != {key: expected[key] for key in actual}:
                    return True
                if not u.GetClientRect(hwnd, C.byref(client)) or client.left != 0 or client.top != 0:
                    callback_failed = True
                    return True
                origin, end = _Point(client.left, client.top), _Point(client.right, client.bottom)
                if not u.ClientToScreen(hwnd, C.byref(origin)) or not u.ClientToScreen(hwnd, C.byref(end)):
                    callback_failed = True
                    return True
                left, top = origin.x - outer.left, origin.y - outer.top
                right, bottom = end.x - outer.left, end.y - outer.top
                if not (0 <= left < right <= actual["width"] and 0 <= top < bottom <= actual["height"]
                        and right - left == client.right and bottom - top == client.bottom):
                    callback_failed = True
                    return True
                # Reject a move, resize, PID change or destroyed HWND during collection.
                again, again_pid = _Rect(), DWORD()
                if (not u.GetWindowRect(hwnd, C.byref(again))
                        or not u.GetWindowThreadProcessId(hwnd, C.byref(again_pid)) or again_pid.value != pid
                        or tuple(getattr(again, key) for key, _ in _Rect._fields_)
                        != tuple(getattr(outer, key) for key, _ in _Rect._fields_)):
                    callback_failed = True
                    return True
                matches.append({"original_size": [actual["width"], actual["height"]],
                                "client_box": [left, top, right, bottom]})
            except Exception:
                callback_failed = True
            return True

        callback = callback_type(inspect)
        if u.EnumWindows(callback, 0) and not callback_failed and len(matches) == 1:
            result = matches[0]
    except (AttributeError, OSError):
        result = None
    finally:
        if not u.SetThreadDpiAwarenessContext(old_dpi):
            result = None
    return result
