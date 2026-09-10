---
paths:
  - "crates/pam-facelock/**"
---

# PAM Dependency Ceiling

`pam-facelock` depends on **libc, toml, serde, zbus ONLY**. No ort, no v4l, no
facelock-core.

Why: this keeps the shared library small and avoids dragging heavy dependencies
into every PAM-using process. The module is a thin client that either connects
to the daemon or spawns `facelock auth`, so inference and camera code belong on
the other side of that boundary, never linked into `pam_facelock.so`.

## zbus must use the tokio backend

The crate list above is not the whole constraint. `just check-pam-standalone`
fails the build if the dependency tree contains any of:

    async-io  async-signal  async-executor  async-fs  async-lock  polling

Those are the `async-io` backend. zbus must be built with the tokio backend and
without default features. Satisfying the four-crate ceiling above while pulling
in the wrong zbus feature set still fails CI — this has already cost two fixes
(#107, #123), because the ceiling was documented and the backend was not.

Run `just check-pam-standalone` after any change to `crates/pam-facelock/Cargo.toml`.
CI runs the same recipe as the "Build pam-facelock in isolation and verify its
dependency surface" step in `build-and-test`.
