# Model License Notice

## Face Detection and Recognition Models

Facelock uses pre-trained ONNX models from the InsightFace project for face
detection and recognition.

### Models Used

| Model | Source | Purpose |
|-------|--------|---------|
| SCRFD 2.5G | [InsightFace](https://github.com/deepinsight/insightface) | Face detection + landmarks |
| SCRFD 10G | [InsightFace](https://github.com/deepinsight/insightface) | Higher accuracy face detection |
| ArcFace W600K R50 | [InsightFace](https://github.com/deepinsight/insightface) | Face embedding (512-dim) |
| GlintR100 | [InsightFace](https://github.com/deepinsight/insightface) | Higher accuracy face embedding |

### InsightFace License

The InsightFace models are released under a **non-commercial research license**.
See: https://github.com/deepinsight/insightface/blob/master/LICENSE

Key points:
- The models are free for **non-commercial research use**
- **Commercial use requires a separate license** from InsightFace/ArcSoft
- Contact: https://insightface.ai for commercial licensing inquiries

### Implications for Facelock Users

- **Personal use** (authenticating on your own Linux machine): Permitted under
  the non-commercial research license
- **Enterprise/commercial deployment**: You must obtain a commercial license
  from InsightFace/ArcSoft before deploying these models

### Facelock Code License

The Facelock source code itself is dual-licensed under MIT and Apache 2.0.
The model license is separate from and does not affect the code license.
See `LICENSE-MIT` and `LICENSE-APACHE` in the repository root.
