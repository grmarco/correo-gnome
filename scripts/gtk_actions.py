import ctypes, sys, time
x = ctypes.CDLL("libX11.so.6"); t = ctypes.CDLL("libXtst.so.6")
x.XOpenDisplay.restype = ctypes.c_void_p
d = ctypes.c_void_p(x.XOpenDisplay(None))
root = x.XDefaultRootWindow(d)
x.XStringToKeysym.restype = ctypes.c_ulong
def kc(n): return x.XKeysymToKeycode(d, ctypes.c_ulong(x.XStringToKeysym(n.encode())))
def key(combo):
    ks = combo.split("+")
    for k in ks: t.XTestFakeKeyEvent(d, kc(k), 1, 0)
    for k in reversed(ks): t.XTestFakeKeyEvent(d, kc(k), 0, 0)
    x.XFlush(d); time.sleep(0.5)
names={'@':'at','.':'period',',':'comma',' ':'space','-':'minus','_':'underscore','\n':'Return',':':'colon'}
x.XKeycodeToKeysym.restype = ctypes.c_ulong
def typ(text):
    for ch in text:
        ksn = names.get(ch, ch)
        ks = x.XStringToKeysym(ksn.encode())
        code = x.XKeysymToKeycode(d, ctypes.c_ulong(ks))
        base = x.XKeycodeToKeysym(d, code, 0)
        shift = base != ks
        if shift: t.XTestFakeKeyEvent(d, kc("Shift_L"), 1, 0)
        t.XTestFakeKeyEvent(d, code, 1, 0); t.XTestFakeKeyEvent(d, code, 0, 0)
        if shift: t.XTestFakeKeyEvent(d, kc("Shift_L"), 0, 0)
        x.XFlush(d); time.sleep(0.05)
    time.sleep(0.5)
def click(px, py, btn=1):
    x.XWarpPointer(d, 0, ctypes.c_ulong(root), 0,0,0,0, int(px), int(py)); x.XFlush(d); time.sleep(0.2)
    t.XTestFakeButtonEvent(d, btn, 1, 0); t.XTestFakeButtonEvent(d, btn, 0, 0); x.XFlush(d); time.sleep(0.8)
def resize(w, h):
    rr, pr, n = ctypes.c_ulong(), ctypes.c_ulong(), ctypes.c_uint()
    ch = ctypes.POINTER(ctypes.c_ulong)()
    x.XQueryTree(d, ctypes.c_ulong(root), ctypes.byref(rr), ctypes.byref(pr), ctypes.byref(ch), ctypes.byref(n))
    for i in range(n.value):
        x.XResizeWindow(d, ctypes.c_ulong(ch[i]), w, h)
    x.XFlush(d); time.sleep(1.5)
x.XWarpPointer(d, 0, ctypes.c_ulong(root), 0,0,0,0, 600, 400); x.XFlush(d)
import os
_log = os.environ.get("GTK_ACTIONS_LOG")
for a in sys.argv[1:]:
    if _log:
        with open(_log, "a") as f: f.write(time.strftime("%H:%M:%S ") + a + "\n")
    p = a.split(":")
    if p[0] == "click": click(p[1], p[2])
    elif p[0] == "rclick": click(p[1], p[2], 3)
    elif p[0] == "key": key(p[1])
    elif p[0] == "type": typ(":".join(p[1:]))
    elif p[0] == "resize": resize(int(p[1]), int(p[2]))
    elif p[0] == "wait": time.sleep(float(p[1]))
