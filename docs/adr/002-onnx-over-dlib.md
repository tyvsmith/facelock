# ADR 002: ONNX Runtime over dlib

## Status

Accepted

## Date

2026-03-11

## Context

The primary open-source Linux face authentication project, Howdy, uses dlib for
face detection and recognition. dlib relies on Python bindings wrapping a large
C++ library. This creates several problems:

- **Build complexity.** dlib compilation from source can take 10-30 minutes and
  frequently hangs on resource-constrained systems. Pre-built wheels are not
  always available for all distributions and architectures.
- **Python overhead.** Each authentication attempt spawns a Python interpreter,
  adding 1-2 seconds of startup latency and approximately 200 MB of memory
  overhead.
- **Embedding quality.** dlib's ResNet model produces 128-dimensional embeddings.
  Modern ArcFace models produce 512-dimensional embeddings with significantly
  better discriminative power, particularly for edge cases (twins, aging, varied
  lighting).
- **GPU acceleration.** dlib's CUDA support requires compile-time configuration
  and specific toolkit versions. ONNX Runtime supports CUDA, TensorRT, ROCm, and
  OpenVINO through runtime-selectable execution providers.

## Decision

Use ONNX Runtime with SCRFD for face detection and ArcFace for face recognition,
accessed through the `ort` Rust crate.

## Alternatives Considered

### dlib via Rust FFI

Bind dlib directly from Rust, bypassing Python. Rejected because dlib's C++ API
is large and poorly suited to safe FFI. The 128-dim embedding limitation remains,
and the build complexity transfers to the Rust build system.

### MediaPipe

Google's MediaPipe offers face detection and mesh models. Rejected because its
face recognition capabilities are limited (no production-quality embedding
extraction), and the C++ library has heavy dependencies on Bazel and TensorFlow
Lite.

### Custom TensorFlow Lite integration

Use TFLite directly for model inference. Rejected because ONNX Runtime has
broader execution provider support, better Rust crate ecosystem (`ort`), and the
ONNX model format is the de facto standard for portable inference.

## Consequences

- **Pre-built model files required.** SCRFD and ArcFace ONNX models must be
  distributed alongside the binary or downloaded during installation. Models are
  not compiled into the binary.
- **Model integrity via SHA-256.** All model files are verified against known
  SHA-256 hashes at load time to prevent tampering or corruption.
- **`ort` crate dependency.** The `facelock-face` crate depends on `ort`, which
  bundles or links against ONNX Runtime shared libraries. Binary size increases
  but inference performance is substantially better than interpreted alternatives.
- **512-dim embeddings improve accuracy.** ArcFace embeddings provide stronger
  separation between genuine and impostor comparisons, reducing false accept and
  false reject rates compared to dlib's 128-dim model.
- **Portable GPU acceleration.** Users with NVIDIA or AMD GPUs can enable
  hardware acceleration without recompilation, by installing the appropriate ONNX
  Runtime execution provider.

## References

- [SCRFD: Sample and Computation Redistribution for Efficient Face Detection](https://arxiv.org/abs/2105.04714)
- [ArcFace: Additive Angular Margin Loss for Deep Face Recognition](https://arxiv.org/abs/1801.07698)
- [ONNX Runtime](https://onnxruntime.ai/)
- [ort crate](https://crates.io/crates/ort)
- [Howdy project](https://github.com/boltgolt/howdy)
