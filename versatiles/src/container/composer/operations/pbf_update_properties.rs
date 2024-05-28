use super::{TileComposerOperation, TileComposerOperationLookup};
use crate::{
	container::{composer::utils::read_csv_file, TilesReaderParameters, TilesStream},
	types::Blob,
	utils::{
		decompress,
		geometry::{vector_tile::VectorTile, GeoProperties},
		YamlWrapper,
	},
};
use anyhow::{anyhow, ensure, Context, Result};
use async_trait::async_trait;
use futures::{future::ready, StreamExt};
use std::{
	collections::{BTreeMap, HashMap},
	fmt::Debug,
	path::Path,
};
use versatiles_core::types::{TileBBox, TileCompression, TileCoord3, TileFormat};
use versatiles_derive::YamlParser;

#[derive(YamlParser)]
struct Config {
	input: String,
	data_source_path: String,
	id_field_tiles: String,
	id_field_values: String,
	replace_properties: bool,
	remove_empty_properties: bool,
	also_save_id: bool,
}

/// The `PBFReplacePropertiesOperation` struct represents an operation that replaces properties in PBF tiles
/// based on a mapping provided in a CSV file.
pub struct Operation {
	config: Config,
	input: Box<dyn TileComposerOperation>,
	name: String,
	parameters: TilesReaderParameters,
	input_compression: TileCompression,
	properties_map: HashMap<String, GeoProperties>,
}

impl Operation {
	fn run(&self, blob: Blob) -> Result<Option<Blob>> {
		let mut tile =
			VectorTile::from_blob(&blob).context("Failed to create VectorTile from Blob")?;

		for layer in tile.layers.iter_mut() {
			layer.map_properties(|properties| {
				if let Some(mut prop) = properties {
					if let Some(id) = prop.get(&self.config.id_field_tiles) {
						if let Some(new_prop) = self.properties_map.get(&id.to_string()) {
							if self.config.replace_properties {
								prop = new_prop.clone();
							} else {
								prop.update(new_prop.clone());
							}
							return Some(prop);
						}
					}
				}
				None
			})?;

			if self.config.remove_empty_properties {
				layer.retain_features(|feature| !feature.tag_ids.is_empty());
			}
		}

		Ok(Some(
			tile
				.to_blob()
				.context("Failed to convert VectorTile to Blob")?,
		))
	}
}

#[async_trait]
impl TileComposerOperation for Operation {
	/// Creates a new `PBFReplacePropertiesOperation` from the provided YAML configuration.
	///
	/// # Arguments
	///
	/// * `yaml` - A reference to a `YamlWrapper` containing the configuration.
	///
	/// # Returns
	///
	/// * `Result<PBFReplacePropertiesOperation>` - The constructed operation or an error if the configuration is invalid.
	async fn new(
		name: &str,
		yaml: YamlWrapper,
		lookup: &mut TileComposerOperationLookup,
	) -> Result<Self>
	where
		Self: Sized,
	{
		let config = Config::from_yaml(&yaml)?;

		let data = read_csv_file(Path::new(&config.data_source_path))
			.with_context(|| format!("Failed to read CSV file from '{}'", config.data_source_path))?;

		let properties_map = data
			.into_iter()
			.map(|mut properties| {
				let key = properties
					.get(&config.id_field_values)
					.ok_or_else(|| anyhow!("Key '{}' not found in CSV data", config.id_field_values))
					.with_context(|| {
						format!(
							"Failed to find key '{}' in the CSV data row: {properties:?}",
							config.id_field_values
						)
					})?
					.to_string();
				if !config.also_save_id {
					properties.remove(&config.id_field_values)
				}
				Ok((key, properties))
			})
			.collect::<Result<HashMap<String, GeoProperties>>>()
			.context("Failed to build properties map from CSV data")?;

		let input = lookup.construct(&config.input).await?;

		let mut parameters = input.get_parameters().await.clone();
		ensure!(
			parameters.tile_format == TileFormat::PBF,
			"operation '{name}' needs vector tiles (PBF) from '{}'",
			config.input
		);

		let input_compression = parameters.tile_compression;
		parameters.tile_compression = TileCompression::None;

		Ok(Operation {
			config,
			input,
			input_compression,
			name: name.to_string(),
			parameters,
			properties_map,
		})
	}

	async fn get_bbox_tile_stream(&self, bbox: TileBBox) -> TilesStream {
		self
			.input
			.get_bbox_tile_stream(bbox)
			.await
			.filter_map(|(coord, blob)| {
				let blob = decompress(blob, &self.input_compression).unwrap();
				let blob = self.run(blob).unwrap();
				ready(if let Some(inner) = blob {
					Some((coord, inner))
				} else {
					None
				})
			})
			.boxed()
	}

	async fn get_parameters(&self) -> &TilesReaderParameters {
		&self.parameters
	}

	async fn get_meta(&self) -> Result<Option<Blob>> {
		self.input.get_meta().await
	}

	async fn get_tile_data(&self, coord: &TileCoord3) -> Result<Option<Blob>> {
		let blob = self.input.get_tile_data(coord).await?;
		if let Some(blob) = blob {
			self.run(decompress(blob, &self.input_compression)?)
		} else {
			Ok(None)
		}
	}
}

impl Debug for Operation {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("PBFReplacePropertiesOperation")
			.field("name", &self.name)
			.field(
				"properties_map",
				&BTreeMap::from_iter(self.properties_map.iter()),
			)
			.field("id_field_tiles", &self.config.id_field_tiles)
			.field(
				"remove_empty_properties",
				&self.config.remove_empty_properties,
			)
			.finish()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::container::{TileComposerReader, TilesReader};

	async fn test(
		parameters: (&str, &str, &[(&str, bool)]),
		debug_operation: &str,
		debug_result: &str,
	) -> Result<()> {
		let flags = parameters
			.2
			.iter()
			.map(|(key, val)| format!("    {key}: {}", if *val { "true" } else { "false" }))
			.collect::<Vec<String>>()
			.join("\n");

		let yaml = vec![
			"operations:",
			"  source:",
			"    action: pbf_mock",
			"  update_values:",
			"    action: pbf_update_properties",
			"    input: source",
			"    data_source_path: ../testdata/cities.csv",
			&format!("    id_field_tiles: {}", parameters.0),
			&format!("    id_field_values: {}", parameters.1),
			&flags,
			"output: update_values",
		]
		.join("\n");

		let mut reader = TileComposerReader::open_str(&yaml).await?;

		assert_eq!(
			cleanup(format!("{:?}", reader.output.operation)),
			debug_operation
		);

		let blob = reader
			.get_tile_data(&TileCoord3::new(0, 0, 0)?)
			.await?
			.unwrap();

		let layers = VectorTile::from_blob(&blob)?.layers;
		assert_eq!(layers.len(), 1);
		let tile = layers[0].to_features()?;

		assert_eq!(cleanup(format!("{tile:?}")), debug_result);

		fn cleanup(text: String) -> String {
			text
				.replace(
					"\"city_name\": String(\"Berlin\"), \"city_population\": UInt(3755251)",
					"…Berlin…",
				)
				.replace(
					"\"city_name\": String(\"Kyiv\"), \"city_population\": UInt(2952301)",
					"…Kyiv…",
				)
				.replace(
					"\"city_name\": String(\"Plovdiv\"), \"city_population\": UInt(346893)",
					"…Plovdiv…",
				)
				.replace(
					"geometry: MultiPolygon([[[[0.0, 0.0], [5.0, 0.0], [3.0, 4.0], [0.0, 0.0]], [[2.0, 1.0], [3.0, 2.0], [3.0, 1.0], [2.0, 1.0]]], [[[6.0, 0.0], [9.0, 0.0], [9.0, 4.0], [6.0, 4.0], [6.0, 0.0]], [[7.0, 1.0], [7.0, 3.0], [8.0, 3.0], [8.0, 1.0], [7.0, 1.0]]]]),",
					"…geometry…"
				)
		}

		Ok(())
	}

	#[tokio::test]
	async fn test_new() -> Result<()> {
		test(
			("tile_id", "city_id", &[]),
			"PBFReplacePropertiesOperation { name: \"update_values\", properties_map: {\"1\": {…Berlin…}, \"2\": {…Kyiv…}, \"3\": {…Plovdiv…}}, id_field_tiles: \"tile_id\", remove_empty_properties: false }",
			"[Feature { id: None, …geometry… properties: Some({…Berlin…, \"tile_id\": UInt(1), \"tile_name\": String(\"Bärlin\")}) }, Feature { id: None, …geometry… properties: None }]"
		).await
	}

	#[tokio::test]
	async fn test_unknown_key() {
		assert_eq!(
			test(("tile_id", "unknown_id", &[]), "", "")
				.await
				.unwrap_err()
				.chain()
				.last()
				.unwrap()
				.to_string(),
			"Key 'unknown_id' not found in CSV data"
		);
	}

	#[tokio::test]
	async fn test_replace_properties() -> Result<()> {
		test(
			("tile_id", "city_id", &[("replace_properties", false)]),
			"PBFReplacePropertiesOperation { name: \"update_values\", properties_map: {\"1\": {…Berlin…}, \"2\": {…Kyiv…}, \"3\": {…Plovdiv…}}, id_field_tiles: \"tile_id\", remove_empty_properties: false }",
			"[Feature { id: None, …geometry… properties: Some({…Berlin…, \"tile_id\": UInt(1), \"tile_name\": String(\"Bärlin\")}) }, Feature { id: None, …geometry… properties: None }]"
		).await?;
		test(
			("tile_id", "city_id", &[("replace_properties", true)]),
			"PBFReplacePropertiesOperation { name: \"update_values\", properties_map: {\"1\": {…Berlin…}, \"2\": {…Kyiv…}, \"3\": {…Plovdiv…}}, id_field_tiles: \"tile_id\", remove_empty_properties: false }",
			"[Feature { id: None, …geometry… properties: Some({…Berlin…}) }, Feature { id: None, …geometry… properties: None }]"
		).await
	}

	#[tokio::test]
	async fn test_remove_empty_properties() -> Result<()> {
		test(
			("tile_id", "city_id", &[("remove_empty_properties", false)]),
			"PBFReplacePropertiesOperation { name: \"update_values\", properties_map: {\"1\": {…Berlin…}, \"2\": {…Kyiv…}, \"3\": {…Plovdiv…}}, id_field_tiles: \"tile_id\", remove_empty_properties: false }",
			"[Feature { id: None, …geometry… properties: Some({…Berlin…, \"tile_id\": UInt(1), \"tile_name\": String(\"Bärlin\")}) }, Feature { id: None, …geometry… properties: None }]"
		).await?;
		test(
			("tile_id", "city_id", &[("remove_empty_properties", true)]),
			"PBFReplacePropertiesOperation { name: \"update_values\", properties_map: {\"1\": {…Berlin…}, \"2\": {…Kyiv…}, \"3\": {…Plovdiv…}}, id_field_tiles: \"tile_id\", remove_empty_properties: true }",
			"[Feature { id: None, …geometry… properties: Some({…Berlin…, \"tile_id\": UInt(1), \"tile_name\": String(\"Bärlin\")}) }]"
		).await
	}

	#[tokio::test]
	async fn test_also_save_id() -> Result<()> {
		test(
			("tile_id", "city_id", &[("also_save_id", false)]),
			"PBFReplacePropertiesOperation { name: \"update_values\", properties_map: {\"1\": {…Berlin…}, \"2\": {…Kyiv…}, \"3\": {…Plovdiv…}}, id_field_tiles: \"tile_id\", remove_empty_properties: false }",
			"[Feature { id: None, …geometry… properties: Some({…Berlin…, \"tile_id\": UInt(1), \"tile_name\": String(\"Bärlin\")}) }, Feature { id: None, …geometry… properties: None }]"
		).await?;
		test(
			("tile_id", "city_id", &[("also_save_id", true)]),
			"PBFReplacePropertiesOperation { name: \"update_values\", properties_map: {\"1\": {\"city_id\": UInt(1), …Berlin…}, \"2\": {\"city_id\": UInt(2), …Kyiv…}, \"3\": {\"city_id\": UInt(3), …Plovdiv…}}, id_field_tiles: \"tile_id\", remove_empty_properties: false }",
			"[Feature { id: None, …geometry… properties: Some({\"city_id\": UInt(1), …Berlin…, \"tile_id\": UInt(1), \"tile_name\": String(\"Bärlin\")}) }, Feature { id: None, …geometry… properties: None }]"
		).await
	}
}
