use std::{cmp::Ordering, sync::Arc};

use super::*;
use crate::{errors::AppError, state::AppState};
use axum::{
	extract::{rejection::QueryRejection, Path, Query, State},
	Json,
};
use common::TimeRange;
use logql::parser;
use moka::sync::Cache;
use tracing::debug;

pub async fn query_labels(
	State(state): State<AppState>,
	Query(req): Query<QueryLabelsRequest>,
) -> Result<QueryLabelsResponse, AppError> {
	let cache = state.cache;
	if let Some(c) = cache.get(label_cache_key()) {
		return Ok(serde_json::from_slice(&c).unwrap());
	}
	let labels = state
		.log_handle
		.labels(QueryLimits {
			limit: None,
			range: time_range_less_in_a_day(req.start, req.end),
			direction: None,
			step: None,
		})
		.await?;
	let should_cache = !labels.is_empty();
	let resp = QueryLabelsResponse {
		status: ResponseStatus::Success,
		data: labels,
	};
	if should_cache {
		let d = serde_json::to_vec(&resp).unwrap();
		cache.insert(label_cache_key().to_string(), Arc::new(d));
	}
	Ok(resp)
}

fn time_range_less_in_a_day(
	start: Option<LokiDate>,
	_: Option<LokiDate>,
) -> TimeRange {
	let two_hour_before = Utc::now() - Duration::from_secs(2 * 60 * 60);
	let start = start.or(Some(LokiDate(two_hour_before))).map(|d| {
		let d = d.0;
		if d > two_hour_before {
			d
		} else {
			two_hour_before
		}
	});
	TimeRange {
		start: start.map(|v| v.naive_utc()),
		end: None,
	}
}

const fn label_cache_key() -> &'static str {
	"cc:labels"
}

fn label_values_cache_key(k: &str) -> String {
	format!("cc:label_values:{}", k)
}

fn series_cache_key() -> String {
	"cc:series:".to_string()
}

fn series_cache_key_with_matches(matches: &str) -> String {
	format!("{}-{}", series_cache_key(), matches)
}

pub async fn query_label_values(
	State(state): State<AppState>,
	Path(label): Path<String>,
	Query(req): Query<QueryLabelValuesRequest>,
) -> Result<QueryLabelsResponse, AppError> {
	let cache = state.cache;
	let cache_key = label_values_cache_key(&label);
	if let Some(c) = cache.get(&cache_key) {
		debug!("hit cache for label values: {}", cache_key);
		return Ok(serde_json::from_slice(&c).unwrap());
	}
	debug!("miss cache for label values: {}", cache_key);
	let values = state
		.log_handle
		.label_values(
			&label,
			QueryLimits {
				limit: None,
				range: time_range_less_in_a_day(req.start, req.end),
				direction: None,
				step: None,
			},
		)
		.await?;
	let should_cache = !values.is_empty();
	let resp = QueryLabelsResponse {
		status: ResponseStatus::Success,
		data: values,
	};
	if should_cache {
		let d = serde_json::to_vec(&resp).unwrap();
		cache.insert(cache_key, Arc::new(d));
	}
	Ok(resp)
}

pub async fn query_series(
	State(state): State<AppState>,
	req: Result<Query<QuerySeriesRequest>, QueryRejection>,
) -> Result<Json<QuerySeriesResponse>, AppError> {
	let req = req
		.map_err(|e| AppError::InvalidQueryString(e.to_string()))?
		.0;
	let matches = if let parser::Query::LogQuery(lq) =
		parser::parse_logql_query(req.matches.as_str())?
	{
		lq
	} else {
		return Err(AppError::InvalidQueryString(req.matches));
	};
	// if no label pairs, client should not call this api
	// instead, it should call query_labels
	if matches.selector.label_paris.is_empty() {
		return Err(AppError::InvalidQueryString(
			req.matches.as_str().to_string(),
		));
	}
	let canonicalized_matches =
		canonicalize_matches(&matches.selector.label_paris);
	let cache_key_with_matches =
		series_cache_key_with_matches(&canonicalized_matches);
	if let Some(v) = state.cache.get(&cache_key_with_matches) {
		debug!("hit cache for series: {}", cache_key_with_matches);
		return Ok(Json(QuerySeriesResponse {
			status: ResponseStatus::Success,
			data: serde_json::from_slice(&v).unwrap(),
		}));
	}
	debug!("miss cache for series: {}", cache_key_with_matches);
	// try best to find cache whose key is the longest prefix of cache_key_with_matches
	// by doing this, can we minimize the number of label pairs that we need to filter
	// todo: this is inefficient, we should use a better way to find the longest prefix like trie
	let mut longest_prefix = None;
	for (k, _) in state.cache.iter() {
		if cache_key_with_matches.starts_with(k.as_ref()) {
			match longest_prefix {
				None => {
					longest_prefix = Some(k);
				}
				Some(p) if k.len() > p.len() => {
					longest_prefix = Some(k);
				}
				_ => {}
			}
		}
	}

	let cache_key = if let Some(v) = longest_prefix {
		debug!("use longest prefix cache: {}", v);
		(*v).clone()
	} else {
		series_cache_key()
	};
	let mut values = if let Some(v) = state.cache.get(&cache_key) {
		serde_json::from_slice(&v).unwrap()
	} else {
		debug!(
			"no cache hit, very slow path, O(n!), cache_key: {}",
			cache_key
		);
		// no cache hit, very slow path, O(n!)
		let v = state
			.log_handle
			.series(
				None,
				QueryLimits {
					limit: None,
					range: time_range_less_in_a_day(req.start, req.end),
					direction: None,
					step: None,
				},
			)
			.await?;
		// cache result to avoid O(n!)
		if !v.is_empty() {
			let d = serde_json::to_vec(&v).unwrap();
			state.cache.insert(series_cache_key(), Arc::new(d));
			let v2 = convert_vec_hashmap(&v);
			cache_values(&state.cache, &v2);
		}
		v
	};
	// get the rest label pairs that we need to filter by
	let rest_label_pairs =
		get_rest_label_pairs(&cache_key, &cache_key_with_matches);
	// filter by matches
	if !rest_label_pairs.is_empty() {
		let before = values.len();
		values.retain(|m| filter_by_matches(m, &rest_label_pairs));
		debug!(
			"filter by matches, before: {}, after: {}",
			before,
			values.len()
		);
	}

	if !values.is_empty() && !rest_label_pairs.is_empty() {
		let d = serde_json::to_vec(&values).unwrap();
		state.cache.insert(cache_key_with_matches, Arc::new(d));
	}
	Ok(Json(QuerySeriesResponse {
		status: ResponseStatus::Success,
		data: values,
	}))
}

// using_key is the cache key that we are using, cache_key_with_matches is the full key
// eg:
//   using_key: cc:series
//   cache_key_with_matches: cc:series:k1/0/v1-k2/1/v2
//   return: [k1/0/v1, k2/1/v2]
//
//   using_key: cc:series:k1/0/v1
//   cache_key_with_matches: cc:series:k1/0/v1-k2/1/v2
//   return: [k2/1/v2]
fn get_rest_label_pairs(
	using_key: &str,
	cache_key_with_matches: &str,
) -> Vec<parser::LabelPair> {
	let start = using_key.len() + 1;
	if start >= cache_key_with_matches.len() {
		return vec![];
	}
	let suffix = &cache_key_with_matches[start..];
	let pairs = suffix
		.split('-')
		.map(|s| {
			let parts = s.split('/').collect::<Vec<_>>();
			parser::LabelPair {
				label: parts[0].to_string(),
				op: str_to_operator(parts[1].chars().next().unwrap()),
				value: parts[2].to_string(),
			}
		})
		.collect();
	pairs
}

fn str_to_operator(c: char) -> parser::Operator {
	match c {
		'0' => parser::Operator::Equal,
		'1' => parser::Operator::NotEqual,
		'2' => parser::Operator::RegexMatch,
		'3' => parser::Operator::RegexNotMatch,
		_ => panic!("invalid operator: {}", c),
	}
}

fn operator_to_str(op: &parser::Operator) -> char {
	match op {
		parser::Operator::Equal => '0',
		parser::Operator::NotEqual => '1',
		parser::Operator::RegexMatch => '2',
		parser::Operator::RegexNotMatch => '3',
	}
}

// canonicalize the matches to a string
// make {k1="v1", k2!="v2"} to k1/0/v1-k2/1/v2
fn canonicalize_matches(matches: &[parser::LabelPair]) -> String {
	let mut arr = matches.to_vec();
	// sort by label but servicename is always first
	arr.sort_by(|a, b| {
		if a.label.eq_ignore_ascii_case("servicename") {
			Ordering::Less
		} else if b.label.eq_ignore_ascii_case("servicename") {
			Ordering::Greater
		} else {
			a.label.cmp(&b.label)
		}
	});
	let mut s = arr.into_iter().fold(String::new(), |mut acc, v| {
		acc.push_str(&v.label);
		acc.push('/');
		acc.push(operator_to_str(&v.op));
		acc.push('/');
		acc.push_str(&v.value);
		acc.push('-');
		acc
	});
	// remove the last '-'
	s.truncate(s.len() - 1);
	s
}

fn filter_by_matches(
	values: &HashMap<String, String>,
	matches: &Vec<parser::LabelPair>,
) -> bool {
	for parser::LabelPair { label, op, value } in matches {
		if let Some(actual) = values.get(label) {
			match *op {
				parser::Operator::Equal => {
					if !actual.eq(value) {
						return false;
					}
				}
				parser::Operator::NotEqual => {
					if actual.eq(value) {
						return false;
					}
				}
				parser::Operator::RegexMatch => {
					// check if actual matches regex value
					if !regex_match(actual, value) {
						return false;
					}
				}
				parser::Operator::RegexNotMatch => {
					if regex_match(actual, value) {
						return false;
					}
				}
			}
		} else if matches!(
			op,
			parser::Operator::Equal | parser::Operator::RegexMatch
		) {
			return false;
		}
	}
	true
}

fn regex_match(actual: &str, value: &str) -> bool {
	if let Ok(r) = regex::Regex::new(value) {
		r.is_match(actual)
	} else {
		false
	}
}

fn cache_values(
	cache: &Cache<String, Arc<Vec<u8>>>,
	values: &HashMap<&String, Vec<&String>>,
) {
	for (k, v) in values {
		let key = label_values_cache_key(k);
		let resp = CacheLabelResponse {
			status: ResponseStatus::Success,
			data: v,
		};
		let d = serde_json::to_vec(&resp).unwrap();
		cache.insert(key, Arc::new(d));
	}
}

fn convert_vec_hashmap(
	input: &Vec<HashMap<String, String>>,
) -> HashMap<&String, Vec<&String>> {
	let mut result: HashMap<&String, Vec<&String>> = HashMap::new();

	for map in input {
		for (key, value) in map {
			result.entry(key).or_default().push(value);
		}
	}

	result
}

#[derive(Serialize, Debug)]
struct CacheLabelResponse<'a> {
	pub status: ResponseStatus,
	pub data: &'a Vec<&'a String>,
}

#[cfg(test)]
mod tests {
	use super::*;
	use logql::parser::{self, LabelPair};

	#[test]
	fn test_get_rest_label_pairs() {
		let test_cases = vec![
			(
				"cc:series:",
				"cc:series:-k1/0/v1",
				vec![LabelPair {
					label: "k1".to_string(),
					op: parser::Operator::Equal,
					value: "v1".to_string(),
				}],
			),
			(
				"cc:series:",
				"cc:series:-k1/0/v1-k2/1/v2",
				vec![
					LabelPair {
						label: "k1".to_string(),
						op: parser::Operator::Equal,
						value: "v1".to_string(),
					},
					LabelPair {
						label: "k2".to_string(),
						op: parser::Operator::NotEqual,
						value: "v2".to_string(),
					},
				],
			),
			(
				"cc:series:-k1/0/v1",
				"cc:series:-k1/0/v1-k2/1/v2",
				vec![LabelPair {
					label: "k2".to_string(),
					op: parser::Operator::NotEqual,
					value: "v2".to_string(),
				}],
			),
			(
				"cc:series:-k1/0/v1-k2/1/v2",
				"cc:series:-k1/0/v1-k2/1/v2-k3/2/v3",
				vec![LabelPair {
					label: "k3".to_string(),
					op: parser::Operator::RegexMatch,
					value: "v3".to_string(),
				}],
			),
		];
		for (using_key, cache_key, expected) in test_cases {
			let actual = get_rest_label_pairs(using_key, cache_key);
			assert_eq!(actual, expected);
		}
	}

	#[test]
	fn test_canonicalize_matches() {
		let test_cases = vec![
			(
				vec![LabelPair {
					label: "k1".to_string(),
					op: parser::Operator::Equal,
					value: "v1".to_string(),
				}],
				"k1/0/v1",
			),
			(
				vec![
					LabelPair {
						label: "k1".to_string(),
						op: parser::Operator::Equal,
						value: "v1".to_string(),
					},
					LabelPair {
						label: "k2".to_string(),
						op: parser::Operator::NotEqual,
						value: "v2".to_string(),
					},
				],
				"k1/0/v1-k2/1/v2",
			),
			(
				vec![
					LabelPair {
						label: "k1".to_string(),
						op: parser::Operator::Equal,
						value: "v1".to_string(),
					},
					LabelPair {
						label: "k2".to_string(),
						op: parser::Operator::NotEqual,
						value: "v2".to_string(),
					},
					LabelPair {
						label: "ServiceName".to_string(),
						op: parser::Operator::RegexNotMatch,
						value: "ss".to_string(),
					},
				],
				"ServiceName/3/ss-k1/0/v1-k2/1/v2",
			),
			(
				vec![
					LabelPair {
						label: "k1".to_string(),
						op: parser::Operator::Equal,
						value: "v1".to_string(),
					},
					LabelPair {
						label: "k2".to_string(),
						op: parser::Operator::NotEqual,
						value: "v2".to_string(),
					},
					LabelPair {
						label: "k3".to_string(),
						op: parser::Operator::RegexMatch,
						value: "v3".to_string(),
					},
				],
				"k1/0/v1-k2/1/v2-k3/2/v3",
			),
		];
		for (matches, expected) in test_cases {
			let actual = canonicalize_matches(&matches);
			assert_eq!(actual, expected);
		}
	}
}
