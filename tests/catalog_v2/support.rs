use serde_json::{Value, json};
use vosu_plugin_api::manifest::PackageManifest;

pub struct Case {
    pub label: String,
    pub valid: Value,
    pub invalid: Value,
    pub rule: &'static str,
    pub pointer: String,
    pub shape: bool,
}
impl Case {
    pub fn mutation(
        label: impl Into<String>,
        valid: &Value,
        path: &str,
        bad: Value,
        rule: &'static str,
        pointer: &str,
        shape: bool,
    ) -> Self {
        let mut invalid = valid.clone();
        *invalid.pointer_mut(path).expect(path) = bad;
        Self {
            label: label.into(),
            valid: valid.clone(),
            invalid,
            rule,
            pointer: pointer.into(),
            shape,
        }
    }
    pub fn addition(
        label: impl Into<String>,
        valid: &Value,
        key: &str,
        bad: Value,
        rule: &'static str,
        pointer: &str,
        shape: bool,
    ) -> Self {
        let mut invalid = valid.clone();
        invalid[key] = bad;
        Self {
            label: label.into(),
            valid: valid.clone(),
            invalid,
            rule,
            pointer: pointer.into(),
            shape,
        }
    }
}
pub fn manifest(kind: &str) -> Value {
    let mut value = json!({"manifest_version":1,"kind":kind,"id":"corpus",
        "name":"Corpus","version":"9.0.0","host":format!("={}",vosu::application_version())});
    match kind {
        "plugin" => {
            value["id"] = json!("acme.corpus");
            value["abi_version"] = json!(1);
            value["runtime"] = json!("in-process");
        }
        "tool" => value["ui"] = json!({"root":"web"}),
        "overlay" => (),
        _ => panic!("unknown fixture kind"),
    }
    value
}
pub fn text(value: &Value) -> String {
    toml::to_string(value).unwrap()
}
pub fn parse(value: &Value) -> Result<PackageManifest, vosu_plugin_api::manifest::ValidationError> {
    PackageManifest::parse(&text(value))
}
pub fn offline(name: &str) -> jsonschema::Validator {
    let bytes = match name {
        "manifest" => include_str!("../../docs/v2/schemas/manifest.schema.json"),
        "catalog" => include_str!("../../docs/v2/schemas/catalog-v2.schema.json"),
        _ => panic!("unknown offline schema"),
    };
    let value: Value = serde_json::from_str(bytes).unwrap();
    fn local(v: &Value) {
        match v {
            Value::Object(o) => {
                for (key, v) in o {
                    if key == "$ref" {
                        assert!(v.as_str().unwrap().starts_with('#'));
                    }
                    local(v);
                }
            }
            Value::Array(a) => a.iter().for_each(local),
            _ => (),
        }
    }
    local(&value);
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&value)
        .unwrap()
}
