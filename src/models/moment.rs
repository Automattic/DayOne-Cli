use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};
use serde_json::Value;

use crate::models::MomentId;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Moment {
    #[serde(default)]
    pub id: Option<MomentId>,
    #[serde(rename = "type", alias = "momentType", alias = "moment_type", default)]
    pub kind: Option<String>,
    #[serde(alias = "content_type", default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub md5: Option<String>,
    #[serde(
        alias = "file_size",
        default,
        deserialize_with = "deserialize_optional_lenient_i64"
    )]
    pub file_size: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_lenient_i64")]
    pub height: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_lenient_i64")]
    pub width: Option<i64>,
    #[serde(alias = "pdf_name", default)]
    pub pdf_name: Option<String>,
    #[serde(default)]
    pub thumbnail: Option<MomentThumbnail>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MomentThumbnail {
    #[serde(alias = "content_type", default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub md5: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_lenient_i64")]
    pub height: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_optional_lenient_i64")]
    pub width: Option<i64>,
    #[serde(
        alias = "file_size",
        default,
        deserialize_with = "deserialize_optional_lenient_i64"
    )]
    pub file_size: Option<i64>,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Moment size fields have been observed from real clients as integers,
/// floats, and numeric strings; accept all of them instead of failing the
/// whole entry deserialization.
fn deserialize_optional_lenient_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_f64().and_then(f64_to_i64_rounded))
            .map(Some)
            .ok_or_else(|| DeError::custom(format!("numeric value out of i64 range: {number}"))),
        Some(Value::String(raw)) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            trimmed
                .parse::<i64>()
                .ok()
                .or_else(|| trimmed.parse::<f64>().ok().and_then(f64_to_i64_rounded))
                .map(Some)
                .ok_or_else(|| DeError::custom(format!("expected numeric string, got {raw:?}")))
        }
        Some(other) => Err(DeError::custom(format!(
            "expected number, numeric string, or null, got {other}"
        ))),
    }
}

/// Checked float→int conversion: a plain `as i64` cast saturates out-of-range
/// values to `i64::MIN`/`i64::MAX` (and maps NaN to 0), silently corrupting
/// them; reject those instead so deserialization fails loudly.
fn f64_to_i64_rounded(value: f64) -> Option<i64> {
    // 2^63 is exactly representable as f64, i64::MAX (2^63 - 1) is not, so
    // the upper bound is exclusive.
    const I64_MIN_F64: f64 = i64::MIN as f64;
    const I64_MAX_PLUS_ONE_F64: f64 = 9_223_372_036_854_775_808.0;
    let rounded = value.round();
    if rounded.is_finite() && (I64_MIN_F64..I64_MAX_PLUS_ONE_F64).contains(&rounded) {
        Some(rounded as i64)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{Moment, MomentThumbnail};
    use serde_json::{Value, json};

    #[test]
    fn moment_round_trips_unknown_fields() {
        let input = json!({
            "id": "m1",
            "type": "image",
            "contentType": "image/jpeg",
            "fileSize": 123,
            "thumbnail": {
                "contentType": "image/jpeg",
                "fileSize": 12,
                "customThumb": true
            },
            "customMoment": "extra"
        });
        let moment: Moment = serde_json::from_value(input).expect("moment should deserialize");
        assert_eq!(moment.id.as_ref().map(|id| id.as_ref()), Some("m1"));
        assert_eq!(
            moment.thumbnail.as_ref().and_then(|t| t.file_size),
            Some(12)
        );
        assert_eq!(
            moment
                .thumbnail
                .as_ref()
                .and_then(|t| t.extra.get("customThumb")),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            moment.extra.get("customMoment"),
            Some(&Value::String("extra".to_owned()))
        );

        let serialized = serde_json::to_value(&moment).expect("moment should serialize");
        assert_eq!(
            serialized.get("customMoment"),
            Some(&Value::String("extra".to_owned()))
        );
        assert_eq!(
            serialized
                .get("thumbnail")
                .and_then(|v| v.get("customThumb")),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn moment_accepts_lenient_numeric_size_fields() {
        let moment: Moment = serde_json::from_value(json!({
            "id": "m1",
            "fileSize": 42.4,
            "width": "640",
            "height": "480.6"
        }))
        .expect("moment should deserialize");
        assert_eq!(moment.file_size, Some(42));
        assert_eq!(moment.width, Some(640));
        assert_eq!(moment.height, Some(481));
    }

    #[test]
    fn moment_rejects_out_of_range_numeric_size_fields_instead_of_saturating() {
        // 1e19 > i64::MAX; a plain `as i64` cast would clamp it to i64::MAX.
        let err = serde_json::from_value::<Moment>(json!({ "fileSize": 1e19 }))
            .expect_err("out-of-range float should fail");
        assert!(
            err.to_string().contains("out of i64 range"),
            "unexpected error: {err}"
        );

        let err = serde_json::from_value::<Moment>(json!({ "fileSize": "1e19" }))
            .expect_err("out-of-range numeric string should fail");
        assert!(
            err.to_string().contains("expected numeric string"),
            "unexpected error: {err}"
        );

        let err = serde_json::from_value::<Moment>(json!({ "fileSize": "NaN" }))
            .expect_err("NaN string should fail");
        assert!(
            err.to_string().contains("expected numeric string"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn thumbnail_aliases_support_snake_case() {
        let thumb: MomentThumbnail = serde_json::from_value(json!({
            "content_type": "image/jpeg",
            "file_size": 9
        }))
        .expect("thumbnail should deserialize");
        assert_eq!(thumb.content_type.as_deref(), Some("image/jpeg"));
        assert_eq!(thumb.file_size, Some(9));
    }
}
