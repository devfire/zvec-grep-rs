//! rmcp schema-object builders shared by the tool registrations.

use schemars::JsonSchema;

/// Builds the rmcp input schema object for a wire input type.
#[must_use]
pub fn input_schema_for<T: JsonSchema>() -> std::sync::Arc<rmcp::model::JsonObject> {
    let schema = schemars::schema_for!(T);
    let value = serde_json::to_value(&schema).unwrap_or(serde_json::Value::Bool(true));
    let object = value.as_object().cloned().unwrap_or_default();
    std::sync::Arc::new(object)
}

/// Builds the rmcp output schema object for a wire output type.
#[must_use]
pub fn output_schema_for<T: JsonSchema>() -> std::sync::Arc<rmcp::model::JsonObject> {
    input_schema_for::<T>()
}
