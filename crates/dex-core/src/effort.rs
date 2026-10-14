//! Görev #671: the leader's per-job reasoning effort. Low and max are excluded — the type
//! holds only medium | high | xhigh. Claude Code (anthropic, glm, muse) gets it as
//! `CLAUDE_CODE_EFFORT_LEVEL` (the env beats `--effort`, measured in #637). Never `/effort` in a
//! session: it writes the patron's settings.

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Medium,
    High,
    Xhigh,
}

pub const CLAUDE_ENV: &str = "CLAUDE_CODE_EFFORT_LEVEL";

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
        }
    }

    pub fn parse(s: &str) -> Result<Effort, String> {
        match s.trim() {
            "medium" => Ok(Effort::Medium),
            "high" => Ok(Effort::High),
            "xhigh" => Ok(Effort::Xhigh),
            other => Err(format!("geçersiz effort: {other:?} (yalnız medium | high | xhigh)")),
        }
    }

    /// An MCP argument: absent or null is none; anything else must be one of the three.
    pub fn from_arg(v: Option<&serde_json::Value>) -> Result<Option<Effort>, String> {
        match v {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(s)) => Effort::parse(s).map(Some),
            Some(other) => Err(format!("geçersiz effort: {other} (yalnız medium | high | xhigh)")),
        }
    }
}

/// MCP schema of the `effort` field (delegate, plan tasks).
pub fn schema() -> serde_json::Value {
    serde_json::json!({ "type": "string", "enum": ["medium", "high", "xhigh"],
        "description": "Bu işin düşünme düzeyi; her işte seç. Worker süreci bununla açılır. GLM'de medium = high; Haiku'da etkisiz." })
}

impl ToSql for Effort {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

impl FromSql for Effort {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        Effort::parse(v.as_str()?).map_err(|e| FromSqlError::Other(e.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_medium_high_xhigh() {
        for (s, e) in [("medium", Effort::Medium), ("high", Effort::High), ("xhigh", Effort::Xhigh)] {
            assert_eq!(Effort::parse(s), Ok(e));
            assert_eq!(e.as_str(), s);
            assert_eq!(serde_json::to_value(e).unwrap(), serde_json::json!(s));
        }
        for bad in ["low", "max", "High", "", "ultra"] {
            assert!(Effort::parse(bad).unwrap_err().contains("medium | high | xhigh"), "{bad}");
            assert!(serde_json::from_value::<Effort>(serde_json::json!(bad)).is_err(), "{bad}");
            assert!(Effort::from_arg(Some(&serde_json::json!(bad))).is_err(), "{bad}");
        }
        assert_eq!(Effort::from_arg(None), Ok(None));
        assert_eq!(Effort::from_arg(Some(&serde_json::Value::Null)), Ok(None));
        assert_eq!(Effort::from_arg(Some(&serde_json::json!("xhigh"))), Ok(Some(Effort::Xhigh)));
        assert!(Effort::from_arg(Some(&serde_json::json!(3))).is_err());
    }
}
