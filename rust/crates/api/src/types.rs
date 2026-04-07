use runtime::{pricing_for_model, TokenUsage, UsageCostEstimate};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThinkingConfig {
    Enabled { budget_tokens: u32 },
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortLevel {
    Low,
    Medium,
    High,
    Max,
}

impl EffortLevel {
    /// Parse an effort level string (case-insensitive). Accepts "low", "medium"/"med", "high", "max".
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "low" => Ok(Self::Low),
            "medium" | "med" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "max" => Ok(Self::Max),
            other => Err(format!(
                "invalid effort level: {} (expected low/medium/high/max)",
                other
            )),
        }
    }

    /// Return the thinking budget token count for this effort level.
    #[must_use]
    pub const fn budget_tokens(self) -> u32 {
        match self {
            Self::Low => 1024,
            Self::Medium => 4096,
            Self::High => 16384,
            Self::Max => 32768,
        }
    }

    /// Convert to a `ThinkingConfig::Enabled` with the appropriate budget.
    #[must_use]
    pub fn to_thinking_config(self) -> ThinkingConfig {
        ThinkingConfig::Enabled {
            budget_tokens: self.budget_tokens(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<InputMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingConfig>,
}

impl MessageRequest {
    #[must_use]
    pub fn with_streaming(mut self) -> Self {
        self.stream = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputMessage {
    pub role: String,
    pub content: Vec<InputContentBlock>,
}

impl InputMessage {
    #[must_use]
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: vec![InputContentBlock::Text { text: text.into() }],
        }
    }

    #[must_use]
    pub fn user_tool_result(
        tool_use_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: "user".to_string(),
            content: vec![InputContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: vec![ToolResultContentBlock::Text {
                    text: content.into(),
                }],
                is_error,
            }],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<ToolResultContentBlock>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContentBlock {
    Text { text: String },
    Json { value: Value },
    Image { source: ImageSource },
}

/// Base64-encoded image source for multimodal content blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageSource {
    /// Source type, always `"base64"` for inline images.
    #[serde(rename = "type")]
    pub kind: String,
    /// MIME type (e.g. `"image/png"`, `"image/jpeg"`).
    pub media_type: String,
    /// Base64-encoded image bytes.
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Any,
    Tool { name: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub role: String,
    pub content: Vec<OutputContentBlock>,
    pub model: String,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_sequence: Option<String>,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub request_id: Option<String>,
}

impl MessageResponse {
    #[must_use]
    pub fn total_tokens(&self) -> u32 {
        self.usage.total_tokens()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    RedactedThinking {
        data: Value,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
}

impl Usage {
    #[must_use]
    pub const fn total_tokens(&self) -> u32 {
        self.input_tokens
            + self.output_tokens
            + self.cache_creation_input_tokens
            + self.cache_read_input_tokens
    }

    #[must_use]
    pub const fn token_usage(&self) -> TokenUsage {
        TokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
        }
    }

    #[must_use]
    pub fn estimated_cost_usd(&self, model: &str) -> UsageCostEstimate {
        let usage = self.token_usage();
        pricing_for_model(model).map_or_else(
            || usage.estimate_cost_usd(),
            |pricing| usage.estimate_cost_usd_with_pricing(pricing),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageStartEvent {
    pub message: MessageResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageDeltaEvent {
    pub delta: MessageDelta,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDelta {
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub stop_sequence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentBlockStartEvent {
    pub index: u32,
    pub content_block: OutputContentBlock,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentBlockDeltaEvent {
    pub index: u32,
    pub delta: ContentBlockDelta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlockDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentBlockStopEvent {
    pub index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageStopEvent {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart(MessageStartEvent),
    MessageDelta(MessageDeltaEvent),
    ContentBlockStart(ContentBlockStartEvent),
    ContentBlockDelta(ContentBlockDeltaEvent),
    ContentBlockStop(ContentBlockStopEvent),
    MessageStop(MessageStopEvent),
}

#[cfg(test)]
mod tests {
    use runtime::format_usd;

    use super::{MessageResponse, Usage};

    #[test]
    fn usage_total_tokens_includes_cache_tokens() {
        let usage = Usage {
            input_tokens: 10,
            cache_creation_input_tokens: 2,
            cache_read_input_tokens: 3,
            output_tokens: 4,
        };

        assert_eq!(usage.total_tokens(), 19);
        assert_eq!(usage.token_usage().total_tokens(), 19);
    }

    #[test]
    fn message_response_estimates_cost_from_model_usage() {
        let response = MessageResponse {
            id: "msg_cost".to_string(),
            kind: "message".to_string(),
            role: "assistant".to_string(),
            content: Vec::new(),
            model: "claude-sonnet-4-20250514".to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            usage: Usage {
                input_tokens: 1_000_000,
                cache_creation_input_tokens: 100_000,
                cache_read_input_tokens: 200_000,
                output_tokens: 500_000,
            },
            request_id: None,
        };

        let cost = response.usage.estimated_cost_usd(&response.model);
        assert_eq!(format_usd(cost.total_cost_usd()), "$54.6750");
        assert_eq!(response.total_tokens(), 1_800_000);
    }

    #[test]
    fn test_effort_level_parse() {
        use super::EffortLevel;
        assert_eq!(EffortLevel::parse("low").unwrap(), EffortLevel::Low);
        assert_eq!(EffortLevel::parse("LOW").unwrap(), EffortLevel::Low);
        assert_eq!(EffortLevel::parse("medium").unwrap(), EffortLevel::Medium);
        assert_eq!(EffortLevel::parse("med").unwrap(), EffortLevel::Medium);
        assert_eq!(EffortLevel::parse("Med").unwrap(), EffortLevel::Medium);
        assert_eq!(EffortLevel::parse("high").unwrap(), EffortLevel::High);
        assert_eq!(EffortLevel::parse("HIGH").unwrap(), EffortLevel::High);
        assert_eq!(EffortLevel::parse("max").unwrap(), EffortLevel::Max);
        assert_eq!(EffortLevel::parse("MAX").unwrap(), EffortLevel::Max);
        assert!(EffortLevel::parse("invalid").is_err());
        assert!(EffortLevel::parse("").is_err());
        let err = EffortLevel::parse("turbo").unwrap_err();
        assert!(err.contains("invalid effort level"));
        assert!(err.contains("turbo"));
    }

    #[test]
    fn test_effort_level_budget_tokens() {
        use super::EffortLevel;
        assert_eq!(EffortLevel::Low.budget_tokens(), 1024);
        assert_eq!(EffortLevel::Medium.budget_tokens(), 4096);
        assert_eq!(EffortLevel::High.budget_tokens(), 16384);
        assert_eq!(EffortLevel::Max.budget_tokens(), 32768);
    }

    #[test]
    fn test_thinking_config_serialization() {
        use super::ThinkingConfig;
        let enabled = ThinkingConfig::Enabled {
            budget_tokens: 4096,
        };
        let json = serde_json::to_value(&enabled).expect("serialize");
        assert_eq!(json["type"], "enabled");
        assert_eq!(json["budget_tokens"], 4096);

        let disabled = ThinkingConfig::Disabled;
        let json = serde_json::to_value(&disabled).expect("serialize");
        assert_eq!(json["type"], "disabled");
    }

    #[test]
    fn test_message_request_with_thinking() {
        use super::{InputMessage, MessageRequest, ThinkingConfig};
        let request = MessageRequest {
            model: "claude-opus-4-6".to_string(),
            max_tokens: 32768,
            messages: vec![InputMessage::user_text("hello")],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
            thinking: Some(ThinkingConfig::Enabled {
                budget_tokens: 16384,
            }),
        };
        let json = serde_json::to_value(&request).expect("serialize");
        assert_eq!(json["thinking"]["type"], "enabled");
        assert_eq!(json["thinking"]["budget_tokens"], 16384);
    }

    #[test]
    fn test_message_request_without_thinking_omits_field() {
        use super::{InputMessage, MessageRequest};
        let request = MessageRequest {
            model: "claude-opus-4-6".to_string(),
            max_tokens: 4096,
            messages: vec![InputMessage::user_text("hello")],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
            thinking: None,
        };
        let json = serde_json::to_value(&request).expect("serialize");
        assert!(json.get("thinking").is_none());
    }

    #[test]
    fn test_image_source_serialization() {
        use super::{ImageSource, ToolResultContentBlock};

        let source = ImageSource {
            kind: "base64".to_string(),
            media_type: "image/png".to_string(),
            data: "iVBORw0KGgo=".to_string(),
        };

        // Verify ImageSource serializes correctly
        let json = serde_json::to_value(&source).expect("serialize ImageSource");
        assert_eq!(json["type"], "base64");
        assert_eq!(json["media_type"], "image/png");
        assert_eq!(json["data"], "iVBORw0KGgo=");

        // Verify ToolResultContentBlock::Image serializes correctly
        let block = ToolResultContentBlock::Image {
            source: source.clone(),
        };
        let json = serde_json::to_value(&block).expect("serialize Image block");
        assert_eq!(json["type"], "image");
        assert_eq!(json["source"]["type"], "base64");
        assert_eq!(json["source"]["media_type"], "image/png");
        assert_eq!(json["source"]["data"], "iVBORw0KGgo=");

        // Verify round-trip deserialization
        let deserialized: ToolResultContentBlock =
            serde_json::from_value(json).expect("deserialize Image block");
        assert_eq!(deserialized, block);
    }

    #[test]
    fn test_image_source_deserialization() {
        use super::ImageSource;

        let json = serde_json::json!({
            "type": "base64",
            "media_type": "image/jpeg",
            "data": "SGVsbG8="
        });
        let source: ImageSource = serde_json::from_value(json).expect("deserialize");
        assert_eq!(source.kind, "base64");
        assert_eq!(source.media_type, "image/jpeg");
        assert_eq!(source.data, "SGVsbG8=");
    }
}
