# Spec 11: Benchmarks & Calibration

**Phase**: 6 (Validation) | **Crate**: howdy-bench | **Depends on**: all prior

## Goal

Benchmark tooling to measure auth latency, preview performance, and face matching accuracy. Provides threshold calibration to back shipped defaults with evidence.

## Dependencies

- `howdy-core`, `howdy-camera`, `howdy-face`, `howdy-store`

## Benchmark Commands

```
howdy-bench <command>

Commands:
  cold-auth       Measure cold auth latency (daemon startup + first auth)
  warm-auth       Measure warm auth latency (daemon already running)
  preview         Measure preview frame latency
  enrollment      Measure enrollment time
  model-load      Measure ONNX model load time
  calibrate       Sweep thresholds and measure FAR/FRR
  report          Generate benchmark report
```

## Performance Targets

| Metric | Target | Method |
|--------|--------|--------|
| Cold auth (daemon start + first auth) | < 3 seconds | `cold-auth` |
| Warm auth | < 450ms | `warm-auth` |
| Preview frame latency | < 120ms | `preview` |
| Enrollment (5 snapshots) | < 12 seconds | `enrollment` |
| ONNX model load (SCRFD + ArcFace) | < 2 seconds | `model-load` |

## Calibration

### Threshold Sweep

Vary `recognition.threshold` from 0.2 to 0.8 in steps of 0.05:
- For each threshold: measure true positives, false positives, true negatives, false negatives
- Requires: enrolled user + test images (same person, different person)
- Output: FAR/FRR curve, recommended threshold

### Detector Confidence Sweep

Vary `recognition.detection_confidence` from 0.3 to 0.9:
- Measure: detection rate, false detection rate
- Output: detection performance curve

## Report Format

Use `templates/benchmark-report.md` template:

```markdown
## Environment
- Hardware: ...
- CPU: ...
- Distribution: ...
- Model pack: SCRFD 2.5G + ArcFace R50
- Build: release

## Results
| Metric | Value | Target | Pass? |
|--------|-------|--------|-------|
| Cold auth | Xms | <3000ms | ... |
| Warm auth | Xms | <450ms | ... |
| ...

## Calibration
- Recommended threshold: X.XX
- Evidence: ...

## Notes
- ...
```

## Acceptance Criteria

1. All benchmark commands exist and run
2. Benchmark results generated in documented format
3. Calibration produces threshold recommendation
4. Shipped defaults backed by measured results

## Verification

```bash
cargo build -p howdy-bench
cargo run --bin howdy-bench -- --help
```
