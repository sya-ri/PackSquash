use std::borrow::Cow;

use futures::{future, StreamExt};
use thiserror::Error;
use tokio::io::AsyncRead;
use tokio_stream::Stream;
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};

use crate::config::CommandsFunctionFileOptions;
use crate::pack_file::asset_type::PackFileAssetType;
use crate::pack_file::util::{LineNumber, MarkLastDecorator};
use crate::pack_file::AsyncReadAndSizeHint;

use super::{OptimizedBytesChunk, PackFile, PackFileConstructor};

#[cfg(test)]
mod tests;

/// Represents a line-oriented text file.
pub struct CommandsFunctionFile<T: AsyncRead + Send + Unpin + 'static> {
	read: T,
	optimization_settings: CommandsFunctionFileOptions
}

/// Represents an error that may happen while optimizing commands function files.
#[derive(Error, Debug)]
pub enum OptimizationError {
	#[error("Error while reading a line: {0}")]
	TextLineRead(#[from] LinesCodecError),
	#[error("Format error: Gratuitous leading slash in command at line {0}. Please remove it")]
	GratuitousLeadingSlash(LineNumber),
	#[error("Format error: Comment delimited by a double slash at line {0}. Please use # instead")]
	DoubleSlashComment(LineNumber)
}

impl<T: AsyncRead + Send + Unpin + 'static> PackFile for CommandsFunctionFile<T> {
	type ByteChunkType = Vec<u8>;
	type OptimizationError = OptimizationError;
	type OptimizedByteChunksStream =
		impl Stream<Item = OptimizedBytesChunk<Self::ByteChunkType, Self::OptimizationError>>;

	fn process(self) -> Self::OptimizedByteChunksStream {
		let mut line_number = LineNumber::new();

		let minify = self.optimization_settings.minify;

		MarkLastDecorator::new(FramedRead::new(
			self.read,
			// Limit line length to 16 MiB to bound memory consumption and be nice to Minecraft.
			// Longer lines are probably an error, and will negatively affect Minecraft performance
			LinesCodec::new_with_max_length(16 * 1024 * 1024)
		))
		.filter_map(move |(line_result, is_last)| {
			let processed_line_result = line_result.map_or_else(
				|err| Some(Err(err.into())),
				|line| process_line(line, is_last, line_number, minify)
			);

			line_number.increment();

			future::ready(processed_line_result)
		})
	}

	fn is_compressed(&self) -> bool {
		false
	}
}

impl<T: AsyncRead + Send + Unpin + 'static> PackFileConstructor<T> for CommandsFunctionFile<T> {
	type OptimizationSettings = CommandsFunctionFileOptions;

	fn new(
		file_read_producer: impl FnOnce() -> Option<AsyncReadAndSizeHint<T>>,
		_: PackFileAssetType,
		optimization_settings: Self::OptimizationSettings
	) -> Option<Self> {
		file_read_producer().map(|(read, _)| Self {
			read,
			optimization_settings
		})
	}
}

/// Processes the specified line of text, returning an optimized bytes chunk with its optimized
/// representation. `None` is returned to signal that the line should not be copied to the output
/// file at all.
fn process_line<L: Into<String>>(
	line: L,
	is_last: bool,
	line_number: LineNumber,
	minify: bool
) -> Option<OptimizedBytesChunk<Vec<u8>, OptimizationError>> {
	const MINIFIED: &str = "Minified";
	const NOT_MINIFIED: &str = "Copied";

	let mut line = line.into();

	// The lines codec takes care of stripping line breaks from the line, even if
	// Windows line endings (CR + LF) are used. However, we should remove the BOM in
	// the first line if present. This fixes problems derived from empty or comment
	// lines being parsed as commands instead, and commands being parsed with a strange
	// character in the beginning, in addition of saving space
	if line_number.is_first() && line.chars().next().map_or(false, |c| c == '\u{feff}') {
		line.remove(0);
	}

	let trimmed_line = line.trim();

	// Check whether the line is empty or a comment. If so, bail out by copying or
	// skipping it, depending on whether we're minifying
	if trimmed_line.is_empty() || trimmed_line.starts_with('#') {
		(!minify).then(|| prepare_for_output(line, is_last, NOT_MINIFIED))
	} else {
		// The line will be parsed as a command.
		// Check that there are no leading slashes, which the game rejects
		if trimmed_line.starts_with("//") {
			// # must be used instead. Most Minecraft versions reject this comment delimiter
			return Some(Err(OptimizationError::DoubleSlashComment(line_number)));
		} else if trimmed_line.starts_with('/') {
			// No leading slash is required by most Minecraft versions
			return Some(Err(OptimizationError::GratuitousLeadingSlash(line_number)));
		}

		if minify {
			Some(prepare_for_output(trimmed_line, is_last, MINIFIED))
		} else {
			Some(prepare_for_output(line, is_last, NOT_MINIFIED))
		}
	}
}

/// Prepares a line for output to the processed representation of this file, adding a line break
/// if necessary.
fn prepare_for_output<L: Into<String>, D: Into<Cow<'static, str>>>(
	line: L,
	is_last: bool,
	description: D
) -> OptimizedBytesChunk<Vec<u8>, OptimizationError> {
	let mut line = line.into();

	// Add a Unix-style line break if there are more lines. We don't need a newline at the end
	if !is_last {
		line.push('\n');
	}

	Ok((description.into(), line.into_bytes()))
}
