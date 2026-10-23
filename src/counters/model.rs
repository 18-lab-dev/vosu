use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingType {
    Text,
    Color,
    Number,
    Checkbox,
    Button,
    Options,
    Commands,
    Textarea,
    Password,
    Header,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CounterSetting {
    #[serde(rename = "uniqueID")]
    pub unique_id: String,
    #[serde(
        rename = "uniqueCheck",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub unique_check: Option<String>,
    #[serde(rename = "type")]
    pub kind: SettingType,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Counter {
    #[serde(rename = "_downloaded", default, skip_serializing_if = "is_false")]
    pub downloaded: bool,
    #[serde(rename = "_updatable", default, skip_serializing_if = "is_false")]
    pub updatable: bool,
    pub folder_name: String,
    pub name: String,
    pub author: String,
    pub version: String,
    pub resolution: Vec<Value>,
    pub authorlinks: Vec<String>,
    pub settings: Vec<CounterSetting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usecase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatiblewith: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_link: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub type CompactValues = serde_json::Map<String, Value>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SettingInput {
    #[serde(rename = "uniqueID")]
    pub unique_id: String,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadRequest {
    pub url: String,
    pub name: String,
    pub update: bool,
}

/// An installed overlay a tool can load from disk (`GET /tools/_host/v1/overlays`).
/// Read-only identity: never settings values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LocalOverlay {
    /// Folder path relative to the static root, as `/api/counters` names it.
    pub id: String,
    /// metadata.txt `Name`, else the folder name.
    pub name: String,
    pub nickname: Option<String>,
    pub alias: Option<String>,
    pub author: Option<String>,
    pub version: Option<String>,
    /// Absolute canonical directory holding `index.html`.
    pub folder: String,
    /// metadata.txt `Resolution: WxH`, when both are positive.
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum LifecycleReply {
    Status { status: &'static str },
    Result { result: &'static str },
    Error { error: String },
}
