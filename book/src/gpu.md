# GPU Acceleration

GPU support in Facelock is **runtime-only** -- no special build flags or recompilation needed. Install a GPU-enabled ONNX Runtime package for your hardware and set `execution_provider` in the configuration.

## Setup

### 1. Install a GPU-enabled ONNX Runtime

| GPU Vendor | Arch Linux Package | Other Distros |
|------------|-------------------|---------------|
| NVIDIA | `onnxruntime-opt-cuda` | Install CUDA toolkit + ONNX Runtime with CUDA provider |
| AMD | `onnxruntime-opt-rocm` | Install ROCm runtime + ONNX Runtime with ROCm provider |
| Intel | `onnxruntime-opt-openvino` | Install OpenVINO runtime + ONNX Runtime with OpenVINO provider |

On Arch Linux:

```bash
sudo pacman -S onnxruntime-opt-cuda      # NVIDIA
sudo pacman -S onnxruntime-opt-rocm      # AMD
sudo pacman -S onnxruntime-opt-openvino  # Intel
```

### 2. Set the execution provider

Edit `/etc/facelock/config.toml`:

```toml
[recognition]
execution_provider = "cuda"    # or "rocm" or "openvino"
```

### 3. Restart the daemon

```bash
facelock restart
```

### 4. Verify

```bash
facelock bench warm-auth
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
