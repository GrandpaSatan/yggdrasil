//! Prometheus metrics for fabric.

use prometheus::{
    register_histogram_vec, register_int_counter_vec, register_int_gauge_vec,
    HistogramVec, IntCounterVec, IntGaugeVec, Registry,
};
use std::sync::OnceLock;

pub struct Metrics {
    pub publish_total: IntCounterVec,        // labels: model
    pub query_total: IntCounterVec,          // labels: flow_id_bucket
    pub l3_hits_total: IntCounterVec,        // labels: pair (producer_model→consumer_model)
    pub evictions_total: IntCounterVec,      // labels: reason
    pub bytes_stored: IntGaugeVec,           // labels: tier
    pub publish_latency: HistogramVec,       // labels: model
    pub query_latency: HistogramVec,         // labels: cache_hit (true|false)
    pub registry: Registry,
}

static METRICS: OnceLock<Metrics> = OnceLock::new();

pub fn init() -> &'static Metrics {
    METRICS.get_or_init(|| {
        let registry = Registry::new();

        let publish_total = register_int_counter_vec!(
            "ygg_fabric_publish_total",
            "Total fabric.publish calls",
            &["model"]
        ).expect("register publish_total");
        let query_total = register_int_counter_vec!(
            "ygg_fabric_query_total",
            "Total fabric.query calls",
            &["flow_id_bucket"]
        ).expect("register query_total");
        let l3_hits_total = register_int_counter_vec!(
            "ygg_fabric_l3_hits_total",
            "L3 semantic-tier hits (non-empty query results)",
            &["pair"]
        ).expect("register l3_hits_total");
        let evictions_total = register_int_counter_vec!(
            "ygg_fabric_evictions_total",
            "Flows explicitly evicted via /fabric/done or TTL",
            &["reason"]
        ).expect("register evictions_total");
        let bytes_stored = register_int_gauge_vec!(
            "ygg_fabric_bytes_stored",
            "Approximate bytes stored per tier",
            &["tier"]
        ).expect("register bytes_stored");
        let publish_latency = register_histogram_vec!(
            "ygg_fabric_publish_latency_seconds",
            "Fabric publish path wall-clock",
            &["model"],
            vec![0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5]
        ).expect("register publish_latency");
        let query_latency = register_histogram_vec!(
            "ygg_fabric_query_latency_seconds",
            "Fabric query path wall-clock",
            &["cache_hit"],
            vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5]
        ).expect("register query_latency");

        for m in [
            Box::new(publish_total.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(query_total.clone()),
            Box::new(l3_hits_total.clone()),
            Box::new(evictions_total.clone()),
            Box::new(bytes_stored.clone()),
            Box::new(publish_latency.clone()),
            Box::new(query_latency.clone()),
        ] {
            registry.register(m).ok();
        }

        Metrics {
            publish_total,
            query_total,
            l3_hits_total,
            evictions_total,
            bytes_stored,
            publish_latency,
            query_latency,
            registry,
        }
    })
}

pub fn get() -> &'static Metrics { init() }
