//! The `llama.cpp` extension — mirrors pi-coding-agent's `extensions/llama`
//! module (`packages/coding-agent/src/extensions/llama`).

pub mod huggingface;

pub use huggingface::{
    find_hugging_face_token, HuggingFaceClient, HuggingFaceGated, HuggingFaceModel,
    HuggingFaceModelDetails, HuggingFaceQuantization, DEFAULT_HUGGING_FACE_URL,
};
