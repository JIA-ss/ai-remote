"""Real keyboard and mouse input on Windows for the desktop e2e (tools/desktop-chat-e2e.py): a page script asks
for it with `(action) input {...}` and the keys go through SendInput, so an input method sees them the way it sees
a person typing. One action is a JSON object with one of:

  {"focus": true}            bring the app's window to the front
  {"click": [x, y]}          click at (x, y) physical pixels from the top left of the window's client area
  {"layout": "00000804"}     ask the focused window to switch to this keyboard layout (0804: Chinese, with the
                             system's default input method for it; 0409: US English)
  {"type": "nihao"}          letters and digits, one key press each
  {"key": "RETURN"}          one key: RETURN, SPACE, SHIFT, ESCAPE, BACK
  {"chord": ["CONTROL", "SPACE"]}  keys held together
  {"restore": true}          undo what "focus" changed (it turns Caps Lock off, which would type capitals)
"""
import ctypes
import json
import time
from ctypes import wintypes

user32 = ctypes.WinDLL("user32", use_last_error=True)

# Physical pixels everywhere (the page's coordinates times devicePixelRatio): without this a scaled display
# gives this process virtualised coordinates and clicks land beside the window.
user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4))  # PER_MONITOR_AWARE_V2

ULONG_PTR = ctypes.c_size_t
user32.WindowFromPoint.argtypes = [wintypes.POINT]
user32.WindowFromPoint.restype = wintypes.HWND
user32.GetForegroundWindow.restype = wintypes.HWND
user32.GetKeyboardLayout.restype = ctypes.c_void_p
user32.GetKeyboardLayout.argtypes = [wintypes.DWORD]
user32.LoadKeyboardLayoutW.restype = ctypes.c_void_p
user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, ctypes.c_void_p]


class KEYBDINPUT(ctypes.Structure):
    _fields_ = [("wVk", wintypes.WORD), ("wScan", wintypes.WORD), ("dwFlags", wintypes.DWORD),
                ("time", wintypes.DWORD), ("dwExtraInfo", ULONG_PTR)]


class MOUSEINPUT(ctypes.Structure):
    _fields_ = [("dx", wintypes.LONG), ("dy", wintypes.LONG), ("mouseData", wintypes.DWORD),
                ("dwFlags", wintypes.DWORD), ("time", wintypes.DWORD), ("dwExtraInfo", ULONG_PTR)]


class _U(ctypes.Union):
    _fields_ = [("ki", KEYBDINPUT), ("mi", MOUSEINPUT), ("pad", ctypes.c_byte * 32)]


class INPUT(ctypes.Structure):
    _fields_ = [("type", wintypes.DWORD), ("u", _U)]


KEYS = {"RETURN": 0x0D, "SPACE": 0x20, "SHIFT": 0x10, "ESCAPE": 0x1B, "BACK": 0x08, "MENU": 0x12, "CAPITAL": 0x14, "CONTROL": 0x11, "LWIN": 0x5B}
WM_INPUTLANGCHANGEREQUEST = 0x0050
_state = {"caps": False}


def _send(*inputs):
    arr = (INPUT * len(inputs))(*inputs)
    if user32.SendInput(len(inputs), arr, ctypes.sizeof(INPUT)) != len(inputs):
        raise OSError(ctypes.get_last_error(), "SendInput did not send every event")


def _key(vk, up=False):
    i = INPUT(type=1)
    i.u.ki = KEYBDINPUT(vk, user32.MapVirtualKeyW(vk, 0), 2 if up else 0, 0, 0)
    return i


def press(vk):
    _send(_key(vk))
    time.sleep(0.03)
    _send(_key(vk, up=True))
    time.sleep(0.06)


class GUITHREADINFO(ctypes.Structure):
    _fields_ = [("cbSize", wintypes.DWORD), ("flags", wintypes.DWORD), ("hwndActive", wintypes.HWND),
                ("hwndFocus", wintypes.HWND), ("hwndCapture", wintypes.HWND), ("hwndMenuOwner", wintypes.HWND),
                ("hwndMoveSize", wintypes.HWND), ("hwndCaret", wintypes.HWND), ("rcCaret", wintypes.RECT)]


def _focus():
    info = GUITHREADINFO(cbSize=ctypes.sizeof(GUITHREADINFO))
    user32.GetGUIThreadInfo(0, ctypes.byref(info))
    return info.hwndFocus


def _describe(hwnd):
    if not hwnd:
        return "nothing"
    cls = ctypes.create_unicode_buffer(128)
    user32.GetClassNameW(hwnd, cls, 128)
    pid = wintypes.DWORD()
    user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    return f"{cls.value}(pid {pid.value})"


def window_of(pid):
    found = []
    proc = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

    def each(hwnd, _):
        owner = wintypes.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value == pid and user32.IsWindowVisible(hwnd) and user32.GetWindowTextLengthW(hwnd) > 0:
            found.append(hwnd)
        return True

    user32.EnumWindows(proc(each), 0)
    return found[0] if found else None


def act(pid, action):
    """Carries out one action for the app process `pid`; returns a short description."""
    hwnd = window_of(pid)
    if not hwnd:
        raise RuntimeError("the app has no visible window")
    if action.get("focus"):
        # A process that is not in front may only take the foreground right after a key event.
        _send(_key(KEYS["MENU"]), _key(KEYS["MENU"], up=True))
        user32.SetForegroundWindow(hwnd)
        time.sleep(0.3)
        caps = ""
        if user32.GetKeyState(KEYS["CAPITAL"]) & 1:
            # Caps Lock on would type capitals past the input method.
            press(KEYS["CAPITAL"])
            _state["caps"] = True
            caps = "; Caps Lock was on, turned it off"
        return f"foreground is the app: {user32.GetForegroundWindow() == hwnd}{caps}"
    if "click" in action:
        pt = wintypes.POINT(*action["click"])
        user32.ClientToScreen(hwnd, ctypes.byref(pt))
        user32.SetCursorPos(pt.x, pt.y)
        under = user32.WindowFromPoint(pt)
        down, up = INPUT(type=0), INPUT(type=0)
        down.u.mi = MOUSEINPUT(0, 0, 0, 0x0002, 0, 0)
        up.u.mi = MOUSEINPUT(0, 0, 0, 0x0004, 0, 0)
        _send(down, up)
        time.sleep(0.2)
        return f"clicked at {pt.x},{pt.y} on {_describe(under)}; focus now {_describe(_focus())}"
    if "layout" in action:
        hkl = user32.LoadKeyboardLayoutW(action["layout"], 1)
        # The layout is per thread: ask the window that has the keyboard focus (the webview's, in another
        # process), not the app's top-level window.
        target = _focus() or user32.GetForegroundWindow() or hwnd
        user32.PostMessageW(target, WM_INPUTLANGCHANGEREQUEST, 0, hkl)
        time.sleep(0.8)
        now = (user32.GetKeyboardLayout(user32.GetWindowThreadProcessId(target, None)) or 0) & 0xFFFFFFFF
        n = user32.GetKeyboardLayoutList(0, None)
        have = (ctypes.c_void_p * n)()
        user32.GetKeyboardLayoutList(n, have)
        listed = ", ".join(f"{(h or 0) & 0xFFFFFFFF:08x}" for h in have)
        return f"layout {action['layout']} asked of {_describe(target)}; it now has {now:08x} (installed: {listed})"
    if action.get("restore"):
        # Puts back what "focus" changed (Caps Lock).
        if _state["caps"] and not user32.GetKeyState(KEYS["CAPITAL"]) & 1:
            press(KEYS["CAPITAL"])
            _state["caps"] = False
            return "Caps Lock back on"
        return "nothing to restore"
    if "type" in action:
        for ch in action["type"]:
            press(ord(ch.upper()))
        return f"typed {action['type']}"
    if "chord" in action:
        vks = [KEYS[k] for k in action["chord"]]
        _send(*[_key(v) for v in vks])
        time.sleep(0.05)
        _send(*[_key(v, up=True) for v in reversed(vks)])
        time.sleep(0.3)
        return f"pressed {'+'.join(action['chord'])}"
    if "key" in action:
        press(KEYS[action["key"]])
        return f"pressed {action['key']}"
    raise ValueError(f"unknown action {json.dumps(action)}")
