#!/usr/bin/env python3
"""Run pam_authenticate twice from one process, pausing between the two.

Usage: pam-auth-twice.py SERVICE USER GATE_FILE

Exists because pamtester cannot do this: it accepts several operations in one
invocation but stops at the first one that fails, and the ADR 008 assertion
that uses this harness (`adr008_live_client_timeout_cancels` in
test/run-integration-tests.sh) needs the first attempt to fail by timing out
and the second to run anyway, in the same process, the way a screen locker or
sudo retries.

One PAM handle, two `pam_authenticate` calls. After the first one this waits
for GATE_FILE to exist, so the test decides when the second request reaches
the daemon. Each result is printed as `attempt N: RC MESSAGE` and flushed at
once, which is what the test watches. Exit 0 means both attempts ran; their
outcome is in the output, not the exit code. Exit 2 is a harness failure.

Nothing here but the standard library and libpam, so the image needs no new
package.
"""

import ctypes
import ctypes.util
import os
import sys
import time

PAM_PROMPT_ECHO_OFF = 1
PAM_PROMPT_ECHO_ON = 2
PAM_ERROR_MSG = 3
PAM_TEXT_INFO = 4
PAM_SUCCESS = 0
PAM_BUF_ERR = 5

GATE_WAIT_SECS = 120


class PamMessage(ctypes.Structure):
    _fields_ = [("msg_style", ctypes.c_int), ("msg", ctypes.c_char_p)]


class PamResponse(ctypes.Structure):
    _fields_ = [("resp", ctypes.c_void_p), ("resp_retcode", ctypes.c_int)]


CONV_FUNC = ctypes.CFUNCTYPE(
    ctypes.c_int,
    ctypes.c_int,
    ctypes.POINTER(ctypes.POINTER(PamMessage)),
    ctypes.POINTER(ctypes.POINTER(PamResponse)),
    ctypes.c_void_p,
)


class PamConv(ctypes.Structure):
    _fields_ = [("conv", CONV_FUNC), ("appdata_ptr", ctypes.c_void_p)]


libc = ctypes.CDLL(ctypes.util.find_library("c"))
libc.calloc.restype = ctypes.c_void_p
libc.calloc.argtypes = [ctypes.c_size_t, ctypes.c_size_t]
libc.strdup.restype = ctypes.c_void_p
libc.strdup.argtypes = [ctypes.c_char_p]

libpam = ctypes.CDLL(ctypes.util.find_library("pam") or "libpam.so.0")
libpam.pam_start.argtypes = [
    ctypes.c_char_p,
    ctypes.c_char_p,
    ctypes.POINTER(PamConv),
    ctypes.POINTER(ctypes.c_void_p),
]
libpam.pam_authenticate.argtypes = [ctypes.c_void_p, ctypes.c_int]
libpam.pam_strerror.argtypes = [ctypes.c_void_p, ctypes.c_int]
libpam.pam_strerror.restype = ctypes.c_char_p
libpam.pam_end.argtypes = [ctypes.c_void_p, ctypes.c_int]


def converse(num_msg, msgs, resp, _appdata):
    """Echo informational messages; answer any prompt with an empty string.

    Linux-PAM passes an array of message pointers. The response array and
    its strings are freed by libpam, so both come from libc's allocator.
    """
    responses = libc.calloc(num_msg, ctypes.sizeof(PamResponse))
    if not responses:
        return PAM_BUF_ERR
    array = ctypes.cast(responses, ctypes.POINTER(PamResponse))
    for i in range(num_msg):
        msg = msgs[i].contents
        text = (msg.msg or b"").decode(errors="replace")
        if msg.msg_style in (PAM_PROMPT_ECHO_OFF, PAM_PROMPT_ECHO_ON):
            array[i].resp = libc.strdup(b"")
        else:
            print(f"pam: {text}", flush=True)
    resp[0] = array
    return PAM_SUCCESS


def attempt(pamh, n):
    rc = libpam.pam_authenticate(pamh, 0)
    message = (libpam.pam_strerror(pamh, rc) or b"").decode(errors="replace")
    print(f"attempt {n}: {rc} {message}", flush=True)
    return rc


def main():
    if len(sys.argv) != 4:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    service, user, gate = sys.argv[1:]

    conv = PamConv(CONV_FUNC(converse), None)
    pamh = ctypes.c_void_p()
    rc = libpam.pam_start(service.encode(), user.encode(), ctypes.byref(conv), ctypes.byref(pamh))
    if rc != PAM_SUCCESS:
        print(f"pam_start failed: {rc}", file=sys.stderr)
        return 2

    attempt(pamh, 1)

    deadline = time.monotonic() + GATE_WAIT_SECS
    while not os.path.exists(gate):
        if time.monotonic() >= deadline:
            print(f"gate {gate} never appeared", file=sys.stderr)
            libpam.pam_end(pamh, rc)
            return 2
        time.sleep(0.05)

    rc = attempt(pamh, 2)
    libpam.pam_end(pamh, rc)
    return 0


if __name__ == "__main__":
    sys.exit(main())
