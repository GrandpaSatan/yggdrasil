//! Seed the Odin SDR router's intent prototypes from a curated phrase list.
//!
//! The hybrid router's "System 1" layer (`SdrRouter`) compares each incoming
//! query's SDR against per-intent prototypes via Hamming similarity. On a fresh
//! deploy the prototype store is empty, so every request falls through to the
//! slow "System 2" LLM classifier. This offline seeder bootstraps the store
//! with one OR-accumulated prototype per intent using the same pipeline that
//! runs at request time: Mimir's `/api/v1/embed` endpoint → `sdr::binarize`.
//!
//! Usage:
//!   cargo run --example seed_prototypes --release -- \
//!       --phrases training/router/seed-phrases.json \
//!       --mimir-url http://10.0.65.8:9090 \
//!       --out /tmp/odin-sdr-prototypes.json
//!
//! Then scp to Munin `/var/lib/yggdrasil/odin-sdr-prototypes.json`,
//! `chown yggdrasil:yggdrasil`, and restart `yggdrasil-odin.service`.
//! After the restart Odin logs should show `intent=X confidence=Y method=SDR`
//! on requests that previously logged `method=LLM` or `method=Fallback`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::Utc;
use odin::sdr_router::IntentPrototype;
use serde::Deserialize;
use ygg_domain::sdr;

#[derive(Debug, Deserialize)]
struct EmbedResponse {
    embedding: Vec<f32>,
}

struct Args {
    phrases: PathBuf,
    mimir_url: String,
    out: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut phrases: Option<PathBuf> = None;
    let mut mimir_url = "http://10.0.65.8:9090".to_string();
    let mut out = PathBuf::from("odin-sdr-prototypes.json");

    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--phrases" => {
                i += 1;
                phrases = Some(PathBuf::from(&argv[i]));
            }
            "--mimir-url" => {
                i += 1;
                mimir_url = argv[i].clone();
            }
            "--out" => {
                i += 1;
                out = PathBuf::from(&argv[i]);
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: seed_prototypes --phrases <path> [--mimir-url <url>] [--out <path>]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg: {other}")),
        }
        i += 1;
    }

    let phrases = phrases.ok_or("--phrases is required")?;
    Ok(Args { phrases, mimir_url, out })
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args().map_err(|e| {
        eprintln!("arg error: {e}");
        e
    })?;

    // seed-phrases.json is a simple { intent: [phrase, ...] } map. BTreeMap
    // for stable iteration / deterministic output.
    let raw = std::fs::read_to_string(&args.phrases)?;
    let phrases_by_intent: BTreeMap<String, Vec<String>> = serde_json::from_str(&raw)?;
    eprintln!(
        "loaded {} intents from {}",
        phrases_by_intent.len(),
        args.phrases.display()
    );

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let embed_url = format!("{}/api/v1/embed", args.mimir_url.trim_end_matches('/'));

    let mut prototypes: Vec<IntentPrototype> = Vec::with_capacity(phrases_by_intent.len());

    for (intent, phrases) in &phrases_by_intent {
        if phrases.is_empty() {
            eprintln!("  [{intent}] SKIP — no phrases");
            continue;
        }
        eprintln!("  [{intent}] encoding {} phrases via Mimir", phrases.len());

        let mut accumulator = sdr::ZERO;
        let mut accumulated = 0u64;

        for phrase in phrases {
            let resp = http
                .post(&embed_url)
                .json(&serde_json::json!({ "text": phrase }))
                .send()
                .await?;
            if !resp.status().is_success() {
                return Err(format!(
                    "Mimir embed returned HTTP {} for phrase {:?}",
                    resp.status(),
                    phrase
                )
                .into());
            }
            let body: EmbedResponse = resp.json().await?;
            if body.embedding.len() < sdr::SDR_BITS {
                return Err(format!(
                    "Mimir returned {}-dim embedding, need at least {}",
                    body.embedding.len(),
                    sdr::SDR_BITS
                )
                .into());
            }

            // Binarize uses the SAME sign-threshold as the live request path
            // in sdr_router — any drift here would make seeded prototypes
            // incompatible with runtime SDRs.
            let phrase_sdr = sdr::binarize(&body.embedding);
            accumulator = sdr::or(&accumulator, &phrase_sdr);
            accumulated += 1;
        }

        let pop = sdr::popcount(&accumulator);
        eprintln!(
            "    → OR-accumulated {} phrases, popcount={}/{}",
            accumulated,
            pop,
            sdr::SDR_BITS
        );

        prototypes.push(IntentPrototype {
            intent: intent.clone(),
            sdr: accumulator,
            sample_count: accumulated,
            last_updated: Utc::now(),
        });
    }

    let json = serde_json::to_string_pretty(&prototypes)?;
    std::fs::write(&args.out, json)?;
    eprintln!(
        "wrote {} prototypes to {}",
        prototypes.len(),
        args.out.display()
    );

    Ok(())
}
