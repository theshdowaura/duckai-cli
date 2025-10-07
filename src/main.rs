mod challenge;
mod chat;
mod cli;
mod error;
mod js;
mod model;
mod server;
mod session;
mod util;
mod vqd;

use clap::Parser;
use cli::CliArgs;
use error::Result;

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let _ = fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .try_init();
}

async fn run(args: CliArgs) -> Result<()> {
    let session_config = args.session_config();
    let session = session::HttpSession::new(&session_config)?;
    let vqd = vqd::prepare_session(&session).await?;

    println!("UA: {}", args.user_agent);
    println!("client_hashes raw: {:?}", vqd.raw_client);
    println!("client_hashes sha256: {:?}", vqd.hashed_client);
    println!("x-fe-version: {}", vqd.fe_version);
    println!("x-vqd-hash-1 header: {}", vqd.vqd_header);

    if args.only_vqd {
        return Ok(());
    }

    let prompt = args.resolve_prompt()?;
    let chat = chat::send_chat(&session, &vqd, &prompt, &args.model, None).await?;
    println!("chat status: {}", chat.status);
    match chat.status {
        200 => println!("chat stream:\n{}", chat.body),
        418 => println!("challenge response:\n{}", chat.body),
        _ => println!("chat response:\n{}", chat.body),
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    init_tracing();
    let args = CliArgs::parse();

    let result = if args.serve {
        server::run_openai_server(&args).await
    } else {
        run(args).await
    };

    if let Err(error) = result {
        tracing::error!("{error:?}");
        std::process::exit(1);
    }
}
