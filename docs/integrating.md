# Integrating Facelock

Desktop environments and distributions own their user experience: package
selection, menu entries, lock-screen UI, and the setup/removal wrapper. Facelock
owns the face-authentication backend, its PAM module, and the public commands
documented here. This boundary keeps a desktop integration releasable on its
own schedule without teaching Facelock about every desktop.

## Check the installed interface

Use `facelock capabilities`, not help-text scraping, to decide which features a
build offers. The command is unprivileged, reads no configuration, and does not
activate the daemon. It prints one stable capability name per line; a build
that predates the command exits non-zero and provides no capabilities.

```bash
capabilities=$(facelock capabilities 2>/dev/null) || capabilities=

has_facelock_capability() {
    printf '%s\n' "$capabilities" | grep -Fqx -- "$1"
}

for required in is-enrolled pam-if-present pam-json pam-multi-service pam-status setup-no-pam setup-systemd; do
    if ! has_facelock_capability "$required"; then
        printf '%s lacks required capability %s\n' \
            "$(facelock --version 2>/dev/null || printf 'facelock version unknown')" \
            "$required" >&2
        exit 1
    fi
done
```

If a downstream package also needs a minimum Facelock version, express that
dependency through its package manager and still run the capability check.
Versions identify a release; they do not prove features because distribution
backports and development builds exist. Report `facelock --version` in an error,
but branch on capability names.

For example, a Debian wrapper package can gate its tested baseline in
`debian/control` while its runtime script gates the actual interfaces as above:

```text
Depends: facelock (>= 0.2.0), ${misc:Depends}
```

## Show enrollment state

Run `facelock is-enrolled --quiet` as the logged-in user. Never run it through
`sudo`. The exit status is the interface:

| Status | Meaning | Integration response |
|--------|---------|----------------------|
| `0` | the user has a usable enrollment | show the face-auth affordance |
| `1` | the user is not enrolled or the marker is not usable | hide the affordance |
| `2` | the query could not be answered, such as bad arguments or an existing malformed marker | report a diagnostic or degrade without the affordance |

An absent marker and a permissions failure while opening it both return `1`
and, without `--quiet`, print `not-enrolled`. That is deliberate: a UI unable
to read the user's marker should hide its optional face affordance without
revealing enrollment state or blocking password authentication. Do not repair
permissions, parse the marker file, read the database, or activate the daemon
from a desktop probe. PAM remains authoritative when authentication actually
runs.

## Wire a PAM service

Package recipes install `pam_facelock.so` in a native PAM module directory.
Facelock discovers the module, in order, at `/lib/security`,
`/usr/lib/security`, and `/usr/lib64/security`; `pam add` refuses before writing
if no candidate exists. Use the module basename in a PAM rule rather than
copying or linking the library to a hard-coded path. An integrator can inspect
the resolved path without root through:

```bash
facelock pam status --service desktop-lock --json
```

`module_path` is the resolved candidate or `null`. The command's status is on
the same `0`/`1`/`2` scale as `grep`: present, missing, or unanswerable.

`facelock pam add` accepts any valid one-component service name; its built-in
menus are suggestions, not an allowlist. Once the downstream has supplied the
service file, configure one or several services in a single validated call:

```bash
sudo facelock pam add \
    --service desktop-lock \
    --service sudo \
    --service polkit-1 \
    --no-confirm
```

The writer locates the service using Linux-PAM precedence, validates every
requested service before the first write, and inserts
`auth sufficient pam_facelock.so` at the service's auth boundary. Keep a
password path outside the biometric lane. Never change the Facelock line to
`required`: an unavailable camera, timeout, rejection, or internal error must
fall through rather than lock the user out. Use `--if-present` when a service is
genuinely optional; it forgives absence, not a malformed or unreadable file.

Remove the same arbitrary services through the matching public command:

```bash
sudo facelock pam remove \
    --service desktop-lock \
    --service sudo \
    --service polkit-1 \
    --if-present \
    --no-confirm
```

## Public surface and internal details

For desktop integration, the stable public surface is:

- `facelock capabilities` and its additive capability names
- `facelock is-enrolled` and its exit codes
- `facelock pam add`, `remove`, and `status`, including their documented JSON
- the `pam_facelock.so` module basename and PAM's `sufficient` fallback model

The exact contracts are in [`contracts.md`](contracts.md). Source layout,
Rust crate APIs, help wording, database queries, marker contents, setup state,
and the daemon's activation sequence are implementation details. Do not call
`facelock auth` directly, use `facelock test` as a setup gate, scrape `--help`,
or reproduce Facelock's PAM editor. Clients that intentionally speak the D-Bus
protocol must follow the separate IPC contract rather than treating it as a
desktop convenience API.

## Hyprlock policy

`facelock hyprlock enable|disable|status` remains available for existing
Hyprlock users. It is frozen compatibility surface, not a template for new
desktop adapters. Facelock will not add parallel compositor-specific setup
commands. A new desktop should use the generic PAM and enrollment commands
above and keep its UI/configuration edits in the downstream project.

## Worked Omarchy integration

Omarchy owns its setup/removal commands, lock-screen UI, and backend-neutral
PAM service. Facelock packages do not install Omarchy wrappers. The division of
work is:

1. Omarchy installs or selects a Facelock package, then checks the capability
   names its wrapper uses.
2. Facelock's wizard configures models, encryption, the daemon, and enrollment
   without editing PAM:

   ```bash
   sudo facelock setup --no-pam --systemd
   ```

3. Omarchy creates `/etc/pam.d/omarchy-lock-face` as its own backend-neutral
   service:

   ```pam
   #%PAM-1.0
   auth       required                    pam_deny.so
   account    include                     system-local-login
   ```

   Omarchy refuses to overwrite a service already owned by another backend.
   Its lock shell runs password authentication in a separate PAM context, so
   `pam_deny.so` intentionally terminates this face-only lane.

4. Facelock inserts its own `sufficient` rule above that terminator and also
   configures the other services Omarchy selected:

   ```bash
   sudo facelock pam add \
       --service omarchy-lock-face \
       --service sudo \
       --service polkit-1 \
       --no-confirm
   ```

5. Omarchy decides which completion message to show from the enrollment probe,
   without turning a non-enrollment into a failed setup:

   ```bash
   if facelock is-enrolled --quiet; then
       echo 'Face authentication is configured.'
   else
       status=$?
       case $status in
           1) echo 'Facelock is configured; enroll a face when ready.' ;;
           2) echo 'Facelock enrollment state could not be read.' >&2 ;;
       esac
   fi
   ```

6. On removal, Omarchy asks Facelock to withdraw only the lines Facelock owns,
   then removes its backend-neutral service only after confirming no other
   backend still uses it:

   ```bash
   sudo facelock pam remove \
       --service omarchy-lock-face \
       --service sudo \
       --service polkit-1 \
       --if-present \
       --no-confirm
   ```

Omarchy can change its menu, lock behavior, notifications, and package choice
without a Facelock release. Facelock can change its internal implementation
while the capability, probe, and PAM contracts keep the downstream wrapper
working.
