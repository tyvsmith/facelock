# Synthetic face fixture — provenance

The face the loopback tier replays is rendered procedurally by
`crates/facelock-test-support/src/synthetic_face.rs`: ellipses, arcs and
shading expressions over a deterministic hash-noise lattice. No photograph,
scan, dataset sample or generative-model output was used as input or
reference at any point. The identity it encodes belongs to nobody, and no
biometric of a real person is stored in this repository or produced by the
tier.

Nothing under `test/loopback/` is a downloaded asset; there is no third-party
licence to record. The rendered frames are not committed — the tier regenerates
them on every run (`facelock-synth-face`), so the fixture's source of truth is
the renderer and its unit tests, and the bands it has to sit in against the
real models are pinned by
`crates/facelock-daemon/tests/synthetic_face_contract.rs`.

Keep it that way: a replacement fixture must be procedural too, or a work
whose licence and synthetic origin are recorded here.
