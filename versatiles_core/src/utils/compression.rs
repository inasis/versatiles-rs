#![allow(dead_code)]

use crate::types::{
	Blob,
	TileCompression::{self, *},
};
use anyhow::{bail, Result};
use brotli::{enc::BrotliEncoderParams, BrotliCompress, BrotliDecompress};
use enumset::EnumSet;
use flate2::bufread::{GzDecoder, GzEncoder};
use std::io::{Cursor, Read};

#[derive(Debug, PartialEq)]
pub struct TargetCompression {
	compressions: EnumSet<TileCompression>,
	best_compression: bool,
}

impl TargetCompression {
	pub fn from_set(compressions: EnumSet<TileCompression>) -> Self {
		TargetCompression {
			compressions,
			best_compression: true,
		}
	}
	pub fn from(compression: TileCompression) -> Self {
		Self::from_set(EnumSet::only(compression))
	}
	pub fn from_none() -> Self {
		Self::from(Uncompressed)
	}
	pub fn set_best_compression(&mut self, best_compression: bool) {
		self.best_compression = best_compression;
	}
	pub fn contains(&self, compression: TileCompression) -> bool {
		self.compressions.contains(compression)
	}
	pub fn insert(&mut self, compression: TileCompression) {
		self.compressions.insert(compression);
	}
}

pub fn optimize_compression(
	blob: Blob,
	input: &TileCompression,
	target: TargetCompression,
) -> Result<(Blob, TileCompression)> {
	if target.compressions.is_empty() {
		bail!("no compression allowed");
	}

	if !target.best_compression && target.compressions.contains(*input) {
		return Ok((blob, *input));
	}

	match input {
		Uncompressed => {
			if target.compressions.contains(Brotli) {
				return Ok((compress_brotli(&blob)?, Brotli));
			}

			if target.compressions.contains(Gzip) {
				return Ok((compress_gzip(&blob)?, Gzip));
			}

			Ok((blob, Uncompressed))
		}
		Gzip => {
			if target.compressions.contains(Brotli) {
				return Ok((compress_brotli(&decompress_gzip(&blob)?)?, Brotli));
			}

			if target.compressions.contains(Gzip) {
				return Ok((blob, Gzip));
			}

			Ok((decompress_gzip(&blob)?, Uncompressed))
		}
		Brotli => {
			if target.compressions.contains(Brotli) {
				return Ok((blob, Brotli));
			}
			let data = decompress_brotli(&blob)?;

			if target.compressions.contains(Gzip) {
				return Ok((compress_gzip(&data)?, Gzip));
			}

			Ok((data, Uncompressed))
		}
	}
}

pub fn recompress(
	mut blob: Blob,
	input_compression: &TileCompression,
	output_compression: &TileCompression,
) -> Result<Blob> {
	if input_compression == output_compression {
		return Ok(blob);
	}
	blob = decompress(blob, input_compression)?;
	blob = compress(blob, output_compression)?;
	Ok(blob)
}

/// Compresses data based on the given compression algorithm
///
/// # Arguments
///
/// * `data` - The blob of data to compress
/// * `compression` - The compression algorithm to use
pub fn compress(blob: Blob, compression: &TileCompression) -> Result<Blob> {
	match compression {
		Uncompressed => Ok(blob),
		Gzip => compress_gzip(&blob),
		Brotli => compress_brotli(&blob),
	}
}

/// Decompresses data based on the given compression algorithm
///
/// # Arguments
///
/// * `data` - The blob of data to decompress
/// * `compression` - The compression algorithm used for compression
pub fn decompress(blob: Blob, compression: &TileCompression) -> Result<Blob> {
	match compression {
		Uncompressed => Ok(blob),
		Gzip => decompress_gzip(&blob),
		Brotli => decompress_brotli(&blob),
	}
}

/// Compresses data using gzip
///
/// # Arguments
///
/// * `data` - The blob of data to compress
pub fn compress_gzip(blob: &Blob) -> Result<Blob> {
	let mut result: Vec<u8> = Vec::new();
	GzEncoder::new(blob.as_slice(), flate2::Compression::best()).read_to_end(&mut result)?;
	Ok(Blob::from(result))
}

/// Decompresses data that was compressed using gzip
///
/// # Arguments
///
/// * `data` - The blob of data to decompress
pub fn decompress_gzip(blob: &Blob) -> Result<Blob> {
	let mut result: Vec<u8> = Vec::new();
	GzDecoder::new(blob.as_slice()).read_to_end(&mut result)?;
	Ok(Blob::from(result))
}

/// Compresses data using Brotli
///
/// # Arguments
///
/// * `data` - The blob of data to compress
pub fn compress_brotli(blob: &Blob) -> Result<Blob> {
	let params = BrotliEncoderParams {
		quality: 10,
		lgwin: 19,
		size_hint: blob.len() as usize,
		..Default::default()
	};
	let mut input = Cursor::new(blob.as_slice());
	let mut output: Vec<u8> = Vec::new();
	BrotliCompress(&mut input, &mut output, &params)?;

	Ok(Blob::from(output))
}

/// Compresses data using Brotli, but faster
///
/// # Arguments
///
/// * `data` - The blob of data to compress
pub fn compress_brotli_fast(blob: &Blob) -> Result<Blob> {
	let params = BrotliEncoderParams {
		quality: 3,
		lgwin: 16,
		size_hint: blob.len() as usize,
		..Default::default()
	};
	let mut input = Cursor::new(blob.as_slice());
	let mut output: Vec<u8> = Vec::new();
	BrotliCompress(&mut input, &mut output, &params)?;

	Ok(Blob::from(output))
}

/// Decompresses data that was compressed using Brotli
///
/// # Arguments
///
/// * `data` - The blob of data to decompress
pub fn decompress_brotli(blob: &Blob) -> Result<Blob> {
	let mut cursor = Cursor::new(blob.as_slice());
	let mut result: Vec<u8> = Vec::new();
	BrotliDecompress(&mut cursor, &mut result)?;
	Ok(Blob::from(result))
}

#[cfg(test)]
mod tests {
	use super::*;
	use enumset::enum_set;

	#[test]
	fn verify_brotli() -> Result<()> {
		let data1 = random_data(10000);
		assert_eq!(data1, decompress_brotli(&compress_brotli(&data1)?)?);
		Ok(())
	}

	#[test]
	fn verify_fast_brotli() -> Result<()> {
		let data1 = random_data(10000);
		assert_eq!(data1, decompress_brotli(&compress_brotli_fast(&data1)?)?);
		Ok(())
	}

	#[test]
	/// Verify that the Gzip compression and decompression functions work correctly.
	fn verify_gzip() -> Result<()> {
		let data1 = random_data(100000);
		assert_eq!(data1, decompress_gzip(&compress_gzip(&data1)?)?);
		Ok(())
	}

	#[test]
	/// Test optimize_compression and try all combinations
	fn test_optimize_compression() -> Result<()> {
		let blob = random_data(100);
		let blob_gzip = compress_gzip(&blob)?;
		let blob_brotli = compress_brotli(&blob)?;

		let test = |compression_in: TileCompression,
		            compressions_out: EnumSet<TileCompression>,
		            best_compression: bool,
		            compression_exp: TileCompression|
		 -> Result<()> {
			let target = TargetCompression {
				compressions: compressions_out,
				best_compression,
			};
			let data_in = match compression_in {
				Uncompressed => blob.clone(),
				Gzip => blob_gzip.clone(),
				Brotli => blob_brotli.clone(),
			};
			let data_exp = match compression_exp {
				Uncompressed => blob.clone(),
				Gzip => blob_gzip.clone(),
				Brotli => blob_brotli.clone(),
			};
			let (data_res, compression_res) = optimize_compression(data_in, &compression_in, target)?;
			assert_eq!(
				compression_res, compression_exp,
				"compressing from {compression_in:?} to {compressions_out:?} using best compression ({best_compression}) should result {compression_exp:?} and not {compression_res:?}"
			);

			assert_eq!(data_res, data_exp);
			Ok(())
		};

		let cn = Uncompressed;
		let cg = Gzip;
		let cb = Brotli;

		let sn = enum_set!(Uncompressed);
		let sg = enum_set!(Gzip);
		let sb = enum_set!(Brotli);
		let sng = enum_set!(Uncompressed | Gzip);
		let snb = enum_set!(Uncompressed | Brotli);
		let sngb = enum_set!(Uncompressed | Gzip | Brotli);

		test(cn, sn, true, cn)?;
		test(cn, sg, true, cg)?;
		test(cn, sb, true, cb)?;
		test(cn, sng, true, cg)?;
		test(cn, snb, true, cb)?;
		test(cn, sngb, true, cb)?;

		test(cg, sn, true, cn)?;
		test(cg, sg, true, cg)?;
		test(cg, sb, true, cb)?;
		test(cg, sng, true, cg)?;
		test(cg, snb, true, cb)?;
		test(cg, sngb, true, cb)?;

		test(cb, sn, true, cn)?;
		test(cb, sg, true, cg)?;
		test(cb, sb, true, cb)?;
		test(cb, sng, true, cg)?;
		test(cb, snb, true, cb)?;
		test(cb, sngb, true, cb)?;

		test(cn, sn, false, cn)?;
		test(cn, sg, false, cg)?;
		test(cn, sb, false, cb)?;
		test(cn, sng, false, cn)?;
		test(cn, snb, false, cn)?;
		test(cn, sngb, false, cn)?;

		test(cg, sn, false, cn)?;
		test(cg, sg, false, cg)?;
		test(cg, sb, false, cb)?;
		test(cg, sng, false, cg)?;
		test(cg, snb, false, cb)?;
		test(cg, sngb, false, cg)?;

		test(cb, sn, false, cn)?;
		test(cb, sg, false, cg)?;
		test(cb, sb, false, cb)?;
		test(cb, sng, false, cg)?;
		test(cb, snb, false, cb)?;
		test(cb, sngb, false, cb)?;

		Ok(())
	}

	/// Generate random binary data of a specified size.
	fn random_data(size: usize) -> Blob {
		let mut vec: Vec<u8> = vec![0; size];
		(0..size).for_each(|i| {
			vec[i] = (((i as f64 + 1.78123).cos() * 6_513_814_013_423.454).fract() * 256f64) as u8;
		});

		Blob::from(vec)
	}
}
