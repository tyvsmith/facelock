# Testing and Safety

Unit, lint, audit, documentation, and contract checks need no host
authentication changes:

```bash
just check
```

Camera tests, package lifecycle tests, and live authentication have different
risk. Use containers for isolated PAM smoke coverage and an explicitly marked
disposable guest for booted package/login scenarios. The walkthrough runner
does not provision the guest and refuses ordinary hosts. See the canonical
[Testing and Safety](../../docs/testing-safety.md) and
[Testing Walkthrough](../../docs/testing-walkthrough.md).

The development configuration is not rootless. Management commands retain
their root gate, and effective-UID-0 processes ignore `FACELOCK_CONFIG`:

```bash
just build
just link-models
sudo target/debug/facelock --config "$PWD/dev/config.toml" devices
```

Do not use development setup as a shortcut into host PAM. Before any host PAM
test, validate the isolated tiers, retain a separate root shell, start with
`sudo`, and test from a new terminal. Prefer validated removal:

```bash
facelock pam add --service sudo
facelock pam remove --service sudo
```

Run those two commands from the retained root shell. Current managed backups
are versioned beneath `/var/lib/facelock/pam-backups/` and carry JSON
provenance. Facelock does not automatically create
`/etc/pam.d/sudo.facelock-backup`; that path exists only if an operator or an
older release made it. Review any copy before restoring it.

`facelock test` can return zero without a match or camera scan. Treat its human
output, not exit status alone, as the result.
