# ADR 003: Rust PAM Module (No Python)

## Status

Accepted

## Date

2026-03-11

## Context

Existing Linux face authentication solutions take different approaches to PAM
integration:

- **Howdy** implements a PAM module in C++ that spawns a Python process for each
  authentication attempt. This adds 1-2 seconds of startup latency (Python
  interpreter initialization, library imports) and approximately 200 MB of memory
  per authentication. The C++ PAM shim is thin but the real logic lives in Python.
- **fprintd** implements its PAM module in C++ and communicates with a D-Bus
  daemon. The PAM module is well-contained but requires manual memory management
  and is susceptible to the usual C++ safety pitfalls.

Facelock needs a PAM module that is fast to load, memory-efficient, and resistant
to memory safety bugs. PAM modules run in the address space of security-critical
processes (login, sudo, sshd, screen lockers), so crashes or memory corruption
have outsized impact.

## Decision

Implement the PAM module as a Rust `cdylib` crate (`pam-facelock`) that compiles
to `pam_facelock.so`. The module loads directly into the PAM consumer process
with no interpreter startup, no subprocess spawning for the common daemon path,
and memory safety guaranteed by the Rust compiler.

## Alternatives Considered

### C PAM module

Write the PAM module in plain C, following the traditional approach. Rejected
because C lacks memory safety guarantees. PAM modules are high-value attack
surface; manual buffer management in C is an unnecessary risk when Rust can
produce equivalent `cdylib` output.

### C++ PAM module spawning Python (Howdy model)

Replicate Howdy's architecture. Rejected due to the performance and memory
overhead described above. Spawning an interpreter per auth attempt is
fundamentally at odds with the goal of sub-second authentication.

### Go shared library

Go can produce C-compatible shared libraries via `cgo`. Rejected because Go's
runtime (garbage collector, goroutine scheduler) is inappropriate for a library
loaded into arbitrary host processes. The Go runtime's signal handling conflicts
with many PAM consumers.

## Consequences

- **FFI boundary requires `catch_unwind`.** Rust panics must not unwind across
  the FFI boundary into the C PAM consumer. All PAM entry points
  (`pam_sm_authenticate`, `pam_sm_setcred`, etc.) wrap their logic in
  `std::panic::catch_unwind` and return `PAM_SYSTEM_ERR` on panic.
- **Limited to PAM-safe operations.** The module must not perform operations that
  could deadlock or corrupt the host process. No global allocator replacement, no
  signal handler installation, minimal use of threading.
- **Restricted dependency set.** The PAM module crate depends only on `libc`,
  `toml`, and `serde` to minimize binary size and attack surface. All heavy
  lifting (camera access, inference, database queries) happens in the daemon or
  the `facelock auth` subprocess.
- **No unwinding across FFI boundary.** The crate is compiled with
  `panic = "abort"` as a defense-in-depth measure alongside `catch_unwind`,
  ensuring that any missed panic path terminates rather than corrupting the host.
- **Direct loading eliminates startup latency.** The shared library is mapped
  into the process by the dynamic linker with negligible overhead compared to
  spawning a Python interpreter.

## References

- [Linux-PAM Module Writers' Guide](http://www.linux-pam.org/Linux-PAM-html/Linux-PAM_MWG.html)
- [Rust FFI Omnibus](http://jakegoulding.com/rust-ffi-omnibus/)
- [Howdy PAM module](https://github.com/boltgolt/howdy/tree/master/howdy/src/pam)
- [fprintd PAM module](https://gitlab.freedesktop.org/libfprint/fprintd/-/tree/master/pam)
