//! The `llama.cpp` extension — mirrors pi-coding-agent's `extensions/llama`
//! module (`packages/coding-agent/src/extensions/llama`).

pub mod client;
pub mod huggingface;
pub mod provider;
pub mod ui;

pub use client::{
    format_bytes, llama_inference_url, normalize_llama_server_url, parse_sse_frame,
    LlamaArchitecture, LlamaClient, LlamaEventStream, LlamaListOptions, LlamaMeta, LlamaModelEvent,
    LlamaModelInfo, LlamaModelStatus, LlamaModelStatusInfo, LlamaModelsResponse, LlamaProgress,
    LlamaProgressEntry,
};
pub use huggingface::{
    find_hugging_face_token, HuggingFaceClient, HuggingFaceGated, HuggingFaceModel,
    HuggingFaceModelDetails, HuggingFaceQuantization, DEFAULT_HUGGING_FACE_URL,
};
pub use provider::{
    create_llama_provider, LlamaProvider, LlamaProviderController, DEFAULT_LLAMA_SERVER_URL,
    LLAMA_PROVIDER_ID,
};
pub use ui::{
    run_with_progress, show_llama_ui, ConnectionErrorChoice, LlamaManagerAction, LlamaUi,
    LlamaView, ProgressState, ProgressUpdate, RunOutcome, SearchFn,
};
