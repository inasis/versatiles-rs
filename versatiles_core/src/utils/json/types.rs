use std::collections::HashMap;

#[derive(Debug, PartialEq)]
pub enum JsonValue {
	Null,
	Str(String),
	Boolean(bool),
	Num(f64),
	Array(Vec<JsonValue>),
	Object(HashMap<String, JsonValue>),
}

impl From<&str> for JsonValue {
	fn from(input: &str) -> Self {
		JsonValue::Str(input.to_string())
	}
}

impl From<String> for JsonValue {
	fn from(input: String) -> Self {
		JsonValue::Str(input)
	}
}

impl From<bool> for JsonValue {
	fn from(input: bool) -> Self {
		JsonValue::Boolean(input)
	}
}

impl From<f64> for JsonValue {
	fn from(input: f64) -> Self {
		JsonValue::Num(input)
	}
}

impl From<i32> for JsonValue {
	fn from(input: i32) -> Self {
		JsonValue::Num(input as f64)
	}
}

impl<T> From<Vec<(&str, T)>> for JsonValue
where
	JsonValue: From<T>,
{
	fn from(input: Vec<(&str, T)>) -> Self {
		JsonValue::Object(HashMap::from_iter(
			input
				.into_iter()
				.map(|(key, value)| (key.to_string(), JsonValue::from(value))),
		))
	}
}

impl<T> From<Vec<T>> for JsonValue
where
	JsonValue: From<T>,
{
	fn from(input: Vec<T>) -> Self {
		JsonValue::Array(Vec::from_iter(input.into_iter().map(JsonValue::from)))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_from_str() {
		let input = "hello";
		let result: JsonValue = input.into();
		assert_eq!(result, JsonValue::Str("hello".to_string()));
	}

	#[test]
	fn test_from_string() {
		let result: JsonValue = String::from("hello").into();
		assert_eq!(result, JsonValue::Str("hello".to_string()));
	}

	#[test]
	fn test_from_bool() {
		let result: JsonValue = true.into();
		assert_eq!(result, JsonValue::Boolean(true));

		let result: JsonValue = false.into();
		assert_eq!(result, JsonValue::Boolean(false));
	}

	#[test]
	fn test_from_f64() {
		let result: JsonValue = 3.14.into();
		assert_eq!(result, JsonValue::Num(3.14));
	}

	#[test]
	fn test_from_i32() {
		let result: JsonValue = 42.into();
		assert_eq!(result, JsonValue::Num(42.0));
	}

	#[test]
	fn test_from_vec_of_tuples() {
		let result: JsonValue = vec![("key1", "value1"), ("key2", "value2")].into();
		assert_eq!(
			result,
			JsonValue::Object(
				vec![
					("key1".to_string(), JsonValue::Str("value1".to_string())),
					("key2".to_string(), JsonValue::Str("value2".to_string())),
				]
				.into_iter()
				.collect(),
			)
		);
	}

	#[test]
	fn test_from_vec_of_json_values() {
		let result: JsonValue = vec![
			JsonValue::Str("value1".to_string()),
			JsonValue::Boolean(true),
			JsonValue::Num(3.14),
		]
		.into();
		assert_eq!(
			result,
			JsonValue::Array(vec![
				JsonValue::Str("value1".to_string()),
				JsonValue::Boolean(true),
				JsonValue::Num(3.14),
			])
		);
	}

	#[test]
	fn test_from_vec_of_str() {
		let result: JsonValue = vec!["value1", "value2", "value3"].into();
		assert_eq!(
			result,
			JsonValue::Array(vec![
				JsonValue::Str("value1".to_string()),
				JsonValue::Str("value2".to_string()),
				JsonValue::Str("value3".to_string()),
			])
		);
	}
}
