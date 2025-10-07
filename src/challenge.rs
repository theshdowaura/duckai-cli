use std::{fmt::Write, net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{anyhow, Context};
use axum::{
    extract::{Path, State},
    http::{header::CONTENT_TYPE, HeaderValue, StatusCode},
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use dialoguer::Input;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{
    fs,
    net::TcpListener,
    sync::{oneshot, Mutex},
    task::JoinHandle,
};
use url::form_urlencoded;

use crate::error::Result;
use crate::session::HttpSession;
use crate::util::parse_tile_selection;

const CHALLENGE_DIR: &str = "duckai_challenge";

#[derive(Clone)]
struct ChallengeAsset {
    index: usize,
    tile_id: String,
    file_path: PathBuf,
}

#[derive(Clone)]
struct ChallengeState {
    assets: Arc<Vec<ChallengeAsset>>,
    selection_tx: Arc<Mutex<Option<oneshot::Sender<Vec<usize>>>>>,
}

struct ChallengeWebServer {
    address: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl ChallengeWebServer {
    fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Err(err) = self.handle.await {
            if !err.is_cancelled() {
                tracing::error!("challenge web server join error: {err:?}");
            }
        }
    }

    async fn start(assets: Vec<ChallengeAsset>) -> Result<(Self, oneshot::Receiver<Vec<usize>>)> {
        let (selection_tx, selection_rx) = oneshot::channel::<Vec<usize>>();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let state = ChallengeState {
            assets: Arc::new(assets),
            selection_tx: Arc::new(Mutex::new(Some(selection_tx))),
        };

        let router = Router::new()
            .route("/", get(challenge_page))
            .route("/tiles/:index", get(tile_image))
            .route("/submit", post(submit_selection))
            .with_state(state);

        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("binding local challenge server")?;
        let address = listener
            .local_addr()
            .context("reading challenge server address")?;

        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });

        let handle = tokio::spawn(async move {
            if let Err(err) = server.await {
                tracing::error!("challenge server exited with error: {err:?}");
            }
        });

        Ok((
            Self {
                address,
                shutdown: Some(shutdown_tx),
                handle,
            },
            selection_rx,
        ))
    }
}

#[derive(Deserialize)]
struct SubmitPayload {
    selections: Vec<usize>,
}

/// Handles a server-issued challenge payload. Returns `true` when verification succeeds.
pub async fn handle_challenge(session: &HttpSession, payload: &Value) -> Result<bool> {
    let challenge = payload.get("cd").unwrap_or(payload);

    let override_code = challenge
        .get("overrideCode")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .or_else(|| {
            payload
                .get("overrideCode")
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned())
        });

    if let Some(code) = override_code.as_deref() {
        println!("Challenge overrideCode={code}");
    }

    let tiles = extract_tiles(challenge);
    if tiles.is_empty() {
        tracing::warn!("Challenge payload missing tile list: {payload}");
        return Ok(false);
    }

    let assets = save_challenge_assets(session, &tiles).await?;

    if assets.is_empty() {
        println!("未能下载挑战图片，挑战保持未完成。");
        return Ok(false);
    }

    const MAX_ATTEMPTS: usize = 3;
    let mut attempt = 0usize;
    let mut use_web = true;

    loop {
        attempt += 1;

        let selected_indices = if use_web {
            match ChallengeWebServer::start(assets.clone()).await {
                Ok((server, selection_rx)) => {
                    println!(
                        "挑战需要人工验证，请在浏览器打开 {} 并选择所有包含鸭子的图片后提交。",
                        server.url()
                    );
                    println!("提交后返回终端以继续流程。");

                    let result = selection_rx.await;
                    server.shutdown().await;

                    match result {
                        Ok(indices) => indices,
                        Err(_) => {
                            println!("网页会话已结束，但未收到选择结果。");
                            Vec::new()
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!("Failed to start challenge web interface: {err:?}");
                    println!("无法启动本地网页，将回退到命令行输入模式。");
                    use_web = false;
                    println!(
                        "请打开目录 `{CHALLENGE_DIR}` 查看 JPG 文件，并手动选择所有包含鸭子的正方形。"
                    );
                    prompt_tile_selection(&tiles)?
                }
            }
        } else {
            println!(
                "请打开目录 `{CHALLENGE_DIR}` 查看 JPG 文件，并手动选择所有包含鸭子的正方形。"
            );
            prompt_tile_selection(&tiles)?
        };

        if selected_indices.is_empty() {
            println!("未选择任何图片，挑战保持未完成。");
            if attempt >= MAX_ATTEMPTS {
                return Ok(false);
            }
            println!("将重新发起挑战，请重新选择。");
            continue;
        }

        let mut filtered = selected_indices
            .into_iter()
            .filter(|&idx| idx < tiles.len())
            .collect::<Vec<_>>();
        if filtered.is_empty() {
            println!("提交的索引无效，挑战保持未完成。");
            if attempt >= MAX_ATTEMPTS {
                return Ok(false);
            }
            println!("即将重新发起挑战，请检查输入。");
            continue;
        }
        filtered.sort_unstable();
        filtered.dedup();

        let selected_ids = filtered
            .into_iter()
            .map(|idx| tiles[idx].clone())
            .collect::<Vec<_>>();
        println!("已接收选择：{selected_ids:?}");

        match verify_challenge(session, challenge, &selected_ids).await? {
            true => return Ok(true),
            false => {
                if attempt >= MAX_ATTEMPTS {
                    println!("挑战验证失败次数过多，放弃本次挑战。");
                    return Ok(false);
                }
                println!("挑战验证失败，将重新发起挑战，请重新选择。");
            }
        }
    }
}

fn extract_tiles(value: &Value) -> Vec<String> {
    value
        .get("p")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .split('-')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_owned())
        .collect()
}

async fn save_challenge_assets(
    session: &HttpSession,
    tiles: &[String],
) -> Result<Vec<ChallengeAsset>> {
    if tiles.is_empty() {
        return Ok(Vec::new());
    }

    let dir = PathBuf::from(CHALLENGE_DIR);
    fs::create_dir_all(&dir)
        .await
        .context("creating duckai_challenge directory")?;

    println!(
        "Saving {} challenge tiles to `{}`",
        tiles.len(),
        dir.display()
    );

    let mut assets = Vec::with_capacity(tiles.len());

    for (index, tile) in tiles.iter().enumerate() {
        let url = session
            .base_url()
            .join(&format!("assets/anomaly/images/challenge/{tile}.jpg"))
            .context("building tile URL")?;
        let resp = session
            .client()
            .get(url)
            .send()
            .await
            .with_context(|| format!("downloading tile {tile}"))?;

        if !resp.status().is_success() {
            tracing::warn!("Tile {tile} download failed with HTTP {}", resp.status());
            continue;
        }

        let bytes = resp.bytes().await.context("reading tile bytes")?;
        let filename = dir.join(format!("{:02}_{}.jpg", index + 1, tile));
        fs::write(&filename, bytes)
            .await
            .with_context(|| format!("writing tile to {}", filename.display()))?;
        println!(
            "  [{}/{}] {} -> {}",
            index + 1,
            tiles.len(),
            tile,
            filename.display()
        );
        assets.push(ChallengeAsset {
            index,
            tile_id: tile.clone(),
            file_path: filename,
        });
    }

    if assets.is_empty() {
        tracing::warn!("No challenge tiles were saved successfully.");
    }

    Ok(assets)
}

async fn challenge_page(State(state): State<ChallengeState>) -> Html<String> {
    let mut html = String::new();
    html.push_str(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <title>Duck.ai 验证</title>
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <style>
    :root {
      color-scheme: light dark;
    }
    body {
      font-family: "Segoe UI", -apple-system, BlinkMacSystemFont, "PingFang SC", sans-serif;
      background: #f5f5f5;
      margin: 0;
      padding: 1.5rem;
      color: #1f1f1f;
    }
    main {
      max-width: 860px;
      margin: 0 auto;
      background: #ffffffcc;
      backdrop-filter: blur(8px);
      padding: 1.5rem 2rem 2rem;
      border-radius: 18px;
      box-shadow: 0 22px 45px rgba(15, 23, 42, 0.18);
    }
    h1 {
      margin-top: 0;
      font-size: 1.8rem;
    }
    p.lead {
      margin-bottom: 1.5rem;
      font-size: 1rem;
      color: #555;
    }
    .grid {
      display: grid;
      gap: 1rem;
      grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
      margin-bottom: 1.5rem;
    }
    label.tile {
      display: flex;
      flex-direction: column;
      align-items: center;
      gap: 0.65rem;
      padding: 0.9rem;
      border-radius: 16px;
      background: #ffffff;
      border: 2px solid transparent;
      box-shadow: inset 0 0 0 1px rgba(15, 23, 42, 0.08);
      transition: border-color 0.2s ease, box-shadow 0.2s ease;
    }
    label.tile.selected {
      border-color: #38bdf8;
      box-shadow: 0 0 0 3px rgba(56, 189, 248, 0.2);
    }
    label.tile img {
      width: 100%;
      border-radius: 12px;
      object-fit: cover;
      box-shadow: 0 16px 30px rgba(15, 23, 42, 0.18);
    }
    label.tile span {
      font-size: 0.85rem;
      color: #6b7280;
    }
    label.tile input {
      transform: scale(1.35);
    }
    button {
      font-size: 1rem;
      border: none;
      background: linear-gradient(135deg, #22d3ee, #2563eb);
      color: #fff;
      padding: 0.9rem 1.8rem;
      border-radius: 999px;
      cursor: pointer;
      transition: transform 0.15s ease, box-shadow 0.15s ease, filter 0.2s ease;
      box-shadow: 0 14px 26px rgba(37, 99, 235, 0.25);
    }
    button:hover {
      transform: translateY(-1px);
      box-shadow: 0 20px 35px rgba(37, 99, 235, 0.25);
    }
    button:disabled {
      cursor: not-allowed;
      filter: grayscale(0.35);
      box-shadow: none;
      transform: none;
    }
    .status {
      min-height: 1.4rem;
      margin-top: 1rem;
      font-weight: 600;
    }
    .status.error {
      color: #dc2626;
    }
    .status.success {
      color: #16a34a;
    }
    .note {
      margin-top: 2rem;
      color: #6b7280;
      font-size: 0.9rem;
    }
    @media (prefers-color-scheme: dark) {
      body {
        background: radial-gradient(circle at top, #1e293b, #0f172a);
        color: #e2e8f0;
      }
      main {
        background: rgba(15, 23, 42, 0.85);
        box-shadow: 0 26px 55px rgba(15, 23, 42, 0.65);
      }
      label.tile {
        background: rgba(14, 23, 33, 0.75);
        box-shadow: inset 0 0 0 1px rgba(148, 163, 184, 0.15);
      }
      label.tile span {
        color: #94a3b8;
      }
      p.lead, .note {
        color: #94a3b8;
      }
    }
  </style>
</head>
<body>
  <main>
    <h1>选择所有包含鸭子的图片</h1>
    <p class="lead">勾选所有包含鸭子的方块，然后点击提交按钮完成验证。</p>
    <form id="challenge-form" action="javascript:void 0">
      <div class="grid">
"#,
    );

    for asset in state.assets.iter() {
        let _ = write!(
            html,
            r#"<label class="tile">
  <input type="checkbox" value="{index}">
  <img src="/tiles/{index}" alt="challenge tile {index}" />
  <span>{id}</span>
</label>
"#,
            index = asset.index,
            id = asset.tile_id
        );
    }

    html.push_str(
        r#"      </div>
      <button type="submit" id="submit-btn">提交</button>
      <p id="status" class="status"></p>
    </form>
    <p class="note">如需重新选择，可刷新页面；若页面不可用，可回到终端手动输入。</p>
  </main>
  <script>
    const form = document.getElementById("challenge-form");
    const statusNode = document.getElementById("status");
    const submitBtn = document.getElementById("submit-btn");

    function setStatus(message, kind) {
      statusNode.textContent = message;
      statusNode.classList.remove("error", "success");
      if (kind) {
        statusNode.classList.add(kind);
      }
    }

    document.querySelectorAll("label.tile input").forEach((input) => {
      input.addEventListener("change", () => {
        const tile = input.closest("label.tile");
        if (!tile) return;
        tile.classList.toggle("selected", input.checked);
      });
    });

    form.addEventListener("submit", async (event) => {
      event.preventDefault();
      const selections = Array.from(document.querySelectorAll("label.tile input:checked"))
        .map((input) => Number.parseInt(input.value, 10))
        .filter((index) => Number.isInteger(index));

      setStatus("提交中…", null);
      submitBtn.disabled = true;

      try {
        const response = await fetch("/submit", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ selections }),
        });
        const data = await response.json().catch(() => ({}));
        if (response.ok) {
          setStatus(data.message || "提交成功，请返回终端。", "success");
        } else {
          submitBtn.disabled = false;
          setStatus(data.message || "提交失败，请检查选择后重试。", "error");
        }
      } catch (error) {
        submitBtn.disabled = false;
        setStatus("提交失败，请确保终端未退出后重试。", "error");
      }
    });
  </script>
</body>
</html>
"#,
    );

    Html(html)
}

async fn tile_image(
    Path(index): Path<usize>,
    State(state): State<ChallengeState>,
) -> impl IntoResponse {
    match state.assets.get(index) {
        Some(asset) => match fs::read(&asset.file_path).await {
            Ok(bytes) => (
                StatusCode::OK,
                [(CONTENT_TYPE, HeaderValue::from_static("image/jpeg"))],
                bytes,
            )
                .into_response(),
            Err(err) => {
                tracing::error!(
                    "Failed to read challenge tile {}: {err:?}",
                    asset.file_path.display()
                );
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "success": false,
                        "message": "读取图片失败"
                    })),
                )
                    .into_response()
            }
        },
        None => (StatusCode::NOT_FOUND, "图块不存在").into_response(),
    }
}

async fn submit_selection(
    State(state): State<ChallengeState>,
    Json(payload): Json<SubmitPayload>,
) -> impl IntoResponse {
    let total = state.assets.len();
    let mut selections: Vec<usize> = payload
        .selections
        .into_iter()
        .filter(|&idx| idx < total)
        .collect();
    selections.sort_unstable();
    selections.dedup();

    if selections.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "success": false,
                "message": "未选择任何有效图块"
            })),
        )
            .into_response();
    }

    let mut tx_guard = state.selection_tx.lock().await;
    let already_submitted = tx_guard.is_none();
    if let Some(tx) = tx_guard.take() {
        let _ = tx.send(selections.clone());
    }
    drop(tx_guard);

    if already_submitted {
        return Json(json!({
            "success": true,
            "message": "已接收选择，请返回终端。"
        }))
        .into_response();
    }

    Json(json!({
        "success": true,
        "message": "提交成功，请返回终端。"
    }))
    .into_response()
}

fn prompt_tile_selection(tiles: &[String]) -> Result<Vec<usize>> {
    println!("\n识别包含鸭子的图片：");
    for (idx, tile) in tiles.iter().enumerate() {
        println!("  [{}] {}", idx, tile);
    }

    let input: String = Input::new()
        .with_prompt("请输入包含鸭子的编号(逗号/空格分隔，留空跳过)")
        .allow_empty(true)
        .interact_text()?;

    Ok(parse_tile_selection(&input, tiles.len()))
}

async fn verify_challenge(
    session: &HttpSession,
    challenge: &Value,
    selected_ids: &[String],
) -> Result<bool> {
    if selected_ids.is_empty() {
        return Ok(false);
    }

    let q = string_field(challenge, "q").unwrap_or_default();
    let cc = string_field(challenge, "cc").unwrap_or_else(|| "duckchat".to_owned());
    let s_field = string_field(challenge, "s").unwrap_or_else(|| "aichat".to_owned());
    let r_field = string_field(challenge, "r").unwrap_or_else(|| "usw".to_owned());
    let gk = string_field(challenge, "gk");
    let p_field = string_field(challenge, "p");
    let o_field = string_field(challenge, "o");

    let params = {
        let mut serializer = form_urlencoded::Serializer::new(String::new());
        serializer.append_pair("q", &q);
        serializer.append_pair("type", "anomaly");
        serializer.append_pair("acs", &selected_ids.join("-"));
        serializer.append_pair("cc", &cc);
        if let Some(gk) = gk.as_ref() {
            serializer.append_pair("gk", gk);
        }
        if let Some(p) = p_field.as_ref() {
            serializer.append_pair("p", p);
        }
        if let Some(o) = o_field.as_ref() {
            serializer.append_pair("o", o);
        }
        serializer.append_pair("s", &s_field);
        serializer.append_pair("r", &r_field);
        if let Some(sc) = challenge.get("sc").and_then(|v| v.as_i64()) {
            serializer.append_pair("sc", &sc.to_string());
        }
        if let Some(i) = challenge.get("i").and_then(|v| v.as_i64()) {
            serializer.append_pair("i", &i.to_string());
        }
        serializer.finish()
    };
    let url = session
        .base_url()
        .join(&format!("anomaly.js?{params}"))
        .context("building challenge verification URL")?;

    let resp = session
        .client()
        .get(url)
        .header("X-Requested-With", "XMLHttpRequest")
        .send()
        .await
        .context("verifying challenge")?;
    let text = resp.text().await.context("reading verification response")?;

    match serde_json::from_str::<Value>(&text) {
        Ok(json) => {
            println!("验证响应: {json}");
            if json.get("sc").and_then(|v| v.as_i64()) == Some(0) {
                println!("挑战验证成功。");
                return Ok(true);
            }
            println!("挑战验证失败。");
            Ok(false)
        }
        Err(err) => {
            tracing::error!("解析验证响应失败: {err:?}");
            tracing::debug!("Raw response: {text}");
            Err(anyhow!("Failed to parse challenge verification response"))
        }
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
}
