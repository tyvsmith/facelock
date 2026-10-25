#!/usr/bin/env python3
"""Record the real PAM conversation and result without prompting for input."""
import ctypes
import ctypes.util
import json
import sys


class Message(ctypes.Structure):
    _fields_ = [("style", ctypes.c_int), ("text", ctypes.c_char_p)]


class Response(ctypes.Structure):
    _fields_ = [("text", ctypes.c_void_p), ("code", ctypes.c_int)]


Callback = ctypes.CFUNCTYPE(
    ctypes.c_int, ctypes.c_int, ctypes.POINTER(ctypes.POINTER(Message)),
    ctypes.POINTER(ctypes.POINTER(Response)), ctypes.c_void_p,
)


class Conversation(ctypes.Structure):
    _fields_ = [("callback", Callback), ("data", ctypes.c_void_p)]


messages = []


@Callback
def converse(count, incoming, _responses, _data):
    for index in range(count):
        message = incoming[index].contents
        messages.append([message.style, (message.text or b"").decode()])
        if message.style in (1, 2):
            return 19  # PAM_CONV_ERR: this probe never supplies credentials
    return 0


pam = ctypes.CDLL(ctypes.util.find_library("pam"), mode=ctypes.RTLD_GLOBAL)
pam.pam_start.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.POINTER(Conversation), ctypes.POINTER(ctypes.c_void_p)]
pam.pam_authenticate.argtypes = [ctypes.c_void_p, ctypes.c_int]
pam.pam_end.argtypes = [ctypes.c_void_p, ctypes.c_int]
handle = ctypes.c_void_p()
conversation = Conversation(converse, None)
rc = pam.pam_start(b"facelock-notification-test", b"testuser", ctypes.byref(conversation), ctypes.byref(handle))
if rc:
    raise RuntimeError(f"pam_start: {rc}")
try:
    rc = pam.pam_authenticate(handle, int(sys.argv[1], 0))
finally:
    pam.pam_end(handle, rc)
print(json.dumps({"result": rc, "messages": messages}))
