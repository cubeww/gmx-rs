use std::collections::HashSet;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use md5::{Digest, Md5};
use rayon::prelude::*;

use crate::artifact::GeneratedFile;
use crate::assets::Assets;
use crate::cache::{cache_enabled, write_atomic};
use crate::path::gmx_path;
use crate::resources::Sound;
use crate::tool::find_tool;
use crate::wad::{ChunkOptions, ChunkWriter, FourCc, WadBuilder, WriteError};

const AUDO: FourCc = FourCc::new(*b"AUDO");
const AUDIO_CACHE_SCHEMA: &[u8] = b"gmx-rs-audio-cache-v3\0";
const OGG_CRC_TABLE: [u32; 256] = ogg_crc_table();
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct SoundMedia {
    pub kind: i32,
    pub filename: String,
    pub group: usize,
    pub audio_id: i32,
}

#[derive(Debug)]
struct AudioBlob {
    data: Arc<[u8]>,
}

#[derive(Debug)]
pub struct AudioData {
    sounds: Vec<SoundMedia>,
    groups: Vec<Vec<AudioBlob>>,
    streamed: Vec<GeneratedFile>,
    target_masks: Vec<i64>,
    target_mask: i64,
}

impl AudioData {
    pub fn prepare(assets: &Assets, cache_root: &Path) -> Result<Self, WriteError> {
        let group_count = assets.settings.audio_groups.len().max(1);
        let sounds = sound_media(assets, group_count)?;
        if assets.sounds.is_empty() {
            return Ok(Self {
                sounds,
                groups: (0..group_count).map(|_| Vec::new()).collect(),
                streamed: Vec::new(),
                target_masks: assets
                    .settings
                    .audio_groups
                    .iter()
                    .map(|group| group.target_mask)
                    .collect(),
                target_mask: assets.settings.target_mask,
            });
        }

        let ffmpeg = find_ffmpeg().map_err(|error| invalid_audio(error.to_string()))?;
        let converted = assets
            .sounds
            .par_iter()
            .map(|sound| convert_sound(assets, sound, &ffmpeg, cache_root))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| invalid_audio(error.to_string()))?;

        let mut grouped = (0..group_count)
            .map(|_| Vec::<(String, Arc<[u8]>)>::new())
            .collect::<Vec<_>>();
        let mut streamed = Vec::new();
        for (sound, media) in assets.sounds.iter().zip(&sounds) {
            let data = Arc::<[u8]>::from(converted[sound.index].clone());
            if sound.streamed {
                streamed.push(GeneratedFile::new(
                    PathBuf::from(&media.filename),
                    Arc::clone(&data),
                ));
            } else {
                grouped[media.group].push((original_filename(sound), data));
            }
        }
        let groups = grouped
            .into_iter()
            .map(|mut group| {
                group.sort_by(|left, right| culture_cmp(&left.0, &right.0));
                group
                    .into_iter()
                    .map(|(_, data)| AudioBlob { data })
                    .collect()
            })
            .collect();

        Ok(Self {
            sounds,
            groups,
            streamed,
            target_masks: assets
                .settings
                .audio_groups
                .iter()
                .map(|group| group.target_mask)
                .collect(),
            target_mask: assets.settings.target_mask,
        })
    }

    pub fn sound(&self, index: usize) -> &SoundMedia {
        &self.sounds[index]
    }

    pub fn add_chunk<'a>(&'a self, builder: &mut WadBuilder<'a>) -> Result<(), WriteError> {
        builder.add_chunk_with(AUDO, ChunkOptions::AUDIO, move |writer| {
            write_audo(writer, &self.groups[0])
        })
    }

    pub fn external_files(&self) -> Result<Vec<GeneratedFile>, WriteError> {
        let mut files = self.streamed.to_vec();
        for group_index in 1..self.groups.len() {
            if self.groups[group_index].is_empty()
                || self
                    .target_masks
                    .get(group_index)
                    .copied()
                    .unwrap_or(i64::MAX)
                    & self.target_mask
                    == 0
            {
                continue;
            }
            let group = &self.groups[group_index];
            let mut builder = WadBuilder::new();
            builder.add_chunk_with(AUDO, ChunkOptions::AUDIO, move |writer| {
                write_audo(writer, group)
            })?;
            let mut output = Cursor::new(Vec::new());
            builder.write_to(&mut output)?;
            files.push(GeneratedFile::new(
                PathBuf::from(format!("audiogroup{group_index}.dat")),
                output.into_inner(),
            ));
        }
        Ok(files)
    }
}

fn write_audo(writer: &mut ChunkWriter<'_>, sounds: &[AudioBlob]) -> Result<(), WriteError> {
    writer.write_u32(as_u32(sounds.len(), "audio waveform count")?)?;
    let mut records = Vec::with_capacity(sounds.len());
    for _ in sounds {
        records.push(writer.reserve_u32()?);
    }
    for (sound, record) in sounds.iter().zip(records) {
        writer.align(4)?;
        writer.patch_position(record)?;
        writer.write_u32(as_u32(sound.data.len(), "audio waveform size")?)?;
        std::io::Write::write_all(writer, &sound.data)?;
    }
    Ok(())
}

fn sound_media(assets: &Assets, group_count: usize) -> Result<Vec<SoundMedia>, WriteError> {
    let mut group_sounds = vec![Vec::<&Sound>::new(); group_count];
    for sound in &assets.sounds {
        if !sound.streamed {
            group_sounds[sound_group(sound, group_count)].push(sound);
        }
    }
    for sounds in &mut group_sounds {
        sounds.sort_by(|left, right| culture_cmp(&left.name, &right.name));
    }
    let mut audio_ids = vec![-1; assets.sounds.len()];
    for sounds in &group_sounds {
        for (index, sound) in sounds.iter().enumerate() {
            audio_ids[sound.index] =
                i32::try_from(index).map_err(|_| WriteError::SizeOverflow {
                    field: "sound audio index",
                    size: index as u64,
                })?;
        }
    }

    let mut used_default_names = HashSet::new();
    let mut result = Vec::with_capacity(assets.sounds.len());
    for sound in &assets.sounds {
        let group = sound_group(sound, group_count);
        let mut filename = unique_default_audio_name(original_filename(sound), &used_default_names);
        let kind = if sound.streamed {
            if sound.new_audio {
                filename = with_extension(&filename, "ogg");
                100
            } else {
                filename = with_extension(&filename, "mp3");
                1
            }
        } else if sound.new_audio {
            if sound.uncompress_on_load {
                103
            } else if sound.compressed {
                102
            } else {
                101
            }
        } else {
            filename = with_extension(&filename, "wav");
            0
        };
        if !sound.streamed && group == 0 {
            used_default_names.insert(filename.clone());
        }
        result.push(SoundMedia {
            kind,
            filename,
            group,
            audio_id: audio_ids[sound.index],
        });
    }
    Ok(result)
}

fn sound_group(sound: &Sound, group_count: usize) -> usize {
    usize::try_from(sound.group_index)
        .ok()
        .filter(|group| *group < group_count)
        .unwrap_or(0)
}

fn original_filename(sound: &Sound) -> String {
    gmx_path(&sound.original_name)
        .file_name()
        .unwrap_or_else(|| Path::new(&sound.original_name).as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn unique_default_audio_name(mut name: String, used: &HashSet<String>) -> String {
    let mut suffix = 0;
    while used.contains(&name) {
        let extension = Path::new(&name)
            .extension()
            .map(|value| value.to_string_lossy().into_owned());
        let parts = name.split('_').collect::<Vec<_>>();
        let base = if parts.len() <= 1 {
            parts[0]
        } else {
            parts[parts.len() - 2]
        };
        let path = Path::new(base);
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        name = match extension {
            Some(extension) if !extension.is_empty() => format!("{stem}_{suffix}.{extension}"),
            _ => format!("{stem}_{suffix}"),
        };
        suffix += 1;
    }
    name
}

fn with_extension(filename: &str, extension: &str) -> String {
    let stem = Path::new(filename)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    format!("{stem}.{extension}")
}

#[derive(Debug, Clone, Copy)]
enum AudioFormat {
    Wav,
    Ogg,
    Mp3,
}

impl AudioFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Ogg => "ogg",
            Self::Mp3 => "mp3",
        }
    }
}

fn convert_sound(
    assets: &Assets,
    sound: &Sound,
    ffmpeg: &Path,
    cache_root: &Path,
) -> Result<Vec<u8>, AudioError> {
    let source_data =
        assets
            .binary(&sound.audio_source)
            .ok_or_else(|| AudioError::MissingSource {
                sound: sound.name.clone(),
                path: sound.audio_source.clone(),
            })?;
    let format = if sound.compressed {
        if sound.new_audio {
            AudioFormat::Ogg
        } else {
            AudioFormat::Mp3
        }
    } else {
        AudioFormat::Wav
    };

    let mut hasher = Md5::new();
    hasher.update(AUDIO_CACHE_SCHEMA);
    hasher.update(source_data);
    hasher.update(ffmpeg.as_os_str().to_string_lossy().as_bytes());
    add_file_fingerprint(&mut hasher, ffmpeg);
    hasher.update([format as u8]);
    hasher.update(sound.bit_rate.to_le_bytes());
    hasher.update(sound.sample_rate.to_le_bytes());
    hasher.update(sound.bit_depth.to_le_bytes());
    hasher.update([u8::from(sound.stereo)]);
    let key = hasher.finalize();
    let ogg_serial = u32::from_le_bytes([key[0], key[1], key[2], key[3]]);
    let key = key
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let cache_path = cache_enabled().then(|| {
        cache_root
            .join("audio")
            .join(format!("{key}.{}", format.extension()))
    });
    if let Some(path) = &cache_path
        && let Ok(data) = fs::read(path)
        && valid_audio_data(&data, format)
    {
        return Ok(data);
    }

    let output = TempAudioFile::create(&key, format.extension())?;

    let mut command = Command::new(ffmpeg);
    command.arg("-y").arg("-i").arg(&sound.audio_source);
    let channels = if sound.stereo { "2" } else { "1" };
    match format {
        AudioFormat::Wav => {
            command
                .arg("-ac")
                .arg(channels)
                .arg("-ar")
                .arg(sound.sample_rate.to_string())
                .arg("-acodec")
                .arg(if sound.bit_depth == 8 {
                    "pcm_u8"
                } else {
                    "pcm_s16le"
                });
        }
        AudioFormat::Ogg => {
            command
                .arg("-acodec")
                .arg("libvorbis")
                .arg("-ac")
                .arg(channels)
                .arg("-ar")
                .arg(sound.sample_rate.to_string())
                .arg("-aq")
                .arg((sound.bit_rate / 64).min(6).to_string())
                .arg("-loglevel")
                .arg("quiet");
        }
        AudioFormat::Mp3 => {
            command
                .arg("-acodec")
                .arg("libmp3lame")
                .arg("-ac")
                .arg(channels)
                .arg("-ar")
                .arg(sound.sample_rate.to_string())
                .arg("-ab")
                .arg(format!("{}k", sound.bit_rate));
        }
    }
    let result = command
        .arg(&output.path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|source| AudioError::Io {
            path: ffmpeg.to_path_buf(),
            source,
        })?;
    if !result.status.success() {
        return Err(AudioError::ConversionFailed {
            sound: sound.name.clone(),
            status: result.status.code(),
            output: String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        });
    }
    let mut data = fs::read(&output.path).map_err(|source| AudioError::Io {
        path: output.path.clone(),
        source,
    })?;
    if data.is_empty() {
        return Err(AudioError::ConversionFailed {
            sound: sound.name.clone(),
            status: result.status.code(),
            output: "converter produced an empty file".to_owned(),
        });
    }
    if matches!(format, AudioFormat::Ogg) {
        normalize_ogg(&mut data, ogg_serial).map_err(|message| AudioError::ConversionFailed {
            sound: sound.name.clone(),
            status: result.status.code(),
            output: message,
        })?;
    }
    if let Some(path) = cache_path {
        let _ = write_atomic(&path, &data);
    }
    Ok(data)
}

fn add_file_fingerprint(hasher: &mut Md5, path: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        hasher.update(metadata.len().to_le_bytes());
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(UNIX_EPOCH)
        {
            hasher.update(duration.as_secs().to_le_bytes());
            hasher.update(duration.subsec_nanos().to_le_bytes());
        }
    }
    hasher.update([0]);
}

fn valid_audio_data(data: &[u8], format: AudioFormat) -> bool {
    match format {
        AudioFormat::Wav => {
            data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WAVE"
        }
        AudioFormat::Ogg => data.starts_with(b"OggS"),
        AudioFormat::Mp3 => {
            data.starts_with(b"ID3") || data.len() >= 2 && data[0] == 0xff && data[1] & 0xe0 == 0xe0
        }
    }
}

#[derive(Debug)]
struct TempAudioFile {
    path: PathBuf,
}

impl TempAudioFile {
    fn create(key: &str, extension: &str) -> Result<Self, AudioError> {
        let root = env::temp_dir().join("gmx-rs-audio");
        fs::create_dir_all(&root).map_err(|source| AudioError::Io {
            path: root.clone(),
            source,
        })?;
        for _ in 0..100 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = root.join(format!(
                ".{key}.tmp-{}-{sequence}.{extension}",
                std::process::id()
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(source) => return Err(AudioError::Io { path, source }),
            }
        }
        Err(AudioError::TempFileExhausted { root })
    }
}

impl Drop for TempAudioFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn normalize_ogg(data: &mut [u8], serial: u32) -> Result<(), String> {
    let mut offset = 0;
    while offset < data.len() {
        if data.len() - offset < 27 || &data[offset..offset + 4] != b"OggS" {
            return Err(format!("invalid Ogg page at byte {offset}"));
        }
        let segments = usize::from(data[offset + 26]);
        if data.len() - offset < 27 + segments {
            return Err(format!("truncated Ogg segment table at byte {offset}"));
        }
        let body_size = data[offset + 27..offset + 27 + segments]
            .iter()
            .map(|value| usize::from(*value))
            .sum::<usize>();
        let page_size = 27 + segments + body_size;
        if data.len() - offset < page_size {
            return Err(format!("truncated Ogg page at byte {offset}"));
        }
        data[offset + 14..offset + 18].copy_from_slice(&serial.to_le_bytes());
        data[offset + 22..offset + 26].fill(0);
        let checksum = ogg_crc(&data[offset..offset + page_size]);
        data[offset + 22..offset + 26].copy_from_slice(&checksum.to_le_bytes());
        offset += page_size;
    }
    Ok(())
}

fn ogg_crc(bytes: &[u8]) -> u32 {
    let mut crc = 0_u32;
    for byte in bytes {
        crc = (crc << 8) ^ OGG_CRC_TABLE[((crc >> 24) as u8 ^ byte) as usize];
    }
    crc
}

const fn ogg_crc_table() -> [u32; 256] {
    let mut table = [0_u32; 256];
    let mut index = 0;
    while index < table.len() {
        let mut value = (index as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 0x8000_0000 != 0 {
                (value << 1) ^ 0x04c1_1db7
            } else {
                value << 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

fn find_ffmpeg() -> Result<PathBuf, AudioError> {
    let names: &[&str] = if cfg!(windows) {
        &["ffmpeg.exe", "ffmpeg"]
    } else {
        &["ffmpeg"]
    };
    find_tool(names).ok_or(AudioError::MissingConverter)
}

fn culture_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.bytes()
        .map(collation_unit)
        .cmp(right.bytes().map(collation_unit))
        .then_with(|| {
            left.bytes()
                .map(|byte| u8::from(byte.is_ascii_uppercase()))
                .cmp(
                    right
                        .bytes()
                        .map(|byte| u8::from(byte.is_ascii_uppercase())),
                )
        })
        .then_with(|| left.cmp(right))
}

fn collation_unit(byte: u8) -> (u8, u8) {
    if byte.is_ascii_alphabetic() {
        (2, byte.to_ascii_lowercase())
    } else if byte.is_ascii_digit() {
        (1, byte)
    } else {
        (0, byte)
    }
}

fn as_u32(value: usize, field: &'static str) -> Result<u32, WriteError> {
    u32::try_from(value).map_err(|_| WriteError::SizeOverflow {
        field,
        size: value as u64,
    })
}

fn invalid_audio(message: impl Into<String>) -> WriteError {
    WriteError::InvalidVmData {
        message: format!("audio data: {}", message.into()),
    }
}

#[derive(Debug)]
enum AudioError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    MissingSource {
        sound: String,
        path: PathBuf,
    },
    MissingConverter,
    ConversionFailed {
        sound: String,
        status: Option<i32>,
        output: String,
    },
    TempFileExhausted {
        root: PathBuf,
    },
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::MissingSource { sound, path } => {
                write!(
                    formatter,
                    "sound {sound} has no loaded source {}",
                    path.display()
                )
            }
            Self::MissingConverter => {
                formatter.write_str("ffmpeg was not found next to gmx or on PATH")
            }
            Self::ConversionFailed {
                sound,
                status,
                output,
            } => write!(
                formatter,
                "audio conversion failed for {sound} with status {status:?}: {output}"
            ),
            Self::TempFileExhausted { root } => write!(
                formatter,
                "could not create a unique audio conversion file under {}",
                root.display()
            ),
        }
    }
}

impl std::error::Error for AudioError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::io::{Cursor, Read, Seek, SeekFrom};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::{
        AUDO, AudioBlob, AudioData, culture_cmp, normalize_ogg, ogg_crc, unique_default_audio_name,
    };
    use crate::artifact::GeneratedFile;
    use crate::wad::WadFile;

    #[test]
    fn culture_order_puts_identifier_punctuation_before_digits() {
        assert!(culture_cmp("sprBlock_frame", "sprBlock2_frame").is_lt());
        assert!(culture_cmp("sndDeath", "sndDJump").is_lt());
    }

    #[test]
    fn duplicate_names_preserve_the_original_extension() {
        let used = HashSet::from(["sound.wav".to_owned()]);
        assert_eq!(
            unique_default_audio_name("sound.wav".to_owned(), &used),
            "sound_0.wav"
        );
    }

    #[test]
    fn normalizes_ogg_serial_and_recomputes_checksum() {
        let mut page =
            Vec::from(&b"OggS\0\x02\0\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\0\0\0\0\x01\x03abc"[..]);
        normalize_ogg(&mut page, 0x1234_5678).unwrap();
        assert_eq!(&page[14..18], &0x1234_5678_u32.to_le_bytes());
        let stored = u32::from_le_bytes(page[22..26].try_into().unwrap());
        page[22..26].fill(0);
        assert_eq!(stored, ogg_crc(&page));
    }

    #[test]
    fn writes_enabled_audio_groups_as_standalone_wads() {
        let audio = AudioData {
            sounds: Vec::new(),
            groups: vec![
                vec![blob(b"default")],
                vec![blob(b"one"), blob(b"four")],
                vec![blob(b"disabled")],
            ],
            streamed: vec![GeneratedFile::new(
                PathBuf::from("stream.ogg"),
                Vec::from(&b"streamed"[..]),
            )],
            target_masks: vec![i64::MAX, 0b10, 0b01],
            target_mask: 0b10,
        };

        let files = audio.external_files().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, Path::new("stream.ogg"));
        assert_eq!(files[0].data.as_ref(), b"streamed");
        assert_eq!(files[1].path, Path::new("audiogroup1.dat"));
        assert!(
            !files
                .iter()
                .any(|file| file.path == Path::new("audiogroup2.dat"))
        );

        let mut reader = Cursor::new(files[1].data.as_ref());
        let wad = WadFile::read(&mut reader).unwrap();
        assert_eq!(wad.chunks.len(), 1);
        assert_eq!(wad.chunks[0].name, AUDO);
        reader
            .seek(SeekFrom::Start(wad.chunks[0].data_offset))
            .unwrap();
        assert_eq!(read_u32(&mut reader), 2);
        let first = u64::from(read_u32(&mut reader));
        let second = u64::from(read_u32(&mut reader));
        assert_eq!(read_blob(&mut reader, first), b"one");
        assert_eq!(read_blob(&mut reader, second), b"four");
    }

    fn blob(data: &[u8]) -> AudioBlob {
        AudioBlob {
            data: Arc::from(data),
        }
    }

    fn read_blob(reader: &mut Cursor<&[u8]>, offset: u64) -> Vec<u8> {
        reader.seek(SeekFrom::Start(offset)).unwrap();
        let size = usize::try_from(read_u32(reader)).unwrap();
        let mut data = vec![0; size];
        reader.read_exact(&mut data).unwrap();
        data
    }

    fn read_u32(reader: &mut impl Read) -> u32 {
        let mut bytes = [0; 4];
        reader.read_exact(&mut bytes).unwrap();
        u32::from_le_bytes(bytes)
    }
}
