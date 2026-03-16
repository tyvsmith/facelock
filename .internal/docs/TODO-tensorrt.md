# TODO: TensorRT Testing

## Status
Not yet tested — TensorRT SDK is not currently installed on the dev machine.

## Prerequisites
- NVIDIA GPU with CUDA support (already working)
- CUDA toolkit (already installed)
- TensorRT SDK (`libnvinfer`): install via `yay -S libnvinfer` (AUR) or from NVIDIA

## What to test
1. `just install-tensorrt` — builds with `--features cuda,tensorrt` and sets config
2. First inference latency — TensorRT builds an optimized engine on first run (expect 30-120s)
3. Subsequent inference latency — should be ~1-3ms per SCRFD/ArcFace vs ~3-8ms CUDA
4. Engine caching — verify TRT engine cache is created and reused across daemon restarts
5. Verify fallback behavior when TensorRT fails (should fall back to CUDA or CPU)
6. Test with both SCRFD models (2.5g and 10g) and both ArcFace models (R50 and R100)

## Notes
- TensorRT engine caches are GPU-architecture specific — won't work if GPU changes
- The `tensorrt` feature includes `cuda` implicitly in the build targets
- Current CUDA performance is already 0.12s end-to-end, so TensorRT gains may be marginal
