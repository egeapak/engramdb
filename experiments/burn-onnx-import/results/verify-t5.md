| Model | Inputs | Agreement (Burn imported fp32 vs ONNX Runtime) |
|---|---|---|
| T5-small titles | 10 memories | encoder max abs diff 9.09e-7, decoder logits 5.72e-5; title same as ORT fp32 10/10, as shipped int8 5/10; Burn KV-cache decode same as full-prefix 0/10 (ORT KV-cache same as ORT full-prefix 10/10) |
|  | example | Burn: "orderflow API uses axum because tower middleware composes ." / ORT int8: "orderflow API uses axum because tower middleware composes ." |
|  | example | Burn: "access tokens live 15 minutes: fast revocation without a blacklist" / ORT int8: "access tokens live 15 minutes . shorter lifetimes mean revocation takes" |
|  | example | Burn: "a new refresh token is revoked and the user must sign in" / ORT int8: "a new refresh token is revoked and a new token is" |

| Workload | Load ms (Burn / ORT fp32 / ORT shipped) | Burn p50 ms | ORT fp32 p50 ms | ORT shipped p50 ms | Burn vs ORT fp32 | Burn vs ORT shipped |
|---|---:|---:|---:|---:|---:|---:|
| T5 title, 128 input tokens (full-prefix decode) | 517 / 742 / 235 | 1858.5 | 218.4 | 249.2 | 8.5× | 7.5× |
| T5 title, Burn with KV-cache decode | 517 / 742 / 235 | 1427.1 | 218.4 | 249.2 | 6.5× | 5.7× |
