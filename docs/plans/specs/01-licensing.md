# WS1: Licensing — Spec

**Status:** Complete

## Changes Made

- Created `LICENSE-MIT` — MIT license text
- Created `LICENSE-APACHE` — Apache 2.0 license text
- Created `NOTICE` — Apache 2.0 attribution notice
- Created `models/NOTICE.md` — InsightFace model license documentation
- Updated `Cargo.toml` — `license = "MIT OR Apache-2.0"`
- Updated `README.md` — License badge + license section with model notice

## Verification

```bash
cargo metadata --format-version 1 | jq '.packages[].license' | sort -u
# Output: "MIT OR Apache-2.0"
```
