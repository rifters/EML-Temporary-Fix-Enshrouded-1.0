use std::{
    collections::HashMap,
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
};

use crate::{
    Hash32,
    guid::{ContentHash, ResourceId},
    io::{ReadExt, WriteExt, WriteSeekExt},
    reflection::{LookupKey, TypeRegistry},
};

use super::{KFCReadError, KFCWriteError, StaticMap, StaticMapBucket, StaticMapBuilder, header::*};

#[derive(Debug, Clone)]
pub struct KFCFile {
    magic: u32,
    version: String,

    containers: Vec<ContainerInfo>,
    resource_locations: Vec<ResourceLocation>,
    resource_chunks: Vec<ResourceChunkInfo>,

    contents: StaticMap<ContentHash, ContentEntry>,
    resources: StaticMap<ResourceId, ResourceEntry>,

    resource_indices: Vec<u32>,
    resource_bundles: StaticMap<Hash32, ResourceBundleEntry>,
}

impl Default for KFCFile {
    fn default() -> Self {
        Self {
            magic: 0,
            resource_locations: vec![ResourceLocation::default()],
            resource_chunks: Vec::default(),

            version: String::default(),
            containers: Vec::default(),

            contents: StaticMap::default(),
            resources: StaticMap::default(),

            resource_indices: Vec::default(),
            resource_bundles: StaticMap::default(),
        }
    }
}

impl KFCFile {

    pub fn from_path<P: AsRef<Path>>(
        path: P,
        skip_entries: bool
    ) -> Result<Self, KFCReadError> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        Self::read(&mut reader, skip_entries)
    }

    pub fn from_reader<R: Read + Seek>(
        reader: &mut R,
        skip_entries: bool,
    ) -> Result<Self, KFCReadError> {
        Self::read(reader, skip_entries)
    }

    pub fn get_version_tag<P: AsRef<Path>>(
        path: P
    ) -> Result<String, KFCReadError> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);
        let header = KFCHeader::read(&mut reader)?;
        reader.seek(SeekFrom::Start(header.version.offset))?;
        let version = reader.read_string(header.version.count)?;
        Ok(version)
    }

    #[inline]
    pub fn resources(&self) -> &StaticMap<ResourceId, ResourceEntry> {
        &self.resources
    }

    #[inline]
    pub fn resource_chunks(&self) -> &[ResourceChunkInfo] {
        &self.resource_chunks
    }

    #[inline]
    pub fn resource_bundles(&self) -> &StaticMap<Hash32, ResourceBundleEntry> {
        &self.resource_bundles
    }

    #[inline]
    pub fn contents(&self) -> &StaticMap<ContentHash, ContentEntry> {
        &self.contents
    }

    pub fn set_magic(&mut self, magic: u32) {
        self.magic = if is_known_magic(magic) { magic } else { 0 };
    }

    #[inline]
    pub fn game_version(&self) -> &str {
        &self.version
    }

    #[inline]
    pub fn containers(&self) -> &[ContainerInfo] {
        &self.containers
    }

    pub fn resource_types(&self) -> impl Iterator<Item = Hash32> {
        self.resource_bundles.values().iter().filter_map(|info| {
            let start = info.index;
            let end = start + info.count;
            let resource_indices = &self.resource_indices[start..end];

            if resource_indices.is_empty() {
                return None;
            }

            Some(self.resources.keys()[resource_indices[0] as usize].type_hash())
        })
    }

    pub fn resources_by_type(&self, type_hash: Hash32) -> impl Iterator<Item = &ResourceId> {
        self.resource_bundles
            .get(&type_hash)
            .map(|info| {
                let start = info.index;
                let end = start + info.count;
                &self.resource_indices[start..end]
            })
            .unwrap_or_default()
            .iter()
            .map(|&index| &self.resources.keys()[index as usize])
    }

    // mutators

    pub fn set_resources(
        &mut self,
        resources: StaticMap<ResourceId, ResourceEntry>,
        type_registry: &TypeRegistry,
        internal_hash_fallback: Option<&StaticMap<Hash32, ResourceBundleEntry>>,
    ) -> Result<(), KFCWriteError> {
        self.resources = resources;
        self.resource_locations[0].count = self.resources.len();
        self.rebuild_resource_bundles(type_registry, internal_hash_fallback, None)
    }

    /// Like [`KFCFile::set_resources`], but preserves the bundle layout of
    /// a previous version of this file wherever possible:
    ///
    /// - If the resource key sequence is unchanged, the previous file's
    ///   `resource_indices` and bundle map are carried over verbatim, so a
    ///   zero-change write reproduces the loaded layout byte for byte.
    /// - Otherwise, types that already existed keep the relative order of
    ///   their previous bundle entries, and types that are new are appended
    ///   afterwards, ascending by type hash. Indices are then reassigned as
    ///   running offsets in that group order.
    pub fn set_resources_preserving_bundles(
        &mut self,
        resources: StaticMap<ResourceId, ResourceEntry>,
        type_registry: &TypeRegistry,
        previous: &KFCFile,
    ) -> Result<(), KFCWriteError> {
        self.resources = resources;
        self.resource_locations[0].count = self.resources.len();
        self.rebuild_resource_bundles(
            type_registry,
            Some(previous.resource_bundles()),
            Some(previous),
        )
    }

    pub fn set_resource_chunks(
        &mut self,
        chunks: Vec<ResourceChunkInfo>
    ) {
        self.resource_chunks = chunks;
        self.resource_locations[0].uncompressed_size = self.resource_chunks.iter()
            .map(|chunk| chunk.uncompressed_size)
            .sum();
        self.resource_locations[0].compressed_size = self.resource_chunks.iter()
            .map(|chunk| chunk.compressed_size)
            .sum();
    }

    pub fn set_contents(
        &mut self,
        contents: StaticMap<ContentHash, ContentEntry>
    ) {
        self.contents = contents;
    }

    pub fn set_containers(
        &mut self,
        containers: Vec<ContainerInfo>
    ) {
        self.containers = containers;
    }

    pub fn set_game_version(&mut self, version: String) {
        self.version = version;
    }

    fn rebuild_resource_bundles(
        &mut self,
        type_registry: &TypeRegistry,
        internal_hash_fallback: Option<&StaticMap<Hash32, ResourceBundleEntry>>,
        previous: Option<&KFCFile>,
    ) -> Result<(), KFCWriteError> {
        // Fast path: the resource key sequence is unchanged, so the
        // previous file's bundle layout is still exact. Carry it over
        // verbatim instead of recomputing it.
        if let Some(previous) = previous {
            if previous.resources.keys() == self.resources.keys()
                && previous.resource_indices.len() == self.resources.len()
            {
                self.resource_indices = previous.resource_indices.clone();
                self.resource_bundles = previous.resource_bundles.clone();
                return Ok(());
            }
        }

        // Group the positions of the resources by type hash. Within one
        // group the positions follow the resource key order.
        let mut groups: Vec<(Hash32, Vec<u32>)> = Vec::new();
        let mut group_positions: HashMap<Hash32, usize> = HashMap::new();

        for (i, (guid, _)) in self.resources.iter().enumerate() {
            let hash = guid.type_hash();

            match group_positions.get(&hash) {
                Some(&group) => groups[group].1.push(i as u32),
                None => {
                    group_positions.insert(hash, groups.len());
                    groups.push((hash, vec![i as u32]));
                }
            }
        }

        // Determine the order in which the type groups are laid out in
        // `resource_indices`: types that already existed in the previous
        // file keep the relative order of their previous bundle entries;
        // new types are appended afterwards, ascending by type hash, so
        // the layout is deterministic.
        let mut group_order: Vec<Hash32> = Vec::new();

        if let Some(previous) = previous {
            let mut previous_bundles: Vec<(&Hash32, &ResourceBundleEntry)> =
                previous.resource_bundles.iter().collect();
            previous_bundles.sort_by_key(|(_, entry)| entry.index);

            for (hash, _) in previous_bundles {
                if group_positions.contains_key(hash) {
                    group_order.push(*hash);
                }
            }
        }

        let mut new_types: Vec<Hash32> = groups
            .iter()
            .map(|(hash, _)| *hash)
            .filter(|hash| !group_order.contains(hash))
            .collect();
        new_types.sort_unstable();
        group_order.extend(new_types);

        // Assign the bundle entries in group order.
        let mut indices = Vec::with_capacity(self.resources.len());
        let mut bundles = StaticMapBuilder::default();

        for hash in &group_order {
            let internal_hash = type_registry.get_by_hash(LookupKey::Qualified(*hash))
                .map(|info| info.internal_hash)
                .or_else(|| internal_hash_fallback
                    .and_then(|map| map.get(hash))
                    .map(|entry| entry.internal_hash))
                .ok_or(KFCWriteError::UnknownTypeHash(*hash))?;

            let group = &groups[group_positions[hash]].1;

            bundles.insert(*hash, ResourceBundleEntry {
                internal_hash,
                index: indices.len(),
                count: group.len(),
            });

            indices.extend_from_slice(group);
        }

        self.resource_indices = indices;
        self.resource_bundles = bundles.build();

        Ok(())
    }

}

impl KFCFile {

    fn read<R: Read + Seek>(reader: &mut R, skip_entries: bool) -> Result<Self, KFCReadError> {
        let header = KFCHeader::read(reader)?;

        // version
        reader.seek(SeekFrom::Start(header.version.offset))?;
        let version = reader.read_string(header.version.count)?;

        // containers
        reader.seek(SeekFrom::Start(header.containers.offset))?;
        let containers = (0..header.containers.count)
            .map(|_| ContainerInfo::read(reader))
            .collect::<Result<Vec<_>, _>>()?;

        // resource locations
        reader.seek(SeekFrom::Start(header.resource_locations.offset))?;
        let resource_locations = (0..header.resource_locations.count)
            .map(|_| ResourceLocation::read(reader))
            .collect::<Result<Vec<_>, _>>()?;

        if !skip_entries {
            // resource indices
            reader.seek(SeekFrom::Start(header.resource_indices.offset))?;
            let resource_indices = (0..header.resource_indices.count)
                .map(|_| reader.read_u32())
                .collect::<Result<Vec<_>, _>>()?;

            // content static map

            reader.seek(SeekFrom::Start(header.content_buckets.offset))?;
            let content_buckets = (0..header.content_buckets.count)
                .map(|_| StaticMapBucket::read(reader))
                .collect::<Result<Vec<_>, _>>()?;

            reader.seek(SeekFrom::Start(header.content_keys.offset))?;
            let content_keys = (0..header.content_keys.count)
                .map(|_| ContentHash::read(reader))
                .collect::<Result<Vec<_>, _>>()?;

            reader.seek(SeekFrom::Start(header.content_values.offset))?;
            let content_values = (0..header.content_values.count)
                .map(|_| ContentEntry::read(reader))
                .collect::<Result<Vec<_>, _>>()?;

            // resource static map

            reader.seek(SeekFrom::Start(header.resource_buckets.offset))?;
            let resource_buckets = (0..header.resource_buckets.count)
                .map(|_| StaticMapBucket::read(reader))
                .collect::<Result<Vec<_>, _>>()?;

            reader.seek(SeekFrom::Start(header.resource_keys.offset))?;
            let resource_keys = (0..header.resource_keys.count)
                .map(|_| ResourceId::read(reader))
                .collect::<Result<Vec<_>, _>>()?;

            reader.seek(SeekFrom::Start(header.resource_values.offset))?;
            let resource_values = (0..header.resource_values.count)
                .map(|_| ResourceEntry::read(reader))
                .collect::<Result<Vec<_>, _>>()?;

            // resource_bundle static map

            reader.seek(SeekFrom::Start(header.resource_bundle_buckets.offset))?;
            let resource_bundle_buckets = (0..header.resource_bundle_buckets.count)
                .map(|_| StaticMapBucket::read(reader))
                .collect::<Result<Vec<_>, _>>()?;

            reader.seek(SeekFrom::Start(header.resource_bundle_keys.offset))?;
            let resource_bundle_keys = (0..header.resource_bundle_keys.count)
                .map(|_| reader.read_u32())
                .collect::<Result<Vec<_>, _>>()?;

            reader.seek(SeekFrom::Start(header.resource_bundle_values.offset))?;
            let resource_bundle_values = (0..header.resource_bundle_values.count)
                .map(|_| ResourceBundleEntry::read(reader))
                .collect::<Result<Vec<_>, _>>()?;

            // resource_chunks
            reader.seek(SeekFrom::Start(header.resource_chunks.offset))?;
            let resource_chunks = (0..header.resource_chunks.count)
                .map(|_| ResourceChunkInfo::read(reader))
                .collect::<Result<Vec<_>, _>>()?;

            Ok(Self {
                magic: header.magic,
                version,
                containers,

                resource_locations,
                resource_indices,

                contents: StaticMap::from_parts(content_keys, content_values, content_buckets)?,
                resources: StaticMap::from_parts(resource_keys, resource_values, resource_buckets)?,
                resource_bundles: StaticMap::from_parts(resource_bundle_keys, resource_bundle_values, resource_bundle_buckets)?,

                resource_chunks,
            })
        } else {
            Ok(Self {
                magic: header.magic,
                version,
                containers: Vec::new(),
                resource_locations,
                resource_indices: Vec::new(),
                contents: StaticMap::default(),
                resources: StaticMap::default(),
                resource_bundles: StaticMap::default(),
                resource_chunks: Vec::new(),
            })
        }
    }

    pub(super) fn write<W: Write + Seek>(&self, writer: &mut W) -> Result<(), KFCWriteError> {
        KFCHeader::default().write(writer)?;

        // version
        let version_offset = writer.stream_position()?;
        writer.write_string(&self.version, self.version.len())?;
        writer.align(8)?;

        // containers
        let containers_offset = writer.stream_position()?;
        for container in &self.containers {
            container.write(writer)?;
        }
        writer.align(8)?;

        // object locations
        let resource_locations_offset = writer.stream_position()?;
        ResourceLocation::default().write(writer)?;
        writer.align(8)?;

        // resource chunks
        let resource_chunks_offset = writer.stream_position()? + 4;
        writer.write_u32(self.resource_chunks.len() as u32)?;
        for resource_chunks in &self.resource_chunks {
            resource_chunks.write(writer)?;
        }

        // resource indices
        let resource_indices_offset = writer.stream_position()?;
        for resource_index in &self.resource_indices {
            writer.write_u32(*resource_index)?;
        }

        // content static map

        let content_buckets_offset = writer.stream_position()?;
        for content_bucket in self.contents.buckets() {
            content_bucket.write(writer)?;
        }

        let content_keys_offset = writer.stream_position()?;
        for content_key in self.contents.keys() {
            content_key.write(writer)?;
        }
        writer.align(8)?;

        let content_values_offset = writer.stream_position()?;
        for content_value in self.contents.values() {
            content_value.write(writer)?;
        }

        // resource static map

        let resource_buckets_offset = writer.stream_position()?;
        for resource_bucket in self.resources.buckets() {
            resource_bucket.write(writer)?;
        }

        let resource_keys_offset = writer.stream_position()?;
        for resource_key in self.resources.keys() {
            resource_key.write(writer)?;
        }
        writer.align(8)?;

        let resource_values_offset = writer.stream_position()?;
        for resource_value in self.resources.values() {
            resource_value.write(writer)?;
        }

        // resource_bundle static map

        let resource_bundle_buckets_offset = writer.stream_position()?;
        for resource_bundle_bucket in self.resource_bundles.buckets() {
            resource_bundle_bucket.write(writer)?;
        }

        let resource_bundle_keys_offset = writer.stream_position()?;
        for resource_bundle_key in self.resource_bundles.keys() {
            writer.write_u32(*resource_bundle_key)?;
        }

        let resource_bundle_values_offset = writer.stream_position()?;
        for resource_bundle_value in self.resource_bundles.values() {
            resource_bundle_value.write(writer)?;
        }

        let size = writer.stream_position()?;

        writer.align(0x1000)?;

        // resource locations (update)
        writer.seek(SeekFrom::Start(resource_locations_offset))?;
        for resource_location in &self.resource_locations {
            resource_location.write(writer)?;
        }

        // KFCHeader
        let header = KFCHeader {
            magic: self.magic,
            unused0: Default::default(),
            unused1: Default::default(),

            size,

            version: KFCLocation::new(version_offset, self.version.len()),
            containers: KFCLocation::new(containers_offset, self.containers.len()),

            resource_locations: KFCLocation::new(
                resource_locations_offset,
                self.resource_locations.len(),
            ),
            resource_indices: KFCLocation::new(resource_indices_offset, self.resource_indices.len()),

            content_buckets: KFCLocation::new(content_buckets_offset, self.contents.buckets().len()),
            content_keys: KFCLocation::new(content_keys_offset, self.contents.len()),
            content_values: KFCLocation::new(content_values_offset, self.contents.len()),

            resource_buckets: KFCLocation::new(resource_buckets_offset, self.resources.buckets().len()),
            resource_keys: KFCLocation::new(resource_keys_offset, self.resources.len()),
            resource_values: KFCLocation::new(resource_values_offset, self.resources.len()),

            resource_bundle_buckets: KFCLocation::new(resource_bundle_buckets_offset, self.resource_bundles.buckets().len()),
            resource_bundle_keys: KFCLocation::new(resource_bundle_keys_offset, self.resource_bundles.len()),
            resource_bundle_values: KFCLocation::new(resource_bundle_values_offset, self.resource_bundles.len()),

            resource_chunks: KFCLocation::new(resource_chunks_offset, self.resource_chunks.len()),
        };

        writer.seek(SeekFrom::Start(0))?;
        header.write(writer)?;

        writer.seek(SeekFrom::Start(size))?;

        Ok(())
    }

}

#[cfg(test)]
mod magic_tests {
    use super::*;
    use std::io::Cursor;

    fn sample(path: &str) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }

    #[test]
    fn server_directory_is_read_and_rewritten_with_its_own_tag() {
        let Some(bytes) = std::env::var("M182_KFC_SAMPLE").ok().and_then(|p| sample(&p)) else { return };
        let original_tag = [bytes[0], bytes[1], bytes[2], bytes[3]];
        let file = KFCFile::from_reader(&mut Cursor::new(bytes.clone()), false).expect("directory must be readable");
        assert!(!file.resources().is_empty());

        let mut out = Cursor::new(Vec::new());
        file.write(&mut out).expect("rewrite");
        let written = out.into_inner();
        assert_eq!(&written[0..4], &original_tag, "tag must be preserved");

        let again = KFCFile::from_reader(&mut Cursor::new(written), false).expect("rewritten directory must be readable");
        assert_eq!(again.resources().len(), file.resources().len());
        assert_eq!(again.contents().len(), file.contents().len());
        assert_eq!(again.game_version(), file.game_version());
        assert_eq!(again.containers().len(), file.containers().len());
        assert_eq!(again.resource_chunks().len(), file.resource_chunks().len());
    }
}
