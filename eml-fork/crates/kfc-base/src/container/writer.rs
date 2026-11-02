use std::{borrow::Borrow, collections::HashMap, fs::File, io::{BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write}, path::{Path, PathBuf}};

use crate::{Hash32, container::header::ResourceChunkInfo, guid::{ContentHash, ResourceId}, io::{WriteExt, WriteSeekExt}, reflection::TypeRegistry};

use super::{header::{ContentEntry, ContainerInfo, ResourceEntry}, KFCFile, KFCReadError, KFCWriteError, StaticMapBuilder};

const RESOURCE_ALIGNMENT: u64 = 16;
const CONTENT_ALIGNMENT: u64 = 4096;
const RESOURCE_CHUNK_ALIGNMENT: u64 = 4096;
const RESOURCE_CHUNK_SIZE: u64 = 8 * 1024 * 1024; // 8 MiB

pub struct KFCWriter<F, T> {
    type_registry: T,

    path: PathBuf,
    file_name: String,

    file: File,

    chunk_writer: Cursor<Vec<u8>>,
    resource_writer: BufWriter<File>,
    uncompressed_offset: u64,
    uncompressed_chunk_offset: u64,
    resource_chunks: Vec<ResourceChunkInfo>,

    container_writers: Vec<ContainerWriter>,

    game_version: String,
    resources: StaticMapBuilder<ResourceId, ResourceEntry>,
    contents: StaticMapBuilder<ContentHash, ContentEntry>,

    incremental_data: Option<IncrementalData<F>>,
    inplace_data: Option<InPlaceData>,
    options: KFCWriteOptions,
}

struct IncrementalData<F> {
    reference_file: F,
}

/// The zstd level used for chunks that have no reference counterpart
/// (appended data) and as a fallback for changed chunks whose reference
/// level could not be detected. 9 is the game's main-pass level: on the
/// build-1273043 container it reproduces 61 of 66 reference frames (the
/// remaining five are tail chunks at level 3, and three empty chunks
/// match several levels including 9).
const INPLACE_FALLBACK_LEVEL: i32 = 9;

/// Candidate levels for in-place level detection, in the order they are
/// tried. The levels observed in game files (9, 3, 1) come first.
const INPLACE_LEVEL_CANDIDATES: [i32; 19] = [
    9, 3, 1, 2, 4, 5, 6, 7, 8, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
];

/// Everything the in-place writer mode snapshots from the reference
/// file at construction time. The output stream file is the reference
/// stream file itself, rewritten from the start at finalize, so no
/// reference data may be read lazily after construction.
struct InPlaceData {
    /// Decompressed working copy of the reference stream, with all
    /// edits spliced in and appended data at the end. Its length is
    /// always `append_offset`.
    stream: Vec<u8>,
    /// The reference file's chunk table; output chunks preserve this
    /// uncompressed geometry.
    reference_chunks: Vec<ResourceChunkInfo>,
    /// The detected zstd level per reference chunk, or `None` where no
    /// candidate level reproduced the chunk's original frame.
    chunk_levels: Vec<Option<i32>>,
    /// The original compressed frame of every chunk whose level could
    /// not be detected, kept so an unchanged chunk can still be written
    /// out byte-identically. `None` where a level was detected.
    fallback_frames: Vec<Option<Vec<u8>>>,
    /// Per reference chunk: whether any edit touched its byte range.
    dirty_chunks: Vec<bool>,
    /// The current allocation of every resource this writer knows:
    /// `offset` is where its data lives in `stream`, `size` is the span
    /// a rewrite may occupy at that offset. Reference resources start
    /// with their original span (stored sizes recovered from the 24-bit
    /// truncation, mirroring `KFCReader::resource_size`); appended
    /// resources with the span they were appended with.
    allocations: HashMap<ResourceId, ResourceEntry>,
    /// The offset at which the next appended resource is placed:
    /// the end of the reference stream, then the end of the appended
    /// region. Always 16-aligned.
    append_offset: u64,
}

impl InPlaceData {

    fn load(reference: &KFCFile, resources_path: &Path) -> Result<Self, KFCReadError> {
        let reference_chunks = reference.resource_chunks().to_vec();

        let uncompressed_size = reference_chunks.iter()
            .map(|chunk| chunk.uncompressed_offset + chunk.uncompressed_size)
            .max()
            .unwrap_or(0);

        // read and decompress the whole reference stream, detecting each
        // chunk's zstd level by re-encoding its data and comparing the
        // result against the chunk's original frame

        let mut stream = vec![0u8; uncompressed_size as usize];
        let mut chunk_levels = Vec::with_capacity(reference_chunks.len());
        let mut fallback_frames = Vec::with_capacity(reference_chunks.len());

        let mut reader = BufReader::new(File::open(resources_path)?);
        let mut frame = Vec::new();

        for chunk in &reference_chunks {
            frame.clear();
            frame.resize(chunk.compressed_size as usize, 0);

            reader.seek(SeekFrom::Start(chunk.offset))?;
            reader.read_exact(&mut frame[..chunk.compressed_size as usize])?;

            let mut data = Vec::with_capacity(chunk.uncompressed_size as usize);
            zstd::stream::copy_decode(&mut &frame[..], &mut data)?;

            if data.len() as u64 != chunk.uncompressed_size {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "reference chunk at offset {} decompressed to {} bytes, expected {}",
                        chunk.offset,
                        data.len(),
                        chunk.uncompressed_size
                    ),
                ).into());
            }

            let start = chunk.uncompressed_offset as usize;
            stream[start..start + data.len()].copy_from_slice(&data);

            let mut detected = None;

            for level in INPLACE_LEVEL_CANDIDATES {
                if encode_chunk(&data, level)? == frame[..] {
                    detected = Some(level);
                    break;
                }
            }

            fallback_frames.push(if detected.is_none() {
                Some(frame.clone())
            } else {
                None
            });
            chunk_levels.push(detected);
        }

        // recover the original span of every reference resource (the
        // stored sizes of resources >= 16 MiB are truncated to 24 bits)

        let bounds = resource_bounds(reference);

        let mut allocations = HashMap::with_capacity(reference.resources().len());

        for (guid, entry) in reference.resources().iter() {
            allocations.insert(*guid, ResourceEntry {
                offset: entry.offset,
                size: actual_resource_size(&bounds, entry),
            });
        }

        let chunk_count = reference_chunks.len();

        Ok(Self {
            stream,
            reference_chunks,
            chunk_levels,
            fallback_frames,
            dirty_chunks: vec![false; chunk_count],
            allocations,
            append_offset: uncompressed_size,
        })
    }

    fn mark_dirty(&mut self, start: u64, end: u64) {
        for (index, chunk) in self.reference_chunks.iter().enumerate() {
            let chunk_start = chunk.uncompressed_offset;
            let chunk_end = chunk_start + chunk.uncompressed_size;

            if chunk_start < end && start < chunk_end {
                self.dirty_chunks[index] = true;
            }
        }
    }

}

/// Compresses one chunk the same way the writer's chunk submission
/// does (`zstd::stream::copy_encode`), at an explicit level.
fn encode_chunk(data: &[u8], level: i32) -> std::io::Result<Vec<u8>> {
    let mut encoded = Vec::new();
    zstd::stream::copy_encode(&mut &data[..], &mut encoded, level)?;
    Ok(encoded)
}

/// Sorted, distinct resource offsets followed by the end of the
/// resource data. Mirrors `KFCReader`'s bounds computation.
fn resource_bounds(file: &KFCFile) -> Vec<u64> {
    let mut bounds = file.resources()
        .values()
        .iter()
        .map(|entry| entry.offset)
        .collect::<Vec<_>>();

    let data_end = file.resource_chunks()
        .iter()
        .map(|chunk| chunk.uncompressed_offset + chunk.uncompressed_size)
        .max()
        .unwrap_or(0);

    bounds.sort_unstable();
    bounds.dedup();
    bounds.push(data_end);
    bounds
}

/// The actual size of a resource whose stored size may have been
/// truncated to 24 bits. Mirrors the reader's recovery rule
/// (`actual_resource_size` in `reader.rs`): the space up to the next
/// resource must match the stored size plus a multiple of 16 MiB and
/// less than the resource alignment, otherwise the stored size is used
/// as is.
fn actual_resource_size(bounds: &[u64], entry: &ResourceEntry) -> u64 {
    const RESOURCE_SIZE_MODULUS: u64 = 0x1000000;

    let next = match bounds.binary_search(&entry.offset) {
        Ok(index) => bounds.get(index + 1),
        Err(index) => bounds.get(index),
    };

    let Some(&next) = next else {
        return entry.size;
    };

    let Some(gap) = next.checked_sub(entry.offset + entry.size) else {
        return entry.size;
    };

    let missing = gap - gap % RESOURCE_SIZE_MODULUS;

    if missing > 0 && gap - missing < RESOURCE_ALIGNMENT {
        entry.size + missing
    } else {
        entry.size
    }
}

#[derive(Debug)]
pub struct KFCWriteOptions {
    /// The extension to use for kfc files.
    pub kfc_extension: String,
    /// The extension to use for kfc resource files.
    pub resource_extension: String,
    /// The extension to use for dat files.
    pub dat_extension: String,
    /// If true, the writer will overwrite existing data in the .dat files,
    /// instead of causing an error.
    /// **NOTE:** For incremental writes, this will only affect non-default .dat files.
    pub overwrite_containers: bool,
    // TEST: does the game use a i32, u32 or an artifical limit for the dat max size?
    /// The maximum size of a single .dat file before a new one is created.
    pub max_container_size: u32,
    /// Whether to truncate existing .dat files or not.
    /// This is used to remove any leftover data from previous writes.
    /// **NOTE:** For incremental writes, this will only affect non-default .dat files.
    pub truncate_containers: bool,
    /// If true, an incremental writer replaces resource data **in place**
    /// within the reference stream whenever the new data fits the
    /// resource's original allocation, instead of appending every written
    /// resource after the reference stream. A resource whose new data
    /// does not fit its allocation, and every brand-new resource, is
    /// appended after the reference stream's end. Chunk geometry and
    /// uncompressed offsets of everything else stay untouched.
    /// **NOTE:** Only affects incremental writes; ignored otherwise.
    pub in_place: bool,
}

impl Default for KFCWriteOptions {
    fn default() -> Self {
        Self {
            kfc_extension: "kfc".to_string(),
            resource_extension: "kfc_resources".to_string(),
            dat_extension: "dat".to_string(),
            overwrite_containers: false,
            max_container_size: 1024 * 1024 * 1024, // 1 GiB
            truncate_containers: false,
            in_place: false,
        }
    }
}

impl<F, T> KFCWriter<F, T>
where
    F: Borrow<KFCFile>,
    T: Borrow<TypeRegistry>,
{

    #[inline]
    pub fn new(
        path: impl AsRef<Path>,
        file_name: impl AsRef<str>,
        type_registry: T,
        game_version: impl AsRef<str>,
    ) -> Result<Self, KFCReadError> {
        Self::new_with_options(
            path,
            file_name,
            type_registry,
            game_version,
            KFCWriteOptions::default(),
        )
    }

    pub fn new_with_options(
        path: impl AsRef<Path>,
        file_name: impl AsRef<str>,
        type_registry: T,
        game_version: impl AsRef<str>,
        options: KFCWriteOptions,
    ) -> Result<Self, KFCReadError> {
        let kfc_path = path.as_ref().join(format!(
            "{}.{}",
            file_name.as_ref(),
            options.kfc_extension
        ));
        let file = File::options().write(true).read(true).open(&kfc_path)?;

        let resources_path = path.as_ref().join(format!(
            "{}.{}",
            file_name.as_ref(),
            options.resource_extension
        ));
        let resource_file = File::options().write(true).open(&resources_path)?;
        let resource_writer = BufWriter::new(resource_file);

        Ok(Self {
            type_registry,

            path: path.as_ref().to_path_buf(),
            file_name: file_name.as_ref().to_string(),

            file,

            chunk_writer: Cursor::new(Vec::new()),
            resource_writer,
            uncompressed_offset: 0,
            uncompressed_chunk_offset: 0,
            resource_chunks: Vec::new(),

            container_writers: Vec::new(),

            game_version: game_version.as_ref().to_string(),
            resources: StaticMapBuilder::default(),
            contents: StaticMapBuilder::default(),

            incremental_data: None,
            inplace_data: None,
            options,
        })
    }

    #[inline]
    pub fn new_incremental(
        path: impl AsRef<Path>,
        file_name: impl AsRef<str>,
        reference_file: F,
        type_registry: T,
    ) -> Result<Self, KFCReadError> {
        Self::new_incremental_with_options(
            path,
            file_name,
            reference_file,
            type_registry,
            KFCWriteOptions::default(),
        )
    }

    pub fn new_incremental_with_options(
        path: impl AsRef<Path>,
        file_name: impl AsRef<str>,
        reference_file: F,
        type_registry: T,
        options: KFCWriteOptions,
    ) -> Result<Self, KFCReadError> {
        let kfc_path = path.as_ref().join(format!(
            "{}.{}",
            file_name.as_ref(),
            options.kfc_extension
        ));
        let current_file = KFCFile::from_path(&kfc_path, true)?;

        let file = reference_file.borrow();

        let resources = file.resources().as_builder();
        let contents = file.contents().as_builder();

        drop(current_file);

        let file = File::options().write(true).read(true).open(&kfc_path)?;

        let resources_path = path.as_ref().join(format!(
            "{}.{}",
            file_name.as_ref(),
            options.resource_extension
        ));
        let resource_file = File::options().write(true).open(&resources_path)?;
        let mut resource_writer = BufWriter::new(resource_file);

        let previous_resource_size = reference_file.borrow()
            .resource_chunks()
            .iter()
            .map(|chunk| chunk.offset + chunk.size)
            .max()
            .unwrap_or(0);

        let previous_uncompressed_size = reference_file.borrow()
            .resource_chunks()
            .iter()
            .map(|chunk| chunk.uncompressed_offset + chunk.uncompressed_size)
            .max()
            .unwrap_or(0);

        resource_writer.seek(SeekFrom::Start(previous_resource_size))?;

        // In-place mode snapshots everything it needs from the reference
        // stream now: at finalize time the stream file (which is also
        // the output file) is rewritten from the start, so nothing may
        // be read from it lazily later.
        let inplace_data = if options.in_place {
            Some(InPlaceData::load(reference_file.borrow(), &resources_path)?)
        } else {
            None
        };

        Ok(Self {
            type_registry,

            path: path.as_ref().into(),
            file_name: file_name.as_ref().to_string(),

            file,
            chunk_writer: Cursor::new(Vec::new()),
            resource_writer,
            uncompressed_offset: previous_uncompressed_size,
            uncompressed_chunk_offset: previous_uncompressed_size,
            resource_chunks: Vec::new(),

            container_writers: Vec::new(),

            game_version: reference_file.borrow().game_version().to_string(),
            resources,
            contents,

            incremental_data: Some(IncrementalData {
                reference_file,
            }),
            inplace_data,
            options,
        })
    }

    #[inline]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[inline]
    pub fn type_registry(&self) -> &TypeRegistry {
        self.type_registry.borrow()
    }

    #[inline]
    pub fn reference_file(&self) -> Option<&KFCFile> {
        self.incremental_data.as_ref()
            .map(|data| data.reference_file.borrow())
    }

    pub fn write_resource(
        &mut self,
        guid: &ResourceId,
        mut bytes: &[u8]
    ) -> std::io::Result<()> {
        if self.inplace_data.is_some() {
            return self.write_resource_inplace(guid, bytes);
        }

        self.resources.insert(*guid, ResourceEntry {
            offset: self.uncompressed_offset,
            size: bytes.len() as u64
        });

        let previous_offset = self.uncompressed_offset;

        self.uncompressed_offset += bytes.len() as u64;
        self.uncompressed_offset += (RESOURCE_ALIGNMENT - (self.uncompressed_offset % RESOURCE_ALIGNMENT)) % RESOURCE_ALIGNMENT;

        let mut aligned_size = self.uncompressed_offset - previous_offset;

        // write resource data or compress it if needed

        while aligned_size + self.chunk_writer.position() >= RESOURCE_CHUNK_SIZE {
            let available_size = RESOURCE_CHUNK_SIZE - self.chunk_writer.position();
            let split_index = (available_size as usize).min(bytes.len());

            let (chunk_bytes, remaining_bytes) = bytes.split_at(split_index);

            self.chunk_writer.write_all(chunk_bytes)?;
            self.chunk_writer.align(RESOURCE_CHUNK_SIZE as usize)?;

            // compress and submit chunk

            self.submit_resource_data()?;

            // continue with remaining bytes

            bytes = remaining_bytes;
            aligned_size -= available_size;
        }

        if aligned_size > 0 {
            self.chunk_writer.write_all(bytes)?;

            let remaining = aligned_size - bytes.len() as u64;

            if remaining > 0 {
                self.chunk_writer.padding(remaining)?;
            }
        }

        Ok(())
    }

    fn submit_resource_data(&mut self) -> std::io::Result<()> {
        let offset = self.resource_writer.stream_position()?;
        let chunk_data = self.chunk_writer.get_ref();

        zstd::stream::copy_encode(
            &mut &chunk_data[..],
            &mut self.resource_writer,
            0
        )?;

        let compressed_size = self.resource_writer.stream_position()? - offset;

        self.resource_writer.align(RESOURCE_CHUNK_ALIGNMENT as usize)?;

        let size = self.resource_writer.stream_position()? - offset;
        let uncompressed_offset = self.uncompressed_chunk_offset;
        let uncompressed_size = chunk_data.len() as u64;

        self.resource_chunks.push(ResourceChunkInfo {
            offset,
            size,
            compressed_size,
            uncompressed_offset,
            uncompressed_size,
        });

        self.uncompressed_chunk_offset += uncompressed_size;

        // reset chunk writer

        self.chunk_writer.set_position(0);
        self.chunk_writer.get_mut().clear();

        Ok(())
    }

    /// In-place variant of [`KFCWriter::write_resource`]: splices the new
    /// data into the working copy of the reference stream at the
    /// resource's original offset when it fits the resource's allocation
    /// (the remainder of the allocation is zeroed, matching the format's
    /// zero-gap convention); otherwise — and for brand-new resources —
    /// appends the data after the reference stream's end. The directory
    /// entry is updated either way; the stream itself is only written
    /// out at `finalize`.
    fn write_resource_inplace(
        &mut self,
        guid: &ResourceId,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        let data = self.inplace_data.as_mut()
            .expect("write_resource_inplace called without in-place data");
        let new_size = bytes.len() as u64;

        match data.allocations.get(guid).cloned() {
            Some(allocation) if new_size <= allocation.size => {
                let start = allocation.offset as usize;
                let end = (allocation.offset + allocation.size) as usize;

                data.stream[start..start + bytes.len()].copy_from_slice(bytes);
                data.stream[start + bytes.len()..end].fill(0);
                data.mark_dirty(allocation.offset, allocation.offset + allocation.size);

                self.resources.insert(*guid, ResourceEntry {
                    offset: allocation.offset,
                    size: new_size,
                });
            }
            _ => {
                let offset = data.append_offset;
                let aligned_size = new_size
                    + (RESOURCE_ALIGNMENT - (new_size % RESOURCE_ALIGNMENT)) % RESOURCE_ALIGNMENT;

                debug_assert_eq!(data.stream.len() as u64, offset);
                data.stream.extend_from_slice(bytes);
                data.stream.resize((offset + aligned_size) as usize, 0);
                data.append_offset = offset + aligned_size;

                // The appended span (data + its alignment padding)
                // becomes the resource's allocation for any later
                // rewrite within this session. Its previous span, if
                // any, stays behind as unreferenced data — the same
                // orphan pattern the game's own files exhibit.
                data.allocations.insert(*guid, ResourceEntry {
                    offset,
                    size: aligned_size,
                });

                self.resources.insert(*guid, ResourceEntry {
                    offset,
                    size: new_size,
                });
            }
        }

        Ok(())
    }

    /// Writes the in-place stream built from the reference's decompressed
    /// data plus all edits, and returns the new chunk table.
    ///
    /// Reference chunks keep their uncompressed geometry and are encoded
    /// at their own detected zstd level, so a chunk whose content did not
    /// change reproduces its original frame byte for byte. When a changed
    /// chunk's new frame still fits the chunk's original slot, the slot
    /// size is kept as well and no later chunk moves; otherwise the slot
    /// grows to the next 4096 alignment, as in the append path. Appended
    /// data (relocated and brand-new resources) is chunked at
    /// `RESOURCE_CHUNK_SIZE` and encoded at `INPLACE_FALLBACK_LEVEL`.
    fn flush_inplace_stream(&mut self) -> std::io::Result<Vec<ResourceChunkInfo>> {
        let data = self.inplace_data.take()
            .expect("flush_inplace_stream called without in-place data");

        let reference_end = data.reference_chunks.iter()
            .map(|chunk| chunk.uncompressed_offset + chunk.uncompressed_size)
            .max()
            .unwrap_or(0);
        debug_assert_eq!(data.stream.len() as u64, data.append_offset);
        debug_assert!(data.append_offset >= reference_end);

        let mut chunks: Vec<ResourceChunkInfo> = Vec::new();

        self.resource_writer.seek(SeekFrom::Start(0))?;
        let mut write_offset = 0u64;

        for (index, reference) in data.reference_chunks.iter().enumerate() {
            let start = reference.uncompressed_offset as usize;
            let slice = &data.stream[start..start + reference.uncompressed_size as usize];

            let frame = if !data.dirty_chunks[index] {
                match &data.fallback_frames[index] {
                    // No level reproduced this chunk's frame; reuse the
                    // original frame bytes verbatim.
                    Some(frame) => frame.clone(),
                    None => encode_chunk(
                        slice,
                        data.chunk_levels[index]
                            .expect("clean chunk without fallback frame has a detected level"),
                    )?,
                }
            } else {
                // A changed chunk whose level could not be detected is
                // encoded at the fallback level; its content stays
                // correct, only the frame bytes differ from what the
                // reference's encoder would have produced.
                encode_chunk(slice, data.chunk_levels[index].unwrap_or(INPLACE_FALLBACK_LEVEL))?
            };

            let compressed_size = frame.len() as u64;
            let size = if compressed_size <= reference.size {
                reference.size
            } else {
                compressed_size
                    + (RESOURCE_CHUNK_ALIGNMENT - (compressed_size % RESOURCE_CHUNK_ALIGNMENT))
                        % RESOURCE_CHUNK_ALIGNMENT
            };

            self.resource_writer.write_all(&frame)?;
            self.resource_writer.padding(size - compressed_size)?;

            chunks.push(ResourceChunkInfo {
                offset: write_offset,
                size,
                compressed_size,
                uncompressed_offset: reference.uncompressed_offset,
                uncompressed_size: reference.uncompressed_size,
            });
            write_offset += size;
        }

        // appended region: fresh chunks after the reference stream's end
        let mut uncompressed_offset = reference_end;
        while uncompressed_offset < data.append_offset {
            let uncompressed_size = RESOURCE_CHUNK_SIZE.min(data.append_offset - uncompressed_offset);
            let start = uncompressed_offset as usize;
            let slice = &data.stream[start..start + uncompressed_size as usize];

            let frame = encode_chunk(slice, INPLACE_FALLBACK_LEVEL)?;
            let compressed_size = frame.len() as u64;
            let size = compressed_size
                + (RESOURCE_CHUNK_ALIGNMENT - (compressed_size % RESOURCE_CHUNK_ALIGNMENT))
                    % RESOURCE_CHUNK_ALIGNMENT;

            self.resource_writer.write_all(&frame)?;
            self.resource_writer.padding(size - compressed_size)?;

            chunks.push(ResourceChunkInfo {
                offset: write_offset,
                size,
                compressed_size,
                uncompressed_offset,
                uncompressed_size,
            });
            write_offset += size;
            uncompressed_offset += uncompressed_size;
        }

        self.resource_writer.flush()?;
        self.resource_writer.get_mut().set_len(write_offset)?;

        Ok(chunks)
    }

    pub fn write_content(
        &mut self,
        guid: &ContentHash,
        data: &[u8],
    ) -> std::io::Result<()> {
        if guid.size() != data.len() as u32 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Content size mismatch"));
        }

        if self.contents.contains_key(guid) {
            // if the content already exists, we don't need to write it again since they are unique
            return Ok(());
        }

        let container_writer = self.get_container_writer()?;
        let index = container_writer.index;

        let writer = container_writer.aquire()?;
        let offset = writer.stream_position()?;

        writer.write_all(data)?;
        writer.align(CONTENT_ALIGNMENT as usize)?;

        self.contents.insert(*guid, ContentEntry::new(offset, 0, index));

        Ok(())
    }

    pub fn finalize(mut self) -> Result<(), KFCWriteError> {
        // In-place mode builds the whole resources stream at finalize
        // (see `flush_inplace_stream`) and reports its own chunk table;
        // the append path submits its pending chunk buffer instead.
        let inplace_chunks = if self.inplace_data.is_some() {
            Some(self.flush_inplace_stream()?)
        } else {
            // Only submit a trailing chunk when resource data is actually
            // pending. Submitting unconditionally would append an empty chunk
            // (and grow the resources stream) even for a zero-change write.
            if !self.chunk_writer.get_ref().is_empty() {
                self.submit_resource_data()?;
            }

            None
        };

        // prepare container infos

        let mut containers = self.incremental_data.as_ref()
            .map(|data| data.reference_file.borrow().containers().to_vec())
            .unwrap_or_default();

        containers.reserve_exact(self.container_writers.len());

        self.container_writers.sort_by_key(|w| w.index);

        for container_writer in &mut self.container_writers {
            container_writer.flush()?;

            containers.push(ContainerInfo {
                size: container_writer.position()?,
                count: container_writer.count,
            });
        }

        // make sure to have a power of two container files

        let required_container_count = containers.len().next_power_of_two();

        while containers.len() < required_container_count {
            let path = self.dat_path(containers.len());

            if !path.exists() || self.options.truncate_containers {
                File::create(path)?;
            }

            containers.push(ContainerInfo {
                size: 0,
                count: 0,
            });
        }

        // prepare resource chunks

        let chunks = match inplace_chunks {
            Some(chunks) => chunks,
            None => {
                let mut chunks = self.incremental_data.as_ref()
                    .map(|data| data.reference_file.borrow().resource_chunks().to_vec())
                    .unwrap_or_default();

                chunks.extend(self.resource_chunks.iter().cloned());
                chunks
            }
        };

        // header construction

        let magic = {
            let position = self.file.stream_position()?;
            let mut tag = [0u8; 4];
            let found = self.file.seek(SeekFrom::Start(0)).is_ok() && self.file.read_exact(&mut tag).is_ok();
            self.file.seek(SeekFrom::Start(position))?;
            if found { u32::from_le_bytes(tag) } else { 0 }
        };

        let mut file_writer = BufWriter::new(&mut self.file);
        let mut file = KFCFile::default();
        file.set_magic(magic);

        file.set_game_version(self.game_version);
        match self.incremental_data.as_ref()
            .map(|data| data.reference_file.borrow())
        {
            // Incremental writes preserve the reference file's bundle
            // layout wherever the resource set allows it.
            Some(reference) => file.set_resources_preserving_bundles(
                self.resources.build(),
                self.type_registry.borrow(),
                reference,
            )?,
            None => file.set_resources(
                self.resources.build(),
                self.type_registry.borrow(),
                None,
            )?,
        }
        file.set_contents(self.contents.build());
        file.set_containers(containers);
        file.set_resource_chunks(chunks);

        file.write(&mut file_writer)?;

        Ok(())
    }

    pub fn dat_path(&self, index: usize) -> PathBuf {
        // Format: FILE_NAME_{INDEX}.dat where INDEX is 3 digits with leading zeros
        let name = format!(
            "{}_{:03}.{}",
            self.file_name,
            index,
            self.options.dat_extension
        );
        self.path.join(name)
    }

    fn get_container_writer(&mut self) -> std::io::Result<&mut ContainerWriter> {
        if !self.container_writers.is_empty() {
            let writer = self.container_writers.last_mut().unwrap();
            let pos = writer.position()?;

            if pos < self.options.max_container_size as u64 {
                return Ok(self.container_writers.last_mut().unwrap());
            }
        }

        // create a new container writer

        let base_index = self.incremental_data.as_ref()
            .map(|data| data.reference_file.borrow().containers().len())
            .unwrap_or(0);
        let next_index = base_index + self.container_writers.len();
        let path = self.dat_path(next_index);

        if !self.options.overwrite_containers && path.exists() {
            // make sure we don't accidentally overwrite an existing file
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("Data file already exists: {}", path.display())
            ));
        }

        let writer = BufWriter::new(File::create(&path)?);
        let writer = ContainerWriter::new(next_index, writer);

        self.container_writers.push(writer);

        Ok(self.container_writers.last_mut().unwrap())
    }

}

struct ContainerWriter {
    index: usize,
    count: usize,
    writer: BufWriter<File>,
}

impl ContainerWriter {

    #[inline]
    fn new(index: usize, writer: BufWriter<File>) -> Self {
        Self {
            index,
            count: 0,
            writer,
        }
    }

    #[inline]
    fn aquire(&mut self) -> std::io::Result<&mut BufWriter<File>> {
        self.count += 1;
        Ok(&mut self.writer)
    }

    #[inline]
    fn position(&mut self) -> std::io::Result<u64> {
        self.writer.stream_position()
    }

    #[inline]
    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }

}
