# GPU Acceleration

GPU support in Facelock is **runtime-only** -- Facelock itself needs no rebuild.
Install an ONNX Runtime built with the matching execution provider, satisfy the
vendor driver/runtime requirements, and set `execution_provider` in the
configuration. GPU paths are configuration-supported but are not part of the
release package validation matrix.

## Setup

### 1. Install a GPU-enabled ONNX Runtime

| GPU Vendor | Arch Linux Package | Other Distros |
|------------|-------------------|---------------|
| NVIDIA | `onnxruntime-opt-cuda` | Install a compatible NVIDIA driver and ONNX Runtime with CUDA support |
| AMD | `onnxruntime-opt-rocm` | Install the ROCm stack and ONNX Runtime with ROCm support |
| Intel | none packaged | Build ONNX Runtime with the OpenVINO provider and install its required OpenVINO runtime |

On Arch Linux, these are official Extra repository packages as checked on
2026-09-05. Each provides and conflicts with the virtual `onnxruntime`
dependency, so it replaces `onnxruntime-cpu` rather than installing beside it:

```bash
sudo pacman -S onnxruntime-opt-cuda      # NVIDIA
sudo pacman -S onnxruntime-opt-rocm      # AMD
```

Arch packages no OpenVINO build of ONNX Runtime, in the repositories or the AUR
as checked on 2026-09-05. Build ONNX Runtime with the OpenVINO execution
provider yourself to use `execution_provider = "openvino"`. Debian and Ubuntu
also provide no ONNX Runtime package; Facelock's published `.deb` packages bundle
a CPU-only runtime and therefore do not enable a GPU provider. Fedora's COPR
package depends on Fedora's CPU-only `onnxruntime` package.

### 2. Set the execution provider

Either let setup detect it:

```bash
sudo facelock setup --execution-provider=auto
```

`auto` asks the installed ONNX Runtime which providers it was built with and selects `cuda` > `rocm` > `openvino` > `cpu`, printing what it found either way. See [`--execution-provider=auto`](cli-reference.md#execution-provider-auto) in the CLI reference.

Or set it yourself in `/etc/facelock/config.toml`:

```toml
[recognition]
execution_provider = "cuda"    # or "rocm" or "openvino"
```

### 3. Restart the daemon

```bash
sudo facelock daemon restart
```

### 4. Verify

```bash
sudo facelock status
sudo facelock bench warm-auth
```

`facelock status` must report that the configured provider is built into the
installed ONNX Runtime. Then compare the benchmark with
`execution_provider = "cpu"`; timing alone is not proof that the GPU provider
loaded.

## How it works

Facelock uses the `ort` crate with the `load-dynamic` feature. It accepts ONNX
Runtime 1.20+ from fixed, root-owned system and Facelock package directories;
privileged commands ignore `ORT_DYLIB_PATH`. For a configured GPU provider it
prefers the trusted system runtime over Facelock's bundled CPU runtime. The
`execution_provider` config selects which provider to register.

If the configured provider is not built into the installed runtime, status and
daemon startup warn about the mismatch. ONNX Runtime may fall back to CPU, so
do not infer GPU use merely from a successful authentication.

## Supported providers

| Provider | Config value | Status |
|----------|-------------|--------|
| CPU | `"cpu"` | Default; covered by package validation |
| CUDA (NVIDIA) | `"cuda"` | Config supported, requires CUDA-enabled ORT; not release-matrix tested |
| ROCm (AMD) | `"rocm"` | Config supported, requires ROCm-enabled ORT; not release-matrix tested |
| OpenVINO (Intel) | `"openvino"` | Config supported, requires a custom OpenVINO-enabled ORT; not release-matrix tested |

## systemd note

The systemd service has `MemoryDenyWriteExecute=yes` commented out because GPU inference runtimes (CUDA, TensorRT) use JIT compilation which requires writable+executable memory pages. If you are using CPU-only, you can re-enable this directive for additional hardening.

## Troubleshooting

- **"Failed to load execution provider"**: The GPU-enabled ONNX Runtime package is not installed or `libonnxruntime.so` does not include the requested provider.
- **Slower than CPU**: Ensure the GPU driver is loaded (`nvidia-smi` for NVIDIA, `rocm-smi` for AMD). Small models like SCRFD 2.5G may not benefit from GPU due to transfer overhead.
- **Daemon exits during startup**: Check `journalctl -u facelock-daemon` and diagnose the reported error; runtime loading, provider initialization and device-memory failures require different remedies.
