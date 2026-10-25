#!/usr/bin/env python3
"""Container-only shell/PAM notification matrix; no camera or host PAM changes."""
import contextlib
import errno
import itertools
import json
import os
from pathlib import Path
import pty
import select
import shutil
import subprocess
import tempfile
import time

CONFIG = Path('/etc/facelock/config.toml')
SERVICE = Path('/etc/pam.d/facelock-notification-test')
BINARY = Path('/usr/bin/facelock')
ROOT = Path(__file__).resolve().parent


def invoke(interactive, flags):
    command = f'if [ -t 0 ]; then echo TTY=yes; else echo TTY=no; fi; case $- in *i*) echo SHELL=interactive;; *) echo SHELL=noninteractive;; esac; exec python3 {ROOT}/pam-notification-probe.py {flags}'
    args = ['bash', '--noprofile', '--norc', '-ic' if interactive else '-c', command]
    if interactive:
        master, slave = pty.openpty()
        try:
            process = subprocess.Popen(args, stdin=slave, stdout=slave, stderr=slave, preexec_fn=lambda: os.login_tty(0))
            os.close(slave)
            slave = None
            output = bytearray()
            deadline = time.monotonic() + 20
            while True:
                if time.monotonic() > deadline:
                    process.kill()
                    process.wait()
                    raise AssertionError('interactive PAM probe timed out')
                if select.select([master], [], [], .1)[0]:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        break
                    if not chunk:
                        break
                    output.extend(chunk)
            assert process.wait(timeout=2) == 0, output
            output = output.decode()
        finally:
            os.close(master)
            if slave is not None:
                os.close(slave)
    else:
        result = subprocess.run(args, input='', capture_output=True, text=True, timeout=20)
        assert result.returncode == 0, result.stderr
        output = result.stdout
    assert f'TTY={"yes" if interactive else "no"}' in output, output
    assert f'SHELL={"interactive" if interactive else "noninteractive"}' in output, output
    return json.loads(next(line for line in output.splitlines() if line.startswith('{')))


@contextlib.contextmanager
def daemon(work, legacy=False):
    calls = work / 'calls'
    calls.write_text('')
    args = ['python3', str(ROOT / 'fake-facelock-daemon.py'), '--calls', str(calls), '--outcome', str(work / 'outcome')]
    if not legacy:
        args.append('--silent')
    with (work / 'daemon.log').open('w+') as log:
        process = subprocess.Popen(args, stdout=log, stderr=log)
        try:
            for _ in range(100):
                log.seek(0)
                if 'fake facelock daemon ready' in log.read():
                    # The name is acquired asynchronously; wait for bus confirmation.
                    owner = subprocess.run(['dbus-send', '--system', '--print-reply', '--dest=org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus.NameHasOwner', 'string:org.facelock.Daemon'], capture_output=True, text=True)
                    if 'boolean true' in owner.stdout:
                        break
                assert process.poll() is None, 'fake daemon exited'
                time.sleep(.05)
            else:
                raise AssertionError('fake daemon did not acquire name')
            yield calls
        finally:
            process.terminate()
            process.wait(timeout=5)


def main():
    assert os.geteuid() == 0 and Path('/run/.containerenv').exists(), 'run only inside the disposable PAM container'
    assert not SERVICE.exists(), 'test service already exists'
    config = CONFIG.read_bytes()
    count = 0
    with tempfile.TemporaryDirectory(prefix='pam-notifications-') as directory:
        work = Path(directory)
        backup = work / 'facelock'
        shutil.copy2(BINARY, backup)
        marker = work / 'oneshot-calls'
        # A deterministic child proves success/failure feedback without a camera.
        stub = f'#!/bin/sh\necho called >> {marker}\nexit "$(cat {work}/exit)"\n'
        SERVICE.write_text('auth required pam_facelock.so\n')
        try:
            for transport in ['oneshot', 'daemon', 'legacy']:
                (work / 'outcome').write_text('true')
                manager = daemon(work, legacy=transport == 'legacy') if transport != 'oneshot' else contextlib.nullcontext(None)
                with manager as calls:
                    for matched in [True, False]:
                        (work / 'outcome').write_text(json.dumps(matched))
                        (work / 'exit').write_text('0' if matched else '1')
                        BINARY.write_text(stub)
                        BINARY.chmod(0o755)
                        for mode, prompt, success in itertools.product(['off', 'terminal', 'desktop', 'both'], [True, False], [True, False]):
                            CONFIG.write_text(f'[daemon]\nmode = "{"oneshot" if transport == "oneshot" else "daemon"}"\n[security]\nabort_if_ssh = false\nabort_if_lid_closed = false\n[notification]\nmode = "{mode}"\nnotify_prompt = {str(prompt).lower()}\nnotify_on_success = {str(success).lower()}\nnotify_on_failure = true\n')
                            for interactive in [True, False]:
                                for flags in [0, 1, 0x8000, 0x8001]:
                                    if calls:
                                        calls.write_text('')
                                    marker.unlink(missing_ok=True)
                                    actual = invoke(interactive, flags)
                                    silent = bool(flags & 0x8000)
                                    unsupported = transport == 'legacy' and silent
                                    expected = []
                                    if mode in ['terminal', 'both'] and not silent:
                                        if prompt:
                                            expected.append([4, 'Identifying face...'])
                                        if matched and success:
                                            expected.append([4, 'Face recognized.'])
                                    result = 6 if unsupported else (0 if matched else 7)
                                    label = f'{transport=} {matched=} {mode=} {interactive=} {flags=} {prompt=} {success=}'
                                    assert actual == {'result': result, 'messages': expected}, (label, actual, expected, result)
                                    if calls:
                                        expected_calls = [] if unsupported else ['AuthenticateSilent' if silent else 'Authenticate']
                                        assert calls.read_text().splitlines() == expected_calls, label
                                        assert not marker.exists(), f'unexpected oneshot fallback: {label}'
                                    else:
                                        assert marker.read_text() == 'called\n', label
                                    count += 1
            print(f'PASS: {count} shell/PAM notification cases (fake daemon and oneshot child; no desktop delivery claim)')
        finally:
            CONFIG.write_bytes(config)
            shutil.copy2(backup, BINARY)
            SERVICE.unlink(missing_ok=True)


if __name__ == '__main__':
    main()
