//! Data transfer object definitions will live here.

use clap::builder::PossibleValuesParser;
use serde::{Deserialize, Serialize};

/// Available model definitions exposed by Duck.ai.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: &'static str,
    pub object: &'static str,
    pub created: u64,
    pub owned_by: &'static str,
}

pub const MODELS: &[ModelInfo] = &[
    ModelInfo {
        id: "gpt-4o-mini",
        object: "model",
        created: 0,
        owned_by: "duck.ai",
    },
    ModelInfo {
        id: "claude-3-5-haiku-latest",
        object: "model",
        created: 0,
        owned_by: "duck.ai",
    },
    ModelInfo {
        id: "mistralai/Mistral-Small-24B-Instruct-2501",
        object: "model",
        created: 0,
        owned_by: "duck.ai",
    },
    ModelInfo {
        id: "gpt-5-mini",
        object: "model",
        created: 0,
        owned_by: "duck.ai",
    },
    ModelInfo {
        id: "openai/gpt-oss-120b",
        object: "model",
        created: 0,
        owned_by: "duck.ai",
    },
];

pub const DEFAULT_MODEL_ID: &str = "gpt-5-mini";
/// Build a Clap value parser that restricts input to the known model identifiers.
pub fn model_value_parser() -> PossibleValuesParser {
    let values: Vec<&'static str> = MODELS.iter().map(|model| model.id).collect();
    PossibleValuesParser::new(values)
}

/// Raw status payload from `/duckchat/v1/status`.
pub type StatusResponse = serde_json::Value;

/// Minimal structure returned by the obfuscated evaluation helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluatedHashes {
    pub client_hashes: Vec<String>,
    pub server_hashes: Vec<String>,
    #[serde(default)]
    pub signals: serde_json::Value,
    #[serde(default)]
    pub meta: serde_json::Value,
}
