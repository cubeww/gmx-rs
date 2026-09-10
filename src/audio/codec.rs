use std::io::Cursor;
use std::path::Path;

use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use oxideav_vorbis::{StreamEncoderConfig, encode_pcm_to_ogg};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use rusty_mp3::{Error as Mp3Error, Mp3Encoder, Mp3EncoderConfig};
use rusty_vorbis::{Error as VorbisError, VorbisEncoder, VorbisEncoderConfig};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::audio::well_known::CODEC_ID_VORBIS;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use super::AudioFormat;

const RESAMPLE_CHUNK_FRAMES: usize = 1024;
// rusty_vorbis 0.1 uses a fixed 2048-frame profile with 50% overlap.
const PROFILED_VORBIS_HOP_FRAMES: usize = 1024;

pub(super) struct TranscodeSettings {
    pub(super) format: AudioFormat,
    pub(super) sample_rate: u32,
    pub(super) channels: usize,
    pub(super) bit_depth: u16,
    pub(super) bit_rate: u32,
    pub(super) ogg_serial: u32,
}

struct DecodedAudio {
    samples: Vec<f32>,
    sample_rate: u32,
    channels: usize,
}

pub(super) fn is_runner_compatible_ogg(source: &[u8]) -> bool {
    if !source.starts_with(b"OggS") {
        return false;
    }

    let stream = MediaSourceStream::new(Box::new(Cursor::new(source.to_vec())), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("ogg");
    let Ok(mut format) = symphonia::default::get_probe().probe(
        &hint,
        stream,
        FormatOptions::default(),
        MetadataOptions::default(),
    ) else {
        return false;
    };
    let Some(track) = format.default_track(TrackType::Audio) else {
        return false;
    };
    let track_id = track.id;
    let Some(parameters) = track
        .codec_params
        .as_ref()
        .and_then(|parameters| parameters.audio())
    else {
        return false;
    };
    if parameters.codec != CODEC_ID_VORBIS {
        return false;
    }
    let Ok(mut decoder) = symphonia::default::get_codecs()
        .make_audio_decoder(parameters, &AudioDecoderOptions::default())
    else {
        return false;
    };

    loop {
        let Ok(packet) = format.next_packet() else {
            return false;
        };
        let Some(packet) = packet else {
            return false;
        };
        if packet.track_id != track_id {
            continue;
        }
        let Ok(decoded) = decoder.decode(&packet) else {
            return false;
        };
        let channels = decoded.spec().channels().count();
        if decoded.spec().rate() == 0 || !matches!(channels, 1 | 2) {
            return false;
        }
        if decoded.samples_interleaved() > 0 {
            return true;
        }
    }
}

pub(super) fn transcode(
    source: &[u8],
    source_path: &Path,
    settings: TranscodeSettings,
) -> Result<Vec<u8>, String> {
    if settings.sample_rate == 0 {
        return Err("the output sample rate must be greater than zero".to_owned());
    }
    if !matches!(settings.channels, 1 | 2) {
        return Err(format!(
            "unsupported output channel count {}",
            settings.channels
        ));
    }

    let decoded = decode(source, source_path)?;
    let remixed = remix_channels(&decoded.samples, decoded.channels, settings.channels);
    let samples = resample(
        &remixed,
        settings.channels,
        decoded.sample_rate,
        settings.sample_rate,
    )?;
    if samples.is_empty() {
        return Err("the decoded audio contains no samples".to_owned());
    }

    match settings.format {
        AudioFormat::Wav => encode_wav(
            &samples,
            settings.channels,
            settings.sample_rate,
            settings.bit_depth,
        ),
        AudioFormat::Ogg => encode_ogg(
            &samples,
            settings.channels,
            settings.sample_rate,
            settings.bit_rate,
            settings.ogg_serial,
        ),
        AudioFormat::Mp3 => encode_mp3(
            &samples,
            settings.channels,
            settings.sample_rate,
            settings.bit_rate,
        ),
    }
}

fn decode(source: &[u8], source_path: &Path) -> Result<DecodedAudio, String> {
    let stream = MediaSourceStream::new(Box::new(Cursor::new(source.to_vec())), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = source_path.extension().and_then(|value| value.to_str()) {
        hint.with_extension(extension);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| {
            format!(
                "could not recognize audio source {}: {error}",
                source_path.display()
            )
        })?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| format!("{} has no audio track", source_path.display()))?;
    let track_id = track.id;
    let parameters = track
        .codec_params
        .as_ref()
        .and_then(|parameters| parameters.audio())
        .ok_or_else(|| format!("{} has no audio codec parameters", source_path.display()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(parameters, &AudioDecoderOptions::default())
        .map_err(|error| {
            format!(
                "unsupported audio codec in {}: {error}",
                source_path.display()
            )
        })?;

    let mut samples = Vec::new();
    let mut stream_spec = None;
    loop {
        let packet = format
            .next_packet()
            .map_err(|error| format!("could not read {}: {error}", source_path.display()))?;
        let Some(packet) = packet else {
            break;
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = decoder.decode(&packet).map_err(|error| {
            format!(
                "could not decode audio in {}: {error}",
                source_path.display()
            )
        })?;
        let rate = decoded.spec().rate();
        let channels = decoded.spec().channels().count();
        if channels == 0 || rate == 0 {
            return Err(format!(
                "{} has an invalid audio stream",
                source_path.display()
            ));
        }
        match stream_spec {
            Some((expected_rate, expected_channels))
                if expected_rate != rate || expected_channels != channels =>
            {
                return Err(format!(
                    "{} changes audio format partway through the stream",
                    source_path.display()
                ));
            }
            None => stream_spec = Some((rate, channels)),
            _ => {}
        }

        let start = samples.len();
        samples.resize(start + decoded.samples_interleaved(), 0.0);
        decoded.copy_to_slice_interleaved(&mut samples[start..]);
    }

    let (sample_rate, channels) = stream_spec
        .ok_or_else(|| format!("{} contains no decoded audio", source_path.display()))?;
    Ok(DecodedAudio {
        samples,
        sample_rate,
        channels,
    })
}

fn remix_channels(samples: &[f32], source_channels: usize, target_channels: usize) -> Vec<f32> {
    if source_channels == target_channels {
        return samples.to_vec();
    }
    let frames = samples.len() / source_channels;
    let mut output = Vec::with_capacity(frames * target_channels);
    for frame in samples.chunks_exact(source_channels) {
        match target_channels {
            1 => {
                let sum = frame.iter().copied().sum::<f32>();
                output.push(sum / source_channels as f32);
            }
            2 if source_channels == 1 => {
                output.extend_from_slice(&[frame[0], frame[0]]);
            }
            2 => {
                output.extend_from_slice(&frame[..2]);
            }
            _ => unreachable!("the target channel count is validated before remixing"),
        }
    }
    output
}

fn resample(
    samples: &[f32],
    channels: usize,
    source_rate: u32,
    target_rate: u32,
) -> Result<Vec<f32>, String> {
    if source_rate == target_rate {
        return Ok(samples.to_vec());
    }
    let frames = samples.len() / channels;
    let input = InterleavedSlice::new(samples, channels, frames)
        .map_err(|error| format!("invalid resampler input: {error}"))?;
    let mut resampler = Fft::<f32>::new(
        source_rate as usize,
        target_rate as usize,
        RESAMPLE_CHUNK_FRAMES,
        channels,
        FixedSync::Both,
    )
    .map_err(|error| format!("could not configure resampling: {error}"))?;
    resampler
        .process_all(&input, frames, None)
        .map(|output| output.take_data())
        .map_err(|error| format!("could not resample audio: {error}"))
}

fn encode_wav(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    bit_depth: u16,
) -> Result<Vec<u8>, String> {
    let bit_depth = if bit_depth == 8 { 8_u16 } else { 16_u16 };
    let bytes_per_sample = usize::from(bit_depth / 8);
    let data_size = samples
        .len()
        .checked_mul(bytes_per_sample)
        .and_then(|size| u32::try_from(size).ok())
        .ok_or_else(|| "WAV data is too large".to_owned())?;
    let riff_size = 36_u32
        .checked_add(data_size)
        .ok_or_else(|| "WAV data is too large".to_owned())?;
    let channels = u16::try_from(channels).map_err(|_| "too many WAV channels".to_owned())?;
    let block_align = channels
        .checked_mul(bit_depth / 8)
        .ok_or_else(|| "invalid WAV block alignment".to_owned())?;
    let byte_rate = sample_rate
        .checked_mul(u32::from(block_align))
        .ok_or_else(|| "invalid WAV byte rate".to_owned())?;

    let mut output = Vec::with_capacity(44 + data_size as usize);
    output.extend_from_slice(b"RIFF");
    output.extend_from_slice(&riff_size.to_le_bytes());
    output.extend_from_slice(b"WAVEfmt ");
    output.extend_from_slice(&16_u32.to_le_bytes());
    output.extend_from_slice(&1_u16.to_le_bytes());
    output.extend_from_slice(&channels.to_le_bytes());
    output.extend_from_slice(&sample_rate.to_le_bytes());
    output.extend_from_slice(&byte_rate.to_le_bytes());
    output.extend_from_slice(&block_align.to_le_bytes());
    output.extend_from_slice(&bit_depth.to_le_bytes());
    output.extend_from_slice(b"data");
    output.extend_from_slice(&data_size.to_le_bytes());
    if bit_depth == 8 {
        output.extend(samples.iter().map(|sample| {
            (finite_sample(*sample) * 128.0 + 128.0)
                .round()
                .clamp(0.0, 255.0) as u8
        }));
    } else {
        for sample in samples {
            let value = (finite_sample(*sample) * 32_768.0)
                .round()
                .clamp(-32_768.0, 32_767.0) as i16;
            output.extend_from_slice(&value.to_le_bytes());
        }
    }
    Ok(output)
}

fn encode_ogg(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    bit_rate: u32,
    serial: u32,
) -> Result<Vec<u8>, String> {
    if channels == 2 {
        match encode_profiled_ogg(samples, channels, sample_rate, bit_rate, serial) {
            Ok(output) => return Ok(output),
            Err(fast_error) => {
                return encode_general_ogg(samples, channels, sample_rate, bit_rate, serial)
                    .map_err(|fallback_error| {
                        format!("{fast_error}; fallback encoder failed: {fallback_error}")
                    });
            }
        }
    }
    encode_general_ogg(samples, channels, sample_rate, bit_rate, serial)
}

fn encode_profiled_ogg(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    bit_rate: u32,
    serial: u32,
) -> Result<Vec<u8>, String> {
    let bitrate_bps = bit_rate
        .checked_mul(1000)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| "Vorbis bitrate is too large".to_owned())?;
    let mut encoder = VorbisEncoder::new(VorbisEncoderConfig {
        bitrate_bps,
        quality: vorbis_quality(bit_rate),
    });
    let channels_u16 =
        u16::try_from(channels).map_err(|_| "too many Vorbis channels".to_owned())?;
    // The first Vorbis packet only primes overlap-add. Add one silent hop at
    // each edge so short effects retain their beginning and end, then expose
    // granule positions on the original (unpadded) timeline below.
    let padding = vec![0.0; PROFILED_VORBIS_HOP_FRAMES * channels];
    encoder
        .push_pcm_f32(&padding, channels_u16, sample_rate)
        .map_err(|error| format!("Vorbis encoding failed: {error}"))?;
    encoder
        .push_pcm_f32(samples, channels_u16, sample_rate)
        .map_err(|error| format!("Vorbis encoding failed: {error}"))?;
    encoder
        .push_pcm_f32(&padding, channels_u16, sample_rate)
        .map_err(|error| format!("Vorbis encoding failed: {error}"))?;
    encoder.finish();

    let mut packets = Vec::new();
    loop {
        match encoder.next_packet() {
            Ok(packet) => packets.push(packet),
            Err(VorbisError::Eof) => break,
            Err(error) => return Err(format!("Vorbis encoding failed: {error}")),
        }
    }
    if packets.len() < 4 {
        return Err("Vorbis encoder produced an incomplete stream".to_owned());
    }

    let packet_count = packets.len();
    let source_frames = u64::try_from(samples.len() / channels)
        .map_err(|_| "Vorbis input is too large".to_owned())?;
    let mut writer = PacketWriter::new(Vec::new());
    for (index, packet) in packets.into_iter().enumerate() {
        let end = if index == 0 || index == 2 {
            PacketWriteEndInfo::EndPage
        } else if index + 1 == packet_count {
            PacketWriteEndInfo::EndStream
        } else {
            PacketWriteEndInfo::NormalPacket
        };
        let granule = u64::try_from(packet.pts)
            .map_err(|_| "Vorbis encoder produced a negative granule position".to_owned())?;
        let granule = if index < 3 {
            granule
        } else {
            granule
                .saturating_sub(PROFILED_VORBIS_HOP_FRAMES as u64)
                .min(source_frames)
        };
        writer
            .write_packet(packet.data, serial, end, granule)
            .map_err(|error| format!("Ogg muxing failed: {error}"))?;
    }
    Ok(writer.into_inner())
}

fn encode_general_ogg(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    bit_rate: u32,
    serial: u32,
) -> Result<Vec<u8>, String> {
    let frames = samples.len() / channels;
    let mut planar = (0..channels)
        .map(|_| Vec::with_capacity(frames))
        .collect::<Vec<_>>();
    for frame in samples.chunks_exact(channels) {
        for (channel, sample) in frame.iter().enumerate() {
            planar[channel].push(finite_sample(*sample));
        }
    }

    let mut config = StreamEncoderConfig::new(
        sample_rate,
        u8::try_from(channels).map_err(|_| "too many Vorbis channels".to_owned())?,
    );
    config.quality = vorbis_quality(bit_rate);
    config.serial = serial;
    config.training_iterations = 0;
    encode_pcm_to_ogg(&planar, &config).map_err(|error| format!("Vorbis encoding failed: {error}"))
}

fn vorbis_quality(bit_rate: u32) -> f32 {
    // GameMaker's legacy setting advances in 64-kbit/s steps. The pure-Rust
    // encoders' normalized quality scale is substantially more aggressive
    // than libvorbis' q scale below about 0.7, so map the useful range onto a
    // calibrated 0.70..=0.98 interval instead of treating both scales alike.
    let legacy_step = (bit_rate / 64).min(6) as f32;
    (0.70 + legacy_step * 0.05).min(0.98)
}

fn encode_mp3(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    bit_rate: u32,
) -> Result<Vec<u8>, String> {
    let mut encoder = Mp3Encoder::new(Mp3EncoderConfig {
        bitrate_kbps: bit_rate,
        vbr_quality: None,
    });
    encoder
        .push_pcm_f32(
            samples,
            u16::try_from(channels).map_err(|_| "too many MP3 channels".to_owned())?,
            sample_rate,
        )
        .map_err(|error| format!("MP3 encoding failed: {error}"))?;
    encoder.finish();

    let mut output = Vec::new();
    loop {
        match encoder.next_packet() {
            Ok(packet) => output.extend_from_slice(&packet),
            Err(Mp3Error::Eof) => break,
            Err(error) => return Err(format!("MP3 encoding failed: {error}")),
        }
    }
    if output.is_empty() {
        return Err("MP3 encoder produced an empty stream".to_owned());
    }
    Ok(output)
}

fn finite_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_standard_pcm_wav_headers() {
        let wav = encode_wav(&[-1.0, 0.0, 1.0], 1, 22_050, 16).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 22_050);
        assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 6);
    }

    #[test]
    fn remixes_mono_and_stereo() {
        assert_eq!(
            remix_channels(&[0.25, -0.5], 1, 2),
            [0.25, 0.25, -0.5, -0.5]
        );
        assert_eq!(remix_channels(&[1.0, -1.0, 0.5, 0.5], 2, 1), [0.0, 0.5]);
    }

    #[test]
    fn pure_rust_encoders_produce_decodable_streams() {
        let sample_rate = 44_100;
        let samples = (0..4096)
            .map(|index| {
                (2.0 * std::f32::consts::PI * 440.0 * index as f32 / sample_rate as f32).sin()
                    * 0.25
            })
            .collect::<Vec<_>>();

        let ogg = encode_ogg(&samples, 1, sample_rate, 192, 0x1234_5678).unwrap();
        assert!(ogg.starts_with(b"OggS"));
        let decoded_ogg = decode(&ogg, Path::new("tone.ogg")).unwrap();
        assert_eq!(decoded_ogg.sample_rate, sample_rate);
        assert_eq!(decoded_ogg.channels, 1);
        assert_eq!(decoded_ogg.samples.len(), samples.len());
        assert!(correlation(&samples, &decoded_ogg.samples) > 0.8);

        let stereo = samples
            .iter()
            .flat_map(|sample| [*sample, *sample])
            .collect::<Vec<_>>();
        let ogg = encode_ogg(&stereo, 2, sample_rate, 192, 0x8765_4321).unwrap();
        let decoded_ogg = decode(&ogg, Path::new("stereo.ogg")).unwrap();
        assert_eq!(decoded_ogg.sample_rate, sample_rate);
        assert_eq!(decoded_ogg.channels, 2);
        assert_eq!(decoded_ogg.samples.len(), stereo.len());
        let decoded_left = decoded_ogg
            .samples
            .chunks_exact(2)
            .map(|frame| frame[0])
            .collect::<Vec<_>>();
        assert!(correlation(&samples, &decoded_left) > 0.8);

        let mp3 = encode_mp3(&samples, 1, sample_rate, 192).unwrap();
        assert!(mp3.len() > 4);
        let decoded_mp3 = decode(&mp3, Path::new("tone.mp3")).unwrap();
        assert_eq!(decoded_mp3.sample_rate, sample_rate);
        assert_eq!(decoded_mp3.channels, 1);
        assert!(!decoded_mp3.samples.is_empty());
    }

    #[test]
    fn recognizes_only_playable_mono_or_stereo_vorbis_streams() {
        let sample_rate = 44_100;
        let mono = (0..4096)
            .map(|index| {
                (2.0 * std::f32::consts::PI * 440.0 * index as f32 / sample_rate as f32).sin()
                    * 0.25
            })
            .collect::<Vec<_>>();
        let stereo = mono
            .iter()
            .flat_map(|sample| [*sample, *sample])
            .collect::<Vec<_>>();

        let mono_ogg = encode_ogg(&mono, 1, sample_rate, 192, 1).unwrap();
        let stereo_ogg = encode_ogg(&stereo, 2, sample_rate, 192, 2).unwrap();
        assert!(is_runner_compatible_ogg(&mono_ogg));
        assert!(is_runner_compatible_ogg(&stereo_ogg));
        assert!(!is_runner_compatible_ogg(b"OggS"));
        assert!(!is_runner_compatible_ogg(b"RIFFnot-an-ogg-stream"));
    }

    #[test]
    fn legacy_bitrate_uses_the_calibrated_vorbis_quality_range() {
        assert!((vorbis_quality(0) - 0.70).abs() < f32::EPSILON);
        assert!((vorbis_quality(64) - 0.75).abs() < f32::EPSILON);
        assert!((vorbis_quality(192) - 0.85).abs() < f32::EPSILON);
        assert!((vorbis_quality(384) - 0.98).abs() < f32::EPSILON);
        assert!((vorbis_quality(u32::MAX) - 0.98).abs() < f32::EPSILON);
    }

    #[test]
    fn transcodes_wav_with_resampling_and_channel_remixing() {
        let source_rate = 48_000;
        let source_frames = 4_800;
        let samples = (0..source_frames)
            .map(|index| {
                (2.0 * std::f32::consts::PI * 440.0 * index as f32 / source_rate as f32).sin()
                    * 0.25
            })
            .collect::<Vec<_>>();
        let wav = encode_wav(&samples, 1, source_rate, 16).unwrap();

        let ogg = transcode(
            &wav,
            Path::new("tone.wav"),
            TranscodeSettings {
                format: AudioFormat::Ogg,
                sample_rate: 24_000,
                channels: 2,
                bit_depth: 16,
                bit_rate: 192,
                ogg_serial: 0x1234_5678,
            },
        )
        .unwrap();
        let decoded = decode(&ogg, Path::new("tone.ogg")).unwrap();

        assert_eq!(decoded.sample_rate, 24_000);
        assert_eq!(decoded.channels, 2);
        assert_eq!(decoded.samples.len() / decoded.channels, 2_400);
    }

    fn correlation(left: &[f32], right: &[f32]) -> f32 {
        assert_eq!(left.len(), right.len());
        let dot = left
            .iter()
            .zip(right)
            .map(|(left, right)| left * right)
            .sum::<f32>();
        let left_energy = left.iter().map(|sample| sample * sample).sum::<f32>();
        let right_energy = right.iter().map(|sample| sample * sample).sum::<f32>();
        dot / (left_energy * right_energy).sqrt()
    }
}
