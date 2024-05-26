use axum::{
	http::StatusCode,
	response::{IntoResponse, Response},
};
use databend_driver::Error as DBError;
use logql::parser::LogQLParseError;
use thiserror::Error;
use traceql::TraceQLError;

#[derive(Debug, Error)]
pub enum AppError {
	#[error("Invalid logql: {0}")]
	InvalidLogQL(#[from] LogQLParseError),
	#[error("Invalid traceql: {0}")]
	InvalidTraceQL(TraceQLError),
	#[error("Invalid time format: {0}")]
	InvalidTimeFormat(String),
	#[error("db error: {0}")]
	DBError(#[from] DBError),
	#[error("Serde error: {0}")]
	SerdeError(#[from] serde_json::Error),
	#[error("Unsupported data type: {0}")]
	UnsupportedDataType(String),
	#[error("Storage error: {0}")]
	StorageError(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
	fn into_response(self) -> Response {
		match self {
			AppError::StorageError(e) => (
				StatusCode::INTERNAL_SERVER_ERROR,
				format!("Storage error: {}", e),
			)
				.into_response(),
			AppError::InvalidTraceQL(e) => (
				StatusCode::BAD_REQUEST,
				format!("Invalid trace query: {}", e),
			)
				.into_response(),
			AppError::UnsupportedDataType(e) => (
				StatusCode::INTERNAL_SERVER_ERROR,
				format!("Unsupported data type: {}", e),
			)
				.into_response(),
			AppError::SerdeError(e) => (
				StatusCode::INTERNAL_SERVER_ERROR,
				format!("Serde error: {}", e),
			)
				.into_response(),
			AppError::InvalidLogQL(e) => {
				(StatusCode::BAD_REQUEST, format!("Invalid query: {}", e))
					.into_response()
			}
			AppError::InvalidTimeFormat(e) => (
				StatusCode::BAD_REQUEST,
				format!("Invalid time format: {}", e),
			)
				.into_response(),
			AppError::DBError(e) => (
				StatusCode::INTERNAL_SERVER_ERROR,
				format!("DB error: {}", e),
			)
				.into_response(),
		}
	}
}
