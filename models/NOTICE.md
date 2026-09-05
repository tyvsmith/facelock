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
See the upstream [model license policy](https://github.com/deepinsight/insightface#license)
(checked 2026-09-05). The code's MIT license is not the model-weight license.

Key points:
- The models are free for **non-commercial research use**
- **Commercial use requires separate permission** from the model rights holder
- Upstream directs open-source recognition-model licensing inquiries to
  `recognition-oss-pack@insightface.ai`

### Implications for Facelock Users

- **Personal use**: Non-commercial use is not automatically research use.
  Facelock does not grant permission for ordinary desktop authentication;
  confirm that your intended use is covered by the upstream model terms
- **Enterprise/commercial deployment**: Obtain appropriate model licensing
  before deploying these weights

### Facelock Code License

The Facelock source code itself is dual-licensed under MIT and Apache 2.0.
The model license is separate from and does not affect the code license.
See `LICENSE-MIT` and `LICENSE-APACHE` in the repository root.
