//! The burn-onnx-generated sources (written by `../import.sh` into `../gen/`,
//! which is not committed). Each lives in its own module because every file
//! defines its own `Model` and `SubmoduleN` types.
#![allow(clippy::all, unused, non_snake_case)]

pub mod minilm {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../gen/minilm-fp32/minilm-model.rs"
    ));
}
pub mod reranker {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../gen/reranker-fp32/model.rs"
    ));
}
pub mod nli {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../gen/nli-fp32/model.rs"
    ));
}
pub mod t5_encoder {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../gen/t5-encoder-fp32/encoder_model.rs"
    ));
}
pub mod t5_decoder {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../gen/t5-decoder-fp32/decoder_model.rs"
    ));
}
pub mod t5_decoder_past {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../gen/t5-decoder-past-fp32/decoder_with_past_model.rs"
    ));
}
