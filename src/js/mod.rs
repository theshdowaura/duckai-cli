use std::thread;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use boa_engine::js_string;
use boa_engine::property::Attribute;
use boa_engine::{Context as BoaContext, JsError, JsValue, Source};
use serde::Deserialize;

use crate::model::EvaluatedHashes;

const RUNTIME_JS: &str = include_str!("../../js/runtime.js");
const MAX_POLL_ITERATIONS: usize = 500;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Deserialize)]
struct RawHashes {
    server_hashes: Vec<String>,
    client_hashes: Vec<String>,
    #[serde(default)]
    signals: serde_json::Value,
    #[serde(default)]
    meta: serde_json::Value,
}

pub fn evaluate(script_b64: &str, user_agent: &str) -> anyhow::Result<EvaluatedHashes> {
    let mut context = BoaContext::default();
    eval_source(
        &mut context,
        RUNTIME_JS.as_bytes(),
        "loading JS runtime environment",
    )?;

    let _ = context.register_global_property(
        js_string!("DUCKAI_SCRIPT_B64"),
        JsValue::from(script_b64),
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    );
    let _ = context.register_global_property(
        js_string!("DUCKAI_USER_AGENT"),
        JsValue::from(user_agent),
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    );

    eval_source(
        &mut context,
        br#"
        globalThis.__duckai_result = undefined;
        globalThis.__duckai_error = undefined;
        duckaiEvaluate(DUCKAI_SCRIPT_B64, DUCKAI_USER_AGENT)
          .then((value) => { __duckai_result = value; })
          .catch((err) => {
            if (err && typeof err === 'object' && 'message' in err) {
              __duckai_error = String(err.message);
            } else {
              __duckai_error = String(err);
            }
          });
    "#,
        "evaluating duckai runtime",
    )?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut iterations = 0;
    loop {
        context.run_jobs();

        let result = get_global(&mut context, "__duckai_result")?;
        let error = get_global(&mut context, "__duckai_error")?;

        if !error.is_undefined() && !error.is_null() {
            let err_string = js_value_to_string(&mut context, error, "stringifying JS error")?;
            return Err(anyhow!("JS evaluation failed: {}", err_string));
        }

        if !result.is_undefined() && !result.is_null() {
            let json_value = eval_source(
                &mut context,
                br#"JSON.stringify(__duckai_result)"#,
                "serializing JS result",
            )?;
            let json = js_value_to_string(&mut context, json_value, "converting JS string")?;

            let raw: RawHashes = serde_json::from_str(&json)
                .map_err(|err| anyhow!("deserializing JS evaluation result: {}", err))?;

            return Ok(EvaluatedHashes {
                server_hashes: raw.server_hashes,
                client_hashes: raw.client_hashes,
                signals: raw.signals,
                meta: raw.meta,
            });
        }

        if Instant::now() > deadline || iterations >= MAX_POLL_ITERATIONS {
            return Err(anyhow!("JS evaluation timed out before settling result"));
        }
        iterations += 1;
        thread::sleep(POLL_INTERVAL);
    }
}

fn eval_source(context: &mut BoaContext, source: &[u8], label: &str) -> anyhow::Result<JsValue> {
    context
        .eval(Source::from_bytes(source))
        .map_err(|err| js_error_to_anyhow(err, label))
}

fn get_global(context: &mut BoaContext, name: &str) -> anyhow::Result<JsValue> {
    context
        .global_object()
        .get(js_string!(name), context)
        .map_err(|err| js_error_to_anyhow(err, &format!("reading global {name}")))
}

fn js_value_to_string(
    context: &mut BoaContext,
    value: JsValue,
    label: &str,
) -> anyhow::Result<String> {
    value
        .to_string(context)
        .map_err(|err| js_error_to_anyhow(err, label))?
        .to_std_string()
        .map_err(|_| anyhow!("{label}: produced non-utf8 string", label = label))
}

fn js_error_to_anyhow(err: JsError, label: &str) -> anyhow::Error {
    let message = err.to_string();
    anyhow!("{label}: {message}", label = label, message = message)
}
