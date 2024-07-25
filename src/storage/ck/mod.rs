use super::{log::LogStorage, trace::TraceStorage};
use crate::config::{Clickhouse, ClickhouseTrace};
use anyhow::Result;
use reqwest::Client;
use std::time::Duration;

pub(crate) mod common;
pub(crate) mod converter;
pub mod log;
pub mod trace;

pub async fn new_log_source(cfg: Clickhouse) -> Result<Box<dyn LogStorage>> {
	let cli = Client::builder()
		.gzip(true)
		.timeout(Duration::from_secs(60))
		.build()?;
	Ok(Box::new(log::CKLogQuerier::new(
		cli,
		cfg.table.clone(),
		cfg,
	)))
}

pub async fn new_trace_source(
	cfg: ClickhouseTrace,
) -> Result<Box<dyn TraceStorage>> {
	let cli = Client::builder()
		.gzip(true)
		.timeout(Duration::from_secs(60))
		.build()?;
	Ok(Box::new(trace::CKTraceQuerier::new(
		cli,
		cfg.common.table.clone(),
		cfg,
	)))
}
