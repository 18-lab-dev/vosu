#[cfg(test)]
pub(crate) fn serialize_projection_value<T>(value: &T) -> Result<ProjectionValue, ApiError>
where
    T: Serialize,
{
    let json =
        serde_json::to_value(value).map_err(|error| ApiError::serialization(error.to_string()))?;
    let typed =
        serde_value::to_value(value).map_err(|error| ApiError::serialization(error.to_string()))?;
    Ok(projection_value_from_serializations(json, &typed))
}

#[cfg(test)]
pub(crate) fn projection_value_from_serializations(
    json: serde_json::Value,
    typed: &serde_value::Value,
) -> ProjectionValue {
    let typed = unwrap_serde_value(typed);
    match json {
        serde_json::Value::Null
            if matches!(
                typed,
                serde_value::Value::F32(value) if !value.is_finite()
            ) || matches!(
                typed,
                serde_value::Value::F64(value) if !value.is_finite()
            ) =>
        {
            ProjectionValue::NonFiniteNumber
        }
        serde_json::Value::Null => ProjectionValue::Null,
        serde_json::Value::Bool(value) => ProjectionValue::Bool(value),
        serde_json::Value::Number(value) => ProjectionValue::Number(value),
        serde_json::Value::String(value) => ProjectionValue::String(value),
        serde_json::Value::Array(values) => {
            let typed_values = match typed {
                serde_value::Value::Seq(values) => Some(values.as_slice()),
                _ => None,
            };
            ProjectionValue::Array(
                values
                    .into_iter()
                    .enumerate()
                    .map(
                        |(index, value)| match typed_values.and_then(|values| values.get(index)) {
                            Some(typed) => projection_value_from_serializations(value, typed),
                            None => projection_value_from_json(value),
                        },
                    )
                    .collect(),
            )
        }
        serde_json::Value::Object(object) => {
            let typed_object = match typed {
                serde_value::Value::Map(object) => Some(object),
                _ => None,
            };
            ProjectionValue::Object(
                object
                    .into_iter()
                    .map(|(key, value)| {
                        let typed_value = typed_object.and_then(|object| {
                            object.iter().find_map(|(candidate, value)| {
                                matches!(candidate, serde_value::Value::String(candidate) if candidate == &key)
                                    .then_some(value)
                            })
                        });
                        let value = match typed_value {
                            Some(typed) => projection_value_from_serializations(value, typed),
                            None => projection_value_from_json(value),
                        };
                        (Cow::Owned(key), value)
                    })
                    .collect(),
            )
        }
    }
}

#[cfg(test)]
pub(crate) fn unwrap_serde_value(mut value: &serde_value::Value) -> &serde_value::Value {
    loop {
        value = match value {
            serde_value::Value::Newtype(inner) | serde_value::Value::Option(Some(inner)) => inner,
            _ => return value,
        };
    }
}


#[cfg(test)]
pub(crate) fn project_filters(value: &ProjectionValue, filters: &[Filter]) -> ProjectionValue {
    let mut projected = Vec::new();
    for filter in filters {
        match filter {
            Filter::Field(field) => {
                if let Some(value) = javascript_property(value, field) {
                    if let Some((_, existing)) = projected.iter_mut().find(|(key, _)| key == field)
                    {
                        *existing = value;
                    } else {
                        projected.push((Cow::Owned(field.clone()), value));
                    }
                }
            }
            Filter::Nested { field, keys } => {
                if let Some(value) = javascript_property(value, field)
                    .filter(|value| value != &ProjectionValue::Null)
                {
                    let value = project_filters(&value, keys);
                    if let Some((_, existing)) = projected.iter_mut().find(|(key, _)| key == field)
                    {
                        *existing = value;
                    } else {
                        projected.push((Cow::Owned(field.clone()), value));
                    }
                }
            }
        }
    }
    ProjectionValue::Object(projected)
}

#[cfg(test)]
pub(crate) fn javascript_property(value: &ProjectionValue, key: &str) -> Option<ProjectionValue> {
    javascript_property_ref(value, key).map(Cow::into_owned)
}
#[cfg(test)]
pub(crate) fn canonicalize_javascript_keys(value: ProjectionValue) -> ProjectionValue {
    match value {
        ProjectionValue::Object(object) => {
            let mut indices = Vec::new();
            let mut others = Vec::new();
            for (key, value) in object {
                let value = canonicalize_javascript_keys(value);
                if let Some(index) = javascript_array_index(&key) {
                    indices.push((index, key, value));
                } else {
                    others.push((key, value));
                }
            }
            indices.sort_by_key(|(index, _, _)| *index);
            ProjectionValue::Object(
                indices
                    .into_iter()
                    .map(|(_, key, value)| (key, value))
                    .chain(others)
                    .collect(),
            )
        }
        ProjectionValue::Array(values) => ProjectionValue::Array(
            values
                .into_iter()
                .map(canonicalize_javascript_keys)
                .collect(),
        ),
        ProjectionValue::SparseArray { length, elements } => ProjectionValue::SparseArray {
            length,
            elements: elements
                .into_iter()
                .map(|(index, value)| (index, canonicalize_javascript_keys(value)))
                .collect(),
        },
        value => value,
    }
}
