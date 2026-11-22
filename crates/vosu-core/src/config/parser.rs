pub(super) fn parse_toml(content: &str) -> Result<toml::Table, toml::de::Error> {
    toml::from_str(content)
}

pub(super) fn format_number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }

    value.to_string()
}
