#!/usr/bin/env python3
"""Fake org.facelock.Daemon that always replies matched=true.

Container-test harness for the PAM peer-UID check (plan 05): it is run as an
unprivileged user (with a deliberately loosened test bus policy) to prove
that pam_facelock refuses to trust an Authenticate reply from a bus-name
owner that is not running as root — even when that reply claims a match.
"""

import argparse
import json

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

OPTIONS = None

INTROSPECTION_XML = """
<node>
  <interface name="org.facelock.Daemon">
    <method name="Authenticate">
      <arg type="s" name="user" direction="in"/>
      <arg type="(bisd)" name="result" direction="out"/>
    </method>
  </interface>
</node>
"""


def on_method_call(_conn, _sender, _path, _iface, method, _params, invocation):
    if method in ("Authenticate", "AuthenticateSilent"):
        if OPTIONS.calls:
            with open(OPTIONS.calls, "a") as calls:
                calls.write(method + "\n")
        matched = True
        if OPTIONS.outcome:
            with open(OPTIONS.outcome) as outcome:
                matched = json.load(outcome)

        # matched=true, model_id=1, label="fake", similarity=0.999
        invocation.return_value(
            GLib.Variant("((bisd))", ((matched, 1 if matched else -4, "fake", 0.999 if matched else 0.0),))
        )
    else:
        invocation.return_dbus_error(
            "org.freedesktop.DBus.Error.UnknownMethod", f"no such method: {method}"
        )


def main():
    global OPTIONS
    parser = argparse.ArgumentParser()
    parser.add_argument("--silent", action="store_true")
    parser.add_argument("--calls")
    parser.add_argument("--outcome")
    OPTIONS = parser.parse_args()
    xml = INTROSPECTION_XML
    if OPTIONS.silent:
        xml = xml.replace('</interface>', '<method name="AuthenticateSilent"><arg type="s" name="user" direction="in"/><arg type="(bisd)" name="result" direction="out"/></method></interface>')
    node = Gio.DBusNodeInfo.new_for_xml(xml)
    conn = Gio.bus_get_sync(Gio.BusType.SYSTEM, None)
    conn.register_object(
        "/org/facelock/Daemon", node.interfaces[0], on_method_call, None, None
    )
    Gio.bus_own_name_on_connection(
        conn, "org.facelock.Daemon", Gio.BusNameOwnerFlags.NONE, None, None
    )
    print("fake facelock daemon ready", flush=True)
    GLib.MainLoop().run()


if __name__ == "__main__":
    main()
