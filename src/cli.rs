use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgAction, Parser};

use crate::model;
use crate::session::SessionConfig;
use anyhow::{anyhow, Context as AnyhowContext, Result};

const DEFAULT_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// Command-line options for the Duck.ai client.
#[derive(Debug, Clone, Parser)]
#[command(author, version, about = "Duck.ai VQD and chat helper", long_about = None)]
pub struct CliArgs {
    /// User-Agent value to send with HTTP requests.
    #[arg(long = "ua", default_value = DEFAULT_UA)]
    pub user_agent: String,

    /// Prompt text to send to the chat endpoint.
    #[arg(long = "text", conflicts_with_all = ["prompt_file", "stdin_prompt"])]
    pub prompt: Option<String>,

    /// Read the chat prompt from the specified file.
    #[arg(long = "prompt-file", value_name = "PATH", conflicts_with_all = ["prompt", "stdin_prompt"])]
    pub prompt_file: Option<PathBuf>,

    /// Read the chat prompt from STDIN (until EOF).
    #[arg(long = "stdin-prompt", action = ArgAction::SetTrue, conflicts_with_all = ["prompt", "prompt_file"])]
    pub stdin_prompt: bool,

    /// Only fetch and display the VQD header without sending a chat prompt.
    #[arg(long = "only-vqd", action = ArgAction::SetTrue)]
    pub only_vqd: bool,

    /// Run an OpenAI-compatible HTTP server instead of executing a single chat request.
    #[arg(long = "serve", action = ArgAction::SetTrue)]
    pub serve: bool,

    /// Listen address for the OpenAI-compatible HTTP server (requires `--serve`).
    #[arg(long = "listen", value_name = "ADDR", requires = "serve")]
    pub listen: Option<String>,

    /// API key required in the `Authorization` header (Bearer) for incoming requests.
    #[arg(long = "server-api-key", env = "DUCKAI_API_KEY", requires = "serve")]
    pub server_api_key: Option<String>,

    /// Model identifier to request from Duck.ai.
    #[arg(
        long = "model",
        default_value = model::DEFAULT_MODEL_ID,
        value_parser = model::model_value_parser()
    )]
    pub model: String,

    /// Network timeout (seconds) applied to HTTP requests.
    #[arg(long = "timeout", default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
    timeout_secs: u64,
}

impl CliArgs {
    /// Returns the configured network timeout.
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }

    /// Resolve the prompt text based on CLI inputs.
    pub fn resolve_prompt(&self) -> Result<String> {
        if let Some(prompt) = &self.prompt {
            return Ok(prompt.clone());
        }
        if let Some(path) = &self.prompt_file {
            return fs::read_to_string(path)
                .with_context(|| format!("reading prompt file {}", path.display()));
        }
        if self.stdin_prompt {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .context("reading prompt from stdin")?;
            if buf.is_empty() {
                return Err(anyhow!("stdin prompt was empty"));
            }
            return Ok(buf);
        }
        Ok("hello".to_owned())
    }

    /// Convert CLI arguments into a session configuration.
    pub fn session_config(&self) -> SessionConfig {
        SessionConfig::new(self.user_agent.clone(), self.timeout())
    }
}
