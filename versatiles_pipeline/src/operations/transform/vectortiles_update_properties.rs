use crate::{
	geometry::{vector_tile::VectorTile, GeoProperties},
	helpers::read_csv_file,
	traits::{OperationFactoryTrait, OperationTrait, TransformOperationFactoryTrait},
	types::{
		Blob, TileBBox, TileCompression, TileCoord3, TileFormat, TileStream, TilesReaderParameters,
	},
	utils::decompress,
	vpl::VPLNode,
	PipelineFactory,
};
use anyhow::{anyhow, ensure, Context, Result};
use async_trait::async_trait;
use futures::future::BoxFuture;
use log::warn;
use std::{collections::HashMap, sync::Arc};

#[derive(versatiles_derive::VPLDecode, Clone, Debug)]
/// Updates properties of vector tile features using data from an external source (e.g., CSV file). Matches features based on an ID field.
struct Args {
	/// Path to the data source file, e.g., `data_source_path="data.csv"`.
	data_source_path: String,
	/// ID field name in the vector tiles.
	id_field_tiles: String,
	/// ID field name in the data source.
	id_field_data: String,
	/// Name of the layer to update. If unspecified, all layers will be updated.
	layer_name: Option<String>,
	/// If set, old properties will be deleted before new ones are added.
	replace_properties: bool,
	/// If set, removes all features (in the layer) that do not match.
	remove_non_matching: bool,
	/// If set, includes the ID field in the updated properties.
	include_id: bool,
}

#[derive(Debug)]
struct Runner {
	args: Args,
	tile_compression: TileCompression,
	properties_map: HashMap<String, GeoProperties>,
}

impl Runner {
	fn run(&self, mut blob: Blob) -> Result<Option<Blob>> {
		blob = decompress(blob, &self.tile_compression)?;
		let mut tile =
			VectorTile::from_blob(&blob).context("Failed to create VectorTile from Blob")?;

		let layer_name = self.args.layer_name.as_ref();

		for layer in tile.layers.iter_mut() {
			if layer_name.map_or(false, |layer_name| &layer.name != layer_name) {
				continue;
			}

			layer.filter_map_properties(|mut prop| {
				if let Some(id) = prop.get(&self.args.id_field_tiles) {
					if let Some(new_prop) = self.properties_map.get(&id.to_string()) {
						if self.args.replace_properties {
							prop = new_prop.clone();
						} else {
							prop.update(new_prop);
						}
					} else {
						if self.args.remove_non_matching {
							return None;
						}
						warn!("id \"{id}\" not found in data source");
					}
				} else {
					warn!("id field \"{}\" not found", &self.args.id_field_tiles);
				}
				Some(prop)
			})?;
		}

		Ok(Some(
			tile
				.to_blob()
				.context("Failed to convert VectorTile to Blob")?,
		))
	}
}

#[derive(Debug)]
struct Operation {
	runner: Arc<Runner>,
	parameters: TilesReaderParameters,
	source: Box<dyn OperationTrait>,
	meta: Option<Blob>,
}

impl Operation {
	fn build(
		vpl_node: VPLNode,
		source: Box<dyn OperationTrait>,
		factory: &PipelineFactory,
	) -> BoxFuture<'_, Result<Box<dyn OperationTrait>, anyhow::Error>>
	where
		Self: Sized + OperationTrait,
	{
		Box::pin(async move {
			let args = Args::from_vpl_node(&vpl_node)?;
			let data = read_csv_file(&factory.resolve_path(&args.data_source_path))
				.await
				.with_context(|| format!("Failed to read CSV file from '{}'", args.data_source_path))?;

			let properties_map = data
				.into_iter()
				.map(|mut properties| {
					let key = properties
						.get(&args.id_field_data)
						.ok_or_else(|| anyhow!("Key '{}' not found in CSV data", args.id_field_data))
						.with_context(|| {
							format!(
								"Failed to find key '{}' in the CSV data row: {properties:?}",
								args.id_field_data
							)
						})?
						.to_string();
					if !args.include_id {
						properties.remove(&args.id_field_data)
					}
					Ok((key, properties))
				})
				.collect::<Result<HashMap<String, GeoProperties>>>()
				.context("Failed to build properties map from CSV data")?;

			let mut parameters = source.get_parameters().clone();
			ensure!(
				parameters.tile_format == TileFormat::PBF,
				"source must be vector tiles"
			);

			let meta = source.get_meta();

			let runner = Arc::new(Runner {
				args,
				properties_map,
				tile_compression: parameters.tile_compression,
			});

			parameters.tile_compression = TileCompression::Uncompressed;

			Ok(Box::new(Self {
				runner,
				meta,
				parameters,
				source,
			}) as Box<dyn OperationTrait>)
		})
	}
}

#[async_trait]
impl OperationTrait for Operation {
	fn get_parameters(&self) -> &TilesReaderParameters {
		&self.parameters
	}
	async fn get_bbox_tile_stream(&self, bbox: TileBBox) -> TileStream {
		let runner = self.runner.clone();
		self
			.source
			.get_bbox_tile_stream(bbox)
			.await
			.filter_map_blob_parallel(move |blob| runner.run(blob).unwrap())
	}
	fn get_meta(&self) -> Option<Blob> {
		self.meta.clone()
	}
	async fn get_tile_data(&self, coord: &TileCoord3) -> Result<Option<Blob>> {
		Ok(
			if let Some(blob) = self.source.get_tile_data(coord).await? {
				self.runner.run(blob)?
			} else {
				None
			},
		)
	}
}

pub struct Factory {}

impl OperationFactoryTrait for Factory {
	fn get_docs(&self) -> String {
		Args::get_docs()
	}
	fn get_tag_name(&self) -> &str {
		"vectortiles_update_properties"
	}
}

#[async_trait]
impl TransformOperationFactoryTrait for Factory {
	async fn build<'a>(
		&self,
		vpl_node: VPLNode,
		source: Box<dyn OperationTrait>,
		factory: &'a PipelineFactory,
	) -> Result<Box<dyn OperationTrait>> {
		Operation::build(vpl_node, source, factory).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use assert_fs::NamedTempFile;
	use std::{fs::File, io::Write};
	use versatiles_geometry::{
		vector_tile::VectorTileLayer, GeoFeature, GeoProperties, GeoValue, Geometry,
	};

	fn create_sample_vector_tile_blob() -> Blob {
		let mut feature = GeoFeature::new(Geometry::new_example());
		feature.properties = GeoProperties::from(vec![
			("id", GeoValue::from("feature_1")),
			("property1", GeoValue::from("value1")),
		]);
		let layer =
			VectorTileLayer::from_features(String::from("test_layer"), vec![feature], 4096, 1)
				.unwrap();
		let tile = VectorTile::new(vec![layer]);
		tile.to_blob().unwrap()
	}

	#[tokio::test]
	async fn test_runner_run() {
		let properties_map = HashMap::from([(
			"feature_1".to_string(),
			GeoProperties::from(vec![("property2", GeoValue::from("new_value"))]),
		)]);

		let runner = Runner {
			args: Args {
				data_source_path: "data.csv".to_string(),
				id_field_tiles: "id".to_string(),
				id_field_data: "id".to_string(),
				layer_name: None,
				replace_properties: false,
				remove_non_matching: false,
				include_id: false,
			},
			tile_compression: TileCompression::Uncompressed,
			properties_map,
		};

		let blob = create_sample_vector_tile_blob();
		let result_blob = runner.run(blob).unwrap().unwrap();
		let tile = VectorTile::from_blob(&result_blob).unwrap();

		let properties = tile.layers[0].features[0]
			.decode_properties(&tile.layers[0])
			.unwrap();

		assert_eq!(
			properties.get("property2").unwrap(),
			&GeoValue::from("new_value")
		);
	}

	#[test]
	fn test_args_from_vpl_node() {
		let vpl_node = VPLNode::from_str(
			r##"vectortiles_update_properties data_source_path="data.csv" id_field_tiles="id" id_field_data="id" replace_properties="true" include_id="true""##,
		)
		.unwrap();

		let args = Args::from_vpl_node(&vpl_node).unwrap();
		assert_eq!(args.data_source_path, "data.csv");
		assert_eq!(args.id_field_tiles, "id");
		assert_eq!(args.id_field_data, "id");
		assert!(args.replace_properties);
		assert!(args.include_id);
	}

	async fn run(input: &str) -> Result<String> {
		let temp_file = NamedTempFile::new("test.csv")?;
		let mut file = File::create(&temp_file)?;
		writeln!(&mut file, "data_id,value\n0,test")?;

		let parts = input.split(',').collect::<Vec<_>>();
		let replace = |value: &str, key: &str| {
			if value.is_empty() {
				String::from("")
			} else {
				format!("{key}={value}")
			}
		};

		let factory = PipelineFactory::new_dummy();
		let operation = factory
			.operation_from_vpl(
				&vec![
					"from_container filename=dummy |",
					"vectortiles_update_properties",
					&format!("data_source_path=\"{}\"", temp_file.to_str().unwrap()),
					&replace(parts[0], "id_field_tiles"),
					&replace(parts[1], "id_field_data"),
					&replace(parts[2], "layername"),
					&replace(parts[3], "replace_properties"),
					&replace(parts[4], "include_id"),
				]
				.join(" "),
			)
			.await?;

		let blob = operation
			.get_tile_data(&TileCoord3::new(0, 0, 0)?)
			.await?
			.unwrap();
		let tile = VectorTile::from_blob(&blob)?;

		assert_eq!(tile.layers.len(), 1);
		assert_eq!(tile.layers[0].features.len(), 1);
		let properties = tile.layers[0].features[0].decode_properties(&tile.layers[0])?;
		Ok(format!("{properties:?}"))
	}

	#[tokio::test]
	async fn test_run_variation1() -> Result<()> {
		assert_eq!(
			run("x,data_id,,false,false").await?, 
			"{\"filename\": String(\"dummy\"), \"value\": String(\"test\"), \"x\": UInt(0), \"y\": UInt(0), \"z\": UInt(0)}"
		);
		Ok(())
	}

	#[tokio::test]
	async fn test_run_variation2() -> Result<()> {
		assert_eq!(
			run("x,data_id,,false,true").await?, 
			"{\"data_id\": UInt(0), \"filename\": String(\"dummy\"), \"value\": String(\"test\"), \"x\": UInt(0), \"y\": UInt(0), \"z\": UInt(0)}"
		);
		Ok(())
	}

	#[tokio::test]
	async fn test_run_variation3() -> Result<()> {
		assert_eq!(
			run("x,data_id,,true,false").await?,
			"{\"value\": String(\"test\")}"
		);
		Ok(())
	}

	#[tokio::test]
	async fn test_run_variation4() -> Result<()> {
		assert_eq!(
			run("x,data_id,,true,true").await?,
			"{\"data_id\": UInt(0), \"value\": String(\"test\")}"
		);
		Ok(())
	}
}
