# Benchmark Report

## Environment
- **Hardware**: (laptop model)
- **CPU**: (model, cores)
- **Distribution**: (e.g., CachyOS)
- **Kernel**: (version)
- **Model Pack**: SCRFD 2.5G + ArcFace R50
- **Build**: release
- **Date**: YYYY-MM-DD

## Latency Results

| Metric | Value | Target | Pass? |
|--------|-------|--------|-------|
| Cold auth (daemon start + first auth) | ms | <3000ms | |
| Warm auth | ms | <450ms | |
| Preview frame latency | ms | <120ms | |
| Enrollment (5 snapshots) | s | <12s | |
| ONNX model load | ms | <2000ms | |

## Calibration Results

| Threshold | True Positive Rate | False Positive Rate | Notes |
|-----------|-------------------|--------------------|----|
| 0.30 | | | |
| 0.35 | | | |
| 0.40 | | | |
| 0.45 | | | |
| 0.50 | | | |
| 0.55 | | | |
| 0.60 | | | |

**Recommended threshold**: X.XX
**Rationale**: (why this threshold balances security and usability)

## Memory Usage
- Daemon RSS (idle): MB
- Daemon RSS (during auth): MB
- Peak during model load: MB

## Notes
- (observations, anomalies, hardware-specific behavior)
