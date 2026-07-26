# Local GigaAM v3 build assets

Before running `scripts/build-app.sh`, place these exact files in
`vendor/models/gigaam-v3-rnnt/`:

```text
encoder.int8.onnx
decoder.onnx
joiner.onnx
tokens.txt
```

They come from the upstream converted bundle:

```text
sherpa-onnx-nemo-transducer-punct-giga-am-v3-russian-2025-12-16
```

The model files are local, ignored build inputs and are not stored in this
repository. PTT2me intentionally does not support runtime downloading or a
network fallback.
