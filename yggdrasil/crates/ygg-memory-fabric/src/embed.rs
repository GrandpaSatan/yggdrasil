//! TEI embedding client. Uses the HuggingFace TEI REST API on Munin
//! :11438 (all-MiniLM-L6-v2, 384-dim).

use anyhow::{anyhow, Result};
use serde::Serialize;

pub struct Embedder {
    client: reqwest::Client,
    base_url: String,
    dim: usize,
}

impl Embedder {
    pub fn new(base_url: String, dim: usize) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client");
        Self { client, base_url, dim }
    }

    /// Batch-embed a list of texts via POST /embed. TEI returns a 2-D
    /// float array shape (batch, dim).
    pub async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        #[derive(Serialize)]
        struct Req<'a> { inputs: &'a [String] }
        let url = format!("{}/embed", self.base_url.trim_end_matches('/'));
        let resp = self
            .client
            .post(url)
            .json(&Req { inputs: texts })
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("TEI embed failed: {}", resp.status()));
        }
        let body: Vec<Vec<f32>> = resp.json().await?;
        for v in &body {
            if v.len() != self.dim {
                return Err(anyhow!("TEI returned dim={}, expected {}", v.len(), self.dim));
            }
        }
        Ok(body)
    }

    pub async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let v = self.embed_batch(&[text.to_string()]).await?;
        v.into_iter().next().ok_or_else(|| anyhow!("TEI returned empty batch"))
    }

    pub fn dim(&self) -> usize { self.dim }
}

/// Cosine similarity, assuming both vectors already have unit norm
/// (TEI output is already L2-normalized). Returns NaN-safe 0.0 on
/// zero vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() { return 0.0; }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    if dot.is_nan() { 0.0 } else { dot }
}
