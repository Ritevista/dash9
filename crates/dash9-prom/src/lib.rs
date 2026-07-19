//! Prometheus datasource adapter.
//!
//! Implements the `Datasource` trait from `dash9-core` against the
//! Prometheus HTTP API (`/api/v1/query`, `/api/v1/query_range`),
//! normalizing responses to `dash9_core::Frame` at the boundary.
//! Prometheus's fractional Unix-seconds timestamps are converted to
//! `i64` milliseconds before a `Frame` is ever constructed (SPEC.md
//! A.3); nothing upstream of this crate's response-parsing boundary
//! sees the native Prometheus JSON shape.

use std::time::{SystemTime, UNIX_EPOCH};

use dash9_core::{Datasource, Frame, FrameKind, FrameMeta, Labels, Point, Series};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PrometheusError {
    #[error("request to {url} failed: {source}")]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("invalid response body: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("prometheus returned {error_type}: {message}")]
    Api { error_type: String, message: String },
    #[error("success response was missing its data field")]
    MissingData,
    #[error("unexpected result type \"{0}\"")]
    UnexpectedResultType(String),
    #[error("invalid sample value \"{0}\"")]
    InvalidValue(String),
    #[error("invalid sample timestamp {0}")]
    InvalidTimestamp(f64),
}

/// One named Prometheus instance. Construct one per `[[datasources]]`
/// entry (SPEC.md C.1); `name` becomes every produced `Frame`'s
/// `meta.datasource`.
pub struct PrometheusDatasource {
    name: String,
    base_url: String,
    client: reqwest::Client,
}

impl PrometheusDatasource {
    pub fn new(name: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
            client: reqwest::Client::new(),
        }
    }

    async fn get(&self, path: &str, params: &[(&str, String)]) -> Result<String, PrometheusError> {
        let url = format!("{}{path}", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .get(&url)
            .query(params)
            .send()
            .await
            .map_err(|source| PrometheusError::Request {
                url: url.clone(),
                source,
            })?;
        response
            .text()
            .await
            .map_err(|source| PrometheusError::Request { url, source })
    }
}

impl Datasource for PrometheusDatasource {
    type Error = PrometheusError;

    async fn query(&self, query: &str, time_ms: i64) -> Result<Frame, PrometheusError> {
        let executed_at_ms = epoch_ms_now();
        let body = self
            .get(
                "/api/v1/query",
                &[
                    ("query", query.to_string()),
                    ("time", seconds_param(time_ms)),
                ],
            )
            .await?;
        parse_response(&body, &self.name, query, executed_at_ms)
    }

    async fn query_range(
        &self,
        query: &str,
        start_ms: i64,
        end_ms: i64,
        step_ms: i64,
    ) -> Result<Frame, PrometheusError> {
        let executed_at_ms = epoch_ms_now();
        let body = self
            .get(
                "/api/v1/query_range",
                &[
                    ("query", query.to_string()),
                    ("start", seconds_param(start_ms)),
                    ("end", seconds_param(end_ms)),
                    ("step", seconds_param(step_ms)),
                ],
            )
            .await?;
        parse_response(&body, &self.name, query, executed_at_ms)
    }
}

#[allow(clippy::cast_precision_loss)]
fn seconds_param(ms: i64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

fn epoch_ms_now() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
}

#[derive(Deserialize)]
struct RawResponse {
    status: String,
    #[serde(default)]
    data: Option<RawData>,
    #[serde(rename = "errorType", default)]
    error_type: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
struct RawData {
    #[serde(rename = "resultType")]
    result_type: String,
    result: serde_json::Value,
}

#[derive(Deserialize)]
struct VectorResult {
    metric: Labels,
    value: (f64, String),
}

#[derive(Deserialize)]
struct MatrixResult {
    metric: Labels,
    values: Vec<(f64, String)>,
}

/// Parses one Prometheus HTTP API response body into a `Frame`. This
/// is the entire normalization boundary (SPEC.md Section A): nothing
/// upstream of this function ever sees Prometheus's native JSON
/// shape, and every timestamp conversion happens here, before a
/// `Frame` exists.
fn parse_response(
    body: &str,
    datasource_name: &str,
    query: &str,
    executed_at_ms: i64,
) -> Result<Frame, PrometheusError> {
    let parsed: RawResponse = serde_json::from_str(body)?;
    if parsed.status != "success" {
        return Err(PrometheusError::Api {
            error_type: parsed.error_type.unwrap_or_default(),
            message: parsed.error.unwrap_or_default(),
        });
    }
    let data = parsed.data.ok_or(PrometheusError::MissingData)?;

    let (kind, series) = match data.result_type.as_str() {
        "vector" => {
            let results: Vec<VectorResult> = serde_json::from_value(data.result)?;
            let series = results
                .into_iter()
                .map(|r| {
                    let (ts, value) = r.value;
                    Ok(Series {
                        labels: r.metric,
                        points: vec![to_point(ts, &value)?],
                    })
                })
                .collect::<Result<Vec<_>, PrometheusError>>()?;
            (FrameKind::InstantVector, series)
        }
        "matrix" => {
            let results: Vec<MatrixResult> = serde_json::from_value(data.result)?;
            let series = results
                .into_iter()
                .map(|r| {
                    let points = r
                        .values
                        .into_iter()
                        .map(|(ts, value)| to_point(ts, &value))
                        .collect::<Result<Vec<_>, PrometheusError>>()?;
                    Ok(Series {
                        labels: r.metric,
                        points,
                    })
                })
                .collect::<Result<Vec<_>, PrometheusError>>()?;
            (FrameKind::Timeseries, series)
        }
        "scalar" => {
            let (ts, value): (f64, String) = serde_json::from_value(data.result)?;
            let point = to_point(ts, &value)?;
            (
                FrameKind::InstantVector,
                vec![Series {
                    labels: Labels::new(),
                    points: vec![point],
                }],
            )
        }
        other => return Err(PrometheusError::UnexpectedResultType(other.to_string())),
    };

    Ok(Frame {
        kind,
        series,
        table: None,
        meta: FrameMeta {
            query: query.to_string(),
            datasource: datasource_name.to_string(),
            executed_at_ms,
            warnings: vec![],
        },
    })
}

#[allow(clippy::cast_possible_truncation)]
fn to_point(timestamp_seconds: f64, raw_value: &str) -> Result<Point, PrometheusError> {
    if !timestamp_seconds.is_finite() {
        return Err(PrometheusError::InvalidTimestamp(timestamp_seconds));
    }
    let value = raw_value
        .trim()
        .parse::<f64>()
        .map_err(|_| PrometheusError::InvalidValue(raw_value.to_string()))?;
    let timestamp_ms = (timestamp_seconds * 1000.0).round() as i64;
    Ok(Point {
        timestamp_ms,
        value,
    })
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn vector_response_becomes_an_instant_vector_frame() {
        let body = r#"{
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": [
                    { "metric": { "instance": "10.0.0.1:9100", "job": "node" }, "value": [1700000015.421, "0.47"] }
                ]
            }
        }"#;
        let frame = parse_response(body, "prom", "node_load1", 1_700_000_015_421).unwrap();
        assert_eq!(frame.kind, FrameKind::InstantVector);
        assert_eq!(frame.series.len(), 1);
        assert_eq!(frame.series[0].labels["job"], "node");
        assert_eq!(frame.series[0].points.len(), 1);
        assert_eq!(frame.series[0].points[0].value, 0.47);
        assert_eq!(frame.series[0].points[0].timestamp_ms, 1_700_000_015_421);
        assert_eq!(frame.meta.datasource, "prom");
        assert_eq!(frame.meta.query, "node_load1");
        assert!(!frame.is_empty());
    }

    #[test]
    fn matrix_response_becomes_a_timeseries_frame() {
        let body = r#"{
            "status": "success",
            "data": {
                "resultType": "matrix",
                "result": [
                    {
                        "metric": { "instance": "10.0.0.1:9100", "job": "node" },
                        "values": [[1700000000.000, "0.42"], [1700000015.000, "0.47"]]
                    }
                ]
            }
        }"#;
        let frame = parse_response(body, "prom", "rate(x[5m])", 0).unwrap();
        assert_eq!(frame.kind, FrameKind::Timeseries);
        assert_eq!(frame.series[0].points.len(), 2);
        assert_eq!(frame.series[0].points[0].timestamp_ms, 1_700_000_000_000);
        assert_eq!(frame.series[0].points[1].timestamp_ms, 1_700_000_015_000);
    }

    #[test]
    fn scalar_response_becomes_a_single_unlabeled_series() {
        let body =
            r#"{"status":"success","data":{"resultType":"scalar","result":[1700000000.0,"2"]}}"#;
        let frame = parse_response(body, "prom", "1+1", 0).unwrap();
        assert_eq!(frame.kind, FrameKind::InstantVector);
        assert_eq!(frame.series.len(), 1);
        assert!(frame.series[0].labels.is_empty());
        assert_eq!(frame.series[0].points[0].value, 2.0);
    }

    #[test]
    fn empty_vector_result_is_an_empty_frame_not_an_error() {
        let body = r#"{"status":"success","data":{"resultType":"vector","result":[]}}"#;
        let frame = parse_response(body, "prom", "nonexistent_metric", 0).unwrap();
        assert!(frame.is_empty());
    }

    #[test]
    fn error_status_is_reported_as_api_error() {
        let body =
            r#"{"status":"error","errorType":"bad_data","error":"invalid parameter \"query\""}"#;
        let err = parse_response(body, "prom", "up{", 0).unwrap_err();
        match err {
            PrometheusError::Api {
                error_type,
                message,
            } => {
                assert_eq!(error_type, "bad_data");
                assert!(message.contains("invalid parameter"));
            }
            other => panic!("expected Api error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_result_type_is_rejected() {
        let body =
            r#"{"status":"success","data":{"resultType":"string","result":[1700000000.0,"hi"]}}"#;
        let err = parse_response(body, "prom", "q", 0).unwrap_err();
        assert!(matches!(err, PrometheusError::UnexpectedResultType(t) if t == "string"));
    }

    #[test]
    fn non_finite_and_infinite_sample_values_are_handled() {
        let body = r#"{
            "status": "success",
            "data": {
                "resultType": "vector",
                "result": [
                    { "metric": {}, "value": [1700000000.0, "+Inf"] }
                ]
            }
        }"#;
        let frame = parse_response(body, "prom", "1/0", 0).unwrap();
        assert_eq!(frame.series[0].points[0].value, f64::INFINITY);
    }

    #[test]
    fn non_numeric_sample_value_is_rejected() {
        let err = to_point(1_700_000_000.0, "not-a-number").unwrap_err();
        assert!(matches!(err, PrometheusError::InvalidValue(v) if v == "not-a-number"));
    }

    #[test]
    fn malformed_json_body_is_rejected() {
        let err = parse_response("not json", "prom", "up", 0).unwrap_err();
        assert!(matches!(err, PrometheusError::InvalidJson(_)));
    }

    #[test]
    fn missing_data_on_success_is_rejected() {
        let body = r#"{"status":"success"}"#;
        let err = parse_response(body, "prom", "up", 0).unwrap_err();
        assert!(matches!(err, PrometheusError::MissingData));
    }

    #[test]
    fn seconds_param_formats_milliseconds_as_fractional_seconds() {
        assert_eq!(seconds_param(1_700_000_015_421), "1700000015.421");
        assert_eq!(seconds_param(0), "0.000");
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod live_tests {
    //! These spin up a throwaway local TCP listener and hand-roll one
    //! canned HTTP response — no real network access, no mocking
    //! dependency, fully offline and deterministic. They exist to
    //! prove the request-building/response-reading wrapper around
    //! `parse_response` actually works, not just the pure parsing
    //! logic covered above.
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn respond_once(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn query_hits_the_http_api_and_normalizes_the_response() {
        let body = r#"{"status":"success","data":{"resultType":"vector","result":[{"metric":{"job":"node"},"value":[1700000000.000,"0.5"]}]}}"#;
        let base_url = respond_once(body).await;
        let ds = PrometheusDatasource::new("prom", base_url);
        let frame = ds.query("up", 1_700_000_000_000).await.unwrap();
        assert_eq!(frame.kind, FrameKind::InstantVector);
        assert_eq!(frame.series[0].points[0].value, 0.5);
        assert_eq!(frame.meta.datasource, "prom");
    }

    #[tokio::test]
    async fn query_range_hits_the_http_api_and_normalizes_the_response() {
        let body = r#"{"status":"success","data":{"resultType":"matrix","result":[{"metric":{"job":"node"},"values":[[1700000000.0,"1"],[1700000015.0,"2"]]}]}}"#;
        let base_url = respond_once(body).await;
        let ds = PrometheusDatasource::new("prom", base_url);
        let frame = ds
            .query_range("up", 1_700_000_000_000, 1_700_000_015_000, 15_000)
            .await
            .unwrap();
        assert_eq!(frame.kind, FrameKind::Timeseries);
        assert_eq!(frame.series[0].points.len(), 2);
    }

    #[tokio::test]
    async fn api_error_status_surfaces_as_prometheus_error() {
        let body = r#"{"status":"error","errorType":"bad_data","error":"bad query"}"#;
        let base_url = respond_once(body).await;
        let ds = PrometheusDatasource::new("prom", base_url);
        let err = ds.query("up{", 0).await.unwrap_err();
        assert!(matches!(err, PrometheusError::Api { .. }));
    }
}
