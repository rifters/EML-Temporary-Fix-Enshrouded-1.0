use std::io::{Read, Seek, Write};

use crate::{io::{ReadExt, ReadSeekExt, WriteExt, WriteSeekExt}, Hash32};

use super::{KFCReadError, KFCWriteError};

const KFC_DIR_MAGIC: u32 = 0x3343464B; // KFC3
const KFC_DIR_MAGIC_SERVER: u32 = 0x3443464B; // KFC4 (dedicated server)

pub fn is_known_magic(magic: u32) -> bool {
    magic == KFC_DIR_MAGIC || magic == KFC_DIR_MAGIC_SERVER
}

/// # Layout
/// ```c
/// struct KFCHeader {
///     u32 magic; // KFC_DIR_MAGIC
///     u32 size;
///     u32 unk0; // 12
///     u8 padding[4];
///
///     KFCLocation version; // -> u8[count]
///     KFCLocation containers; // -> ContainerInfo[count]
///
///     KFCLocation unused0; // unused
///     KFCLocation unused1; // unused
///
///     KFCLocation resource_locations; // -> ResourceLocation[count]
///     KFCLocation resource_indices; // -> u32[count]
///
///     KFCLocation content_buckets; // -> StaticMapBucket[count]
///     KFCLocation content_keys; // -> ContentHash[count]
///     KFCLocation content_values; // -> ContentEntry[count]
///
///     KFCLocation resource_buckets; // -> StaticMapBucket[count]
///     KFCLocation resource_keys; // -> ResourceId[count]
///     KFCLocation resource_values; // -> ResourceEntry[count]
///
///     KFCLocation resource_bundle_buckets; // -> StaticMapBucket[count]
///     KFCLocation resource_bundle_keys; // -> Hash32[count] (Type::internal_hash)
///     KFCLocation resource_bundle_values; // -> ResourceBundleEntry[count]
///
///     KFCLocation resource_chunks; // -> ResourceSectionInfo[count] (offset-4 has duplicate count)
/// };
/// ```
#[derive(Debug, Clone, Default)]
pub struct KFCHeader {
    pub magic: u32,
    pub size: u64,
    // pub unk0: u32,

    pub version: KFCLocation,
    pub containers: KFCLocation,

    pub unused0: KFCLocation,
    pub unused1: KFCLocation,

    pub resource_locations: KFCLocation,
    pub resource_indices: KFCLocation,

    pub content_buckets: KFCLocation,
    pub content_keys: KFCLocation,
    pub content_values: KFCLocation,

    pub resource_buckets: KFCLocation,
    pub resource_keys: KFCLocation,
    pub resource_values: KFCLocation,

    pub resource_bundle_buckets: KFCLocation,
    pub resource_bundle_keys: KFCLocation,
    pub resource_bundle_values: KFCLocation,

    pub resource_chunks: KFCLocation,
}

impl KFCHeader {

    pub fn read<R: Read + Seek>(reader: &mut R) -> Result<Self, KFCReadError> {
        let magic = reader.read_u32()?;

        if !is_known_magic(magic) {
            return Err(KFCReadError::InvalidMagic(magic));
        }

        let size = reader.read_u32()? as u64;
        let _unk0 = reader.read_u32()?;

        reader.padding(4)?;

        let version = KFCLocation::read(reader)?;
        let containers = KFCLocation::read(reader)?;

        let unused0 = KFCLocation::read(reader)?;
        let unused1 = KFCLocation::read(reader)?;

        let resource_locations = KFCLocation::read(reader)?;
        let resource_indices = KFCLocation::read(reader)?;

        let content_buckets = KFCLocation::read(reader)?;
        let content_keys = KFCLocation::read(reader)?;
        let content_values = KFCLocation::read(reader)?;

        let resource_buckets = KFCLocation::read(reader)?;
        let resource_keys = KFCLocation::read(reader)?;
        let resource_values = KFCLocation::read(reader)?;

        let resource_bundle_buckets = KFCLocation::read(reader)?;
        let resource_bundle_keys = KFCLocation::read(reader)?;
        let resource_bundle_values = KFCLocation::read(reader)?;

        let resource_chunks = KFCLocation::read(reader)?;

        Ok(Self {
            magic,
            size,

            version,
            containers,

            unused0,
            unused1,

            resource_locations,
            resource_indices,

            content_buckets,
            content_keys,
            content_values,

            resource_buckets,
            resource_keys,
            resource_values,

            resource_bundle_buckets,
            resource_bundle_keys,
            resource_bundle_values,

            resource_chunks,
        })
    }

    pub fn write<W: Write + Seek>(&self, writer: &mut W) -> Result<(), KFCWriteError> {
        if self.size > u32::MAX as u64 {
            return Err(KFCWriteError::SizeTooLarge(self.size));
        }

        writer.write_u32(if self.magic == 0 { KFC_DIR_MAGIC } else { self.magic })?;
        writer.write_u32(self.size as u32)?;
        writer.write_u32(12)?;
        writer.padding(4)?;

        self.version.write(writer)?;
        self.containers.write(writer)?;

        self.unused0.write(writer)?;
        self.unused1.write(writer)?;

        self.resource_locations.write(writer)?;
        self.resource_indices.write(writer)?;

        self.content_buckets.write(writer)?;
        self.content_keys.write(writer)?;
        self.content_values.write(writer)?;

        self.resource_buckets.write(writer)?;
        self.resource_keys.write(writer)?;
        self.resource_values.write(writer)?;

        self.resource_bundle_buckets.write(writer)?;
        self.resource_bundle_keys.write(writer)?;
        self.resource_bundle_values.write(writer)?;

        self.resource_chunks.write(writer)?;

        Ok(())
    }

}

/// # Layout
/// ```c
/// struct KFCLocation {
///     u32 offset;
///     u32 count;
/// };
/// ```
#[derive(Debug, Clone, Default)]
pub struct KFCLocation {
    pub offset: u64,
    pub count: usize,
}

impl KFCLocation {

    #[inline]
    pub fn new(offset: u64, count: usize) -> Self {
        Self {
            offset,
            count,
        }
    }

    #[inline]
    pub fn read<R: Read + Seek>(reader: &mut R) -> Result<Self, KFCReadError> {
        let offset = reader.read_u32_offset()?;
        let count = reader.read_u32()? as usize;

        Ok(Self {
            offset,
            count,
        })
    }

    #[inline]
    pub fn write<W: Write + Seek>(&self, writer: &mut W) -> Result<(), KFCWriteError> {
        writer.write_offset(self.offset)?;
        writer.write_u32(self.count as u32)?;

        Ok(())
    }

}

/// # Layout
/// ```c
/// struct ContainerInfo {
///     u64 size;
///     u64 count;
/// };
/// ```
#[derive(Debug, Clone, Default)]
pub struct ContainerInfo {
    pub size: u64,
    pub count: usize,
}

impl ContainerInfo {

    #[inline]
    pub fn read<R: Read>(reader: &mut R) -> Result<Self, KFCReadError> {
        let size = reader.read_u64()?;
        let count = reader.read_u64()? as usize;

        Ok(Self {
            size,
            count,
        })
    }

    #[inline]
    pub fn write<W: Write + Seek>(&self, writer: &mut W) -> Result<(), KFCWriteError> {
        writer.write_u64(self.size)?;
        writer.write_u64(self.count as u64)?;

        Ok(())
    }

}

/// # Layout
/// ```c
/// struct ResourceLocation {
///     u32 uncompressed_size;
///     u32 compressed_size;
///     u32 count;
/// };
/// ```
#[derive(Debug, Clone, Default)]
pub struct ResourceLocation {
    pub uncompressed_size: u64,
    pub compressed_size: u64,
    pub count: usize,
}

impl ResourceLocation {

    #[inline]
    pub fn read<R: Read>(reader: &mut R) -> Result<Self, KFCReadError> {
        let uncompressed_size = reader.read_u32()? as u64;
        let compressed_size = reader.read_u32()? as u64;
        let count = reader.read_u32()? as usize;

        Ok(Self {
            uncompressed_size,
            compressed_size,
            count,
        })
    }

    #[inline]
    pub fn write<W: Write + Seek>(&self, writer: &mut W) -> Result<(), KFCWriteError> {
        writer.write_u32(self.uncompressed_size as u32)?;
        writer.write_u32(self.compressed_size as u32)?;
        writer.write_u32(self.count as u32)?;

        Ok(())
    }

}

/// # Layout
/// ```c
/// struct ContentEntry {
///     u32 offset;
///     u16 flags;
///     u16 container_index;
///     u8 padding[8];
/// };
/// ```
#[derive(Debug, Clone, Default)]
pub struct ContentEntry {
    pub offset: u64,
    pub flags: u16,
    pub container_index: usize,
}

impl ContentEntry {

    #[inline]
    pub fn new(offset: u64, flags: u16, container_index: usize) -> Self {
        Self {
            offset,
            flags,
            container_index,
        }
    }

    #[inline]
    pub fn read<R: Read>(reader: &mut R) -> Result<Self, KFCReadError> {
        let offset = reader.read_u32()? as u64;
        let flags = reader.read_u16()?;
        let container_index = reader.read_u16()? as usize;
        reader.padding(8)?;

        Ok(Self {
            offset,
            flags,
            container_index,
        })
    }

    #[inline]
    pub fn write<W: Write + Seek>(&self, writer: &mut W) -> Result<(), KFCWriteError> {
        writer.write_u32(self.offset as u32)?;
        writer.write_u16(self.flags)?;
        writer.write_u16(self.container_index as u16)?;
        writer.padding(8)?;

        Ok(())
    }

}

/// # Layout
/// ```c
/// struct ResourceEntry {
///     u32 offset;
///     u32 size;
/// };
/// ```
#[derive(Debug, Clone, Default)]
pub struct ResourceEntry {
    pub offset: u64,
    pub size: u64,
}

impl ResourceEntry {

    #[inline]
    pub fn new(offset: u64, size: u64) -> Self {
        Self {
            offset,
            size,
        }
    }

    #[inline]
    pub fn read<R: Read>(reader: &mut R) -> Result<Self, KFCReadError> {
        let offset = reader.read_u32()? as u64;
        let size = reader.read_u32()? as u64;

        Ok(Self {
            offset,
            size,
        })
    }

    #[inline]
    pub fn write<W: Write + Seek>(&self, writer: &mut W) -> Result<(), KFCWriteError> {
        writer.write_u32(self.offset as u32)?;
        // The game stores resource sizes in 24 bits: a size of 16 MiB or
        // larger is truncated to its lower 24 bits, and the reader
        // recovers the actual size from the neighbouring offsets (see
        // `KFCReader::resource_size`). Truncate here as well, so a size
        // that was re-derived from the actual data (e.g. by
        // `KFCWriter::write_resource`, which records the true byte count)
        // serializes exactly like the game's own writer wrote it:
        // 44,659,852 & 0xFF_FFFF = 11,105,420.
        writer.write_u32(self.size as u32 & 0x00FF_FFFF)?;

        Ok(())
    }

}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn write_entry(entry: &ResourceEntry) -> Vec<u8> {
        let mut buffer = Cursor::new(Vec::new());
        entry.write(&mut buffer).unwrap();
        buffer.into_inner()
    }

    #[test]
    fn resource_entry_write_truncates_size_to_24_bits() {
        // The one resource in the build-1273043 container whose size
        // exceeds 24 bits: stored by the game as 11,105,420, actual
        // 44,659,852 bytes.
        assert_eq!(44_659_852 & 0x00FF_FFFF, 11_105_420);

        let bytes = write_entry(&ResourceEntry::new(25_175_536, 44_659_852));
        assert_eq!(
            bytes,
            [
                25_175_536u32.to_le_bytes(),
                11_105_420u32.to_le_bytes()
            ]
            .concat()
        );

        // Exactly 16 MiB truncates to zero, one byte less is kept whole.
        let bytes = write_entry(&ResourceEntry::new(0, 0x100_0000));
        assert_eq!(&bytes[4..], 0u32.to_le_bytes());
        let bytes = write_entry(&ResourceEntry::new(0, 0xFF_FFFF));
        assert_eq!(&bytes[4..], 0xFF_FFFFu32.to_le_bytes());
    }

    #[test]
    fn resource_entry_write_keeps_small_sizes() {
        for size in [0, 1, 9_376, 1_048_576] {
            let bytes = write_entry(&ResourceEntry::new(16, size));
            assert_eq!(&bytes[4..], (size as u32).to_le_bytes());
        }
    }
}

/// # Layout
/// ```c
/// struct ResourceBundleEntry {
///     u32 internal_hash;
///     u32 index;
///     u32 count;
/// };
/// ```
#[derive(Debug, Clone, Default)]
pub struct ResourceBundleEntry {
    pub internal_hash: Hash32,
    pub index: usize,
    pub count: usize,
}

impl ResourceBundleEntry {

    #[inline]
    pub fn read<R: Read>(reader: &mut R) -> Result<Self, KFCReadError> {
        let internal_hash = reader.read_u32()?;
        let index = reader.read_u32()? as usize;
        let count = reader.read_u32()? as usize;

        Ok(Self {
            internal_hash,
            index,
            count,
        })
    }

    #[inline]
    pub fn write<W: Write + Seek>(&self, writer: &mut W) -> Result<(), KFCWriteError> {
        writer.write_u32(self.internal_hash)?;
        writer.write_u32(self.index as u32)?;
        writer.write_u32(self.count as u32)?;

        Ok(())
    }

}

/// # Layout
/// ```c
/// struct ResourceChunkInfo {
///     u32 offset;
///     u32 size;
///     u32 compressed_size;
///     u32 uncompressed_offset;
///     u32 uncompressed_size;
/// };
/// ```
#[derive(Debug, Clone, Default)]
pub struct ResourceChunkInfo {
    pub offset: u64,
    pub size: u64,
    pub compressed_size: u64,
    pub uncompressed_offset: u64,
    pub uncompressed_size: u64,
}

impl ResourceChunkInfo {

    #[inline]
    pub fn read<R: Read>(reader: &mut R) -> Result<Self, KFCReadError> {
        let offset = reader.read_u32()? as u64;
        let size = reader.read_u32()? as u64;
        let compressed_size = reader.read_u32()? as u64;
        let uncompressed_offset = reader.read_u32()? as u64;
        let uncompressed_size = reader.read_u32()? as u64;

        Ok(Self {
            offset,
            size,
            compressed_size,
            uncompressed_offset,
            uncompressed_size,
        })
    }

    #[inline]
    pub fn write<W: Write + Seek>(&self, writer: &mut W) -> Result<(), KFCWriteError> {
        writer.write_u32(self.offset as u32)?;
        writer.write_u32(self.size as u32)?;
        writer.write_u32(self.compressed_size as u32)?;
        writer.write_u32(self.uncompressed_offset as u32)?;
        writer.write_u32(self.uncompressed_size as u32)?;

        Ok(())
    }

}
