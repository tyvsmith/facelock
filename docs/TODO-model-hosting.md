# TODO: Self-Host Model Files

## Current State

Models are downloaded from third-party GitHub releases (visomaster/visomaster-assets).
This works for development but is fragile — the upstream repo could disappear.

## Action Items

1. Create a GitHub release on the facelock repo (e.g. `v0.1.0-models`)
2. Upload these files to the release:
   - `scrfd_2.5g_bnkps.onnx` (~3MB) — SHA256: `bc24bb349491481c3ca793cf89306723162c280cb284c5a5e49df3760bf5c2ce`
   - `w600k_r50.onnx` (~166MB) — SHA256: `4c06341c33c2ca1f86781dab0e829f88ad5b64be9fba56e56bc9ebdefc619e43`
3. Update `models/manifest.toml` URLs to point to your own release
4. Optionally add the higher-accuracy optional models:
   - `scrfd_10g_bnkps.onnx` (~16MB)
   - `w600k_r100.onnx` (~249MB)

## License Note

InsightFace models are licensed for **non-commercial research purposes only**.
Review licensing implications before distributing.
