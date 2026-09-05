# GPU Acceleration

GPU support in Facelock is **runtime-only** -- no special build flags or recompilation needed. Install a GPU-enabled ONNX Runtime package for your hardware and set `execution_provider` in the configuration.

## Setup

### 1. Install a GPU-enabled ONNX Runtime

| GPU Vendor | Arch Linux Package | Other Distros |
|------------|-------------------|---------------|
| NVIDIA | `onnxruntime-opt-cuda` | Install CUDA toolkit + ONNX Runtime with CUDA provider |
| AMD | `onnxruntime-opt-rocm` | Install ROCm runtime + ONNX Runtime with ROCm provider |
| Intel | none packaged | Install OpenVINO runtime + ONNX Runtime with OpenVINO provider |

On Arch Linux:

```bash
sudo pacman -S onnxruntime-opt-cuda      # NVIDIA
sudo pacman -S onnxruntime-opt-rocm      # AMD
```

Arch packages no OpenVINO build of ONNX Runtime, in the repositories or the AUR. Build ONNX Runtime with the OpenVINO execution provider yourself to use `execution_provider = "openvino"`.

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
sudo facelock bench warm-auth
```

Compare latency with `execution_provider = "cpu"` to confirm GPU acceleration is active.

## How it works

Facelock uses the `ort` crate with the `load-dynamic` feature. At startup, it loads `libonnxruntime.so` from the system library path. If a GPU-enabled ONNX Runtime is installed, it provides CUDA/ROCm/OpenVINO execution providers automatically. The `execution_provider` config selects which provider to register.

If the requested provider is not available (e.g., CUDA requested but only CPU ORT installed), Facelock falls back to CPU with a warning.

## Supported providers

| Provider | Config value | Status |
|----------|-------------|--------|
| CPU | `"cpu"` | Default, tested |
| CUDA (NVIDIA) | `"cuda"` | Config ready, requires GPU-enabled ORT |
| ROCm (AMD) | `"rocm"` | Config ready, requires GPU-enabled ORT |
| OpenVINO (Intel) | `"openvino"` | Config ready, requires GPU-enabled ORT |

## systemd note

The systemd service has `MemoryDenyWriteExecute=yes` commented out because GPU inference runtimes (CUDA, TensorRT) use JIT compilation which requires writable+executable memory pages. If you are using CPU-only, you can re-enable this directive for additional hardening.

## Troubleshooting

- **"Failed to load execution provider"**: The GPU-enabled ONNX Runtime package is not installed or `libonnxruntime.so` does not include the requested provider.
- **Slower than CPU**: Ensure the GPU driver is loaded (`nvidia-smi` for NVIDIA, `rocm-smi` for AMD). Small models like SCRFD 2.5G may not benefit from GPU due to transfer overhead.
- **Daemon crashes on startup**: Check `journalctl -u facelock-daemon` for ORT initialization errors. GPU memory allocation failures are the most common cause.
