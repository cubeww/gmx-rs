use std::io::{Seek, Write};

use crate::artifact::{GeneratedFile, merge_generated_files};
use crate::assets::Assets;
use crate::audio::AudioData;
use crate::cache::cache_root;
use crate::gml::{CompiledProject, add_vm_chunks, function_classifications};
use crate::package::collect_project_files;
use crate::project::ProjectManifest;
use crate::resource_chunks::add_resource_chunks;
use crate::resources::{Extension, ExtensionFile, ExtensionFunction, Room};
use crate::settings::{CompileConstant, ConstantSource};
use crate::texture::{TextureData, add_texture_chunks};
use crate::wad::{ChunkWriter, FourCc, StringTable, WadBuilder, WadFile, WriteError};

const GEN8: FourCc = FourCc::new(*b"GEN8");
const OPTN: FourCc = FourCc::new(*b"OPTN");
const LANG: FourCc = FourCc::new(*b"LANG");
const EXTN: FourCc = FourCc::new(*b"EXTN");
const STRG: FourCc = FourCc::new(*b"STRG");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildOptions {
    pub debug: bool,
    pub timestamp: i64,
    pub major_version: i32,
    pub minor_version: i32,
    pub release_version: i32,
    pub build_version: i32,
    pub license_crc: i32,
    pub license_md5: [u8; 16],
    pub function_classifications: i64,
    pub steam_app_id: i32,
    pub debugger_port: i32,
    pub steam_project: bool,
    pub ecma_script: bool,
    pub used_constants: Vec<String>,
}

#[derive(Debug)]
pub struct BuildOutput {
    pub wad: WadFile,
    pub external_files: Vec<GeneratedFile>,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            debug: false,
            // A fixed default makes identical inputs reproducible. Callers can
            // supply the current Unix timestamp when official parity matters.
            timestamp: 0,
            major_version: 1,
            minor_version: 0,
            release_version: 0,
            build_version: 9999,
            license_crc: 0,
            license_md5: [0; 16],
            function_classifications: 0,
            steam_app_id: 0,
            debugger_port: 6502,
            steam_project: false,
            ecma_script: false,
            used_constants: Vec::new(),
        }
    }
}

/// Writes the phase-two WAD: the official 1.4 GEN8/OPTN/LANG/EXTN records and
/// their relocated STRG table. Resource, VM, texture, and audio chunks are
/// intentionally added by later compiler phases through the same WadBuilder.
pub fn write_base_wad<W: Write + Seek>(
    project: &ProjectManifest,
    assets: &Assets,
    options: &BuildOptions,
    output: &mut W,
) -> Result<WadFile, WriteError> {
    let strings = StringTable::new();
    let mut builder = WadBuilder::new();
    builder.add_chunk(GEN8, |writer| {
        write_gen8(writer, &strings, project, assets, options)
    })?;
    builder.add_chunk(OPTN, |writer| write_optn(writer, &strings, assets, options))?;
    builder.add_chunk(LANG, write_empty_lang)?;
    builder.add_chunk(EXTN, |writer| write_extn(writer, &strings, assets))?;
    builder.add_chunk(STRG, |writer| strings.write_strg(writer))?;
    builder.write_to(output)
}

/// Writes the current WAD, adding implemented resource chunks and executable
/// GMS 1.4 VM chunks to the phase-two metadata. STRG remains last so names can
/// be relocated and string operands can use the finalized entry order.
pub fn write_vm_wad<W: Write + Seek>(
    project: &ProjectManifest,
    assets: &Assets,
    options: &BuildOptions,
    compiled: &CompiledProject,
    output: &mut W,
) -> Result<WadFile, WriteError> {
    Ok(write_vm_wad_with_artifacts(project, assets, options, compiled, output)?.wad)
}

pub fn write_vm_wad_with_artifacts<W: Write + Seek>(
    project: &ProjectManifest,
    assets: &Assets,
    options: &BuildOptions,
    compiled: &CompiledProject,
    output: &mut W,
) -> Result<BuildOutput, WriteError> {
    let mut effective_options = options.clone();
    effective_options.function_classifications |= function_classifications(compiled);
    let strings = StringTable::new();
    let cache = cache_root(&project.project_file);
    let textures = TextureData::prepare(assets, &cache)?;
    let audio = AudioData::prepare(assets, &cache)?;
    let mut external_files = collect_project_files(project, assets)?;
    external_files.extend(audio.external_files()?);
    let external_files = merge_generated_files(external_files)
        .map_err(|error| invalid_build_output(error.to_string()))?;
    let mut builder = WadBuilder::new();
    builder.add_chunk(GEN8, |writer| {
        write_gen8(writer, &strings, project, assets, &effective_options)
    })?;
    builder.add_chunk(OPTN, |writer| {
        write_optn(writer, &strings, assets, &effective_options)
    })?;
    builder.add_chunk(LANG, write_empty_lang)?;
    builder.add_chunk(EXTN, |writer| write_extn(writer, &strings, assets))?;
    add_resource_chunks(&mut builder, &strings, assets, compiled, &textures, &audio)?;
    add_texture_chunks(&mut builder, &textures)?;
    add_vm_chunks(&mut builder, &strings, compiled)?;
    builder.add_chunk(STRG, |writer| strings.write_strg(writer))?;
    audio.add_chunk(&mut builder)?;
    let wad = builder.write_to(output)?;
    Ok(BuildOutput {
        wad,
        external_files,
    })
}

fn invalid_build_output(message: impl Into<String>) -> WriteError {
    WriteError::InvalidVmData {
        message: format!("build output: {}", message.into()),
    }
}

fn write_gen8(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    project: &ProjectManifest,
    assets: &Assets,
    build: &BuildOptions,
) -> Result<(), WriteError> {
    let options = &assets.settings.options;
    writer.write_i32(i32::from(!build.debug) | 0x1000)?;
    strings.write_reference(writer, &project.name)?;
    strings.write_reference(writer, &options.config)?;
    writer.write_i32(assets.next_room_instance_id)?;
    writer.write_i32(assets.next_room_tile_id)?;
    writer.write_i32(options.game_id)?;
    for _ in 0..4 {
        writer.write_i32(0)?;
    }
    strings.write_reference(writer, &internal_game_name(&project.name))?;
    writer.write_i32(build.major_version)?;
    writer.write_i32(build.minor_version)?;
    writer.write_i32(build.release_version)?;
    writer.write_i32(build.build_version)?;
    let (width, height) = first_room_size(&assets.rooms);
    writer.write_i32(width)?;
    writer.write_i32(height)?;

    let mut flags = options.gen8_flags();
    if build.steam_project {
        flags |= 0x1000;
    }
    if build.ecma_script {
        flags |= 0x8000;
    }
    writer.write_u32(flags)?;
    writer.write_i32(build.license_crc)?;
    writer.write_all(&build.license_md5)?;
    writer.write_i64(build.timestamp)?;
    strings.write_reference(writer, &options.display_name)?;
    writer.write_i64(options.active_targets)?;
    writer.write_i64(if build.ecma_script {
        -1
    } else {
        build.function_classifications
    })?;
    writer.write_i32(build.steam_app_id)?;
    writer.write_i32(build.debugger_port)?;
    let room_count = i32::try_from(assets.rooms.len()).map_err(|_| WriteError::SizeOverflow {
        field: "room order count",
        size: assets.rooms.len() as u64,
    })?;
    writer.write_i32(room_count)?;
    for room in &assets.rooms {
        writer.write_i32(
            i32::try_from(room.index).map_err(|_| WriteError::SizeOverflow {
                field: "room index",
                size: room.index as u64,
            })?,
        )?;
    }
    Ok(())
}

fn first_room_size(rooms: &[Room]) -> (i32, i32) {
    let Some(room) = rooms.first() else {
        return (0, 0);
    };
    let mut left = 0;
    let mut top = 0;
    let mut right = room.width;
    let mut bottom = room.height;
    let mut first = true;
    if room.enable_views {
        for view in room.views.iter().filter(|view| view.visible) {
            if first {
                left = view.x_port;
                top = view.y_port;
                right = view.x_port + view.width_port;
                bottom = view.y_port + view.height_port;
                first = false;
            } else {
                left = left.min(view.x_port);
                top = top.min(view.y_port);
                right = right.max(view.x_port + view.width_port);
                bottom = bottom.max(view.y_port + view.height_port);
            }
        }
    }
    (right - left, bottom - top)
}

fn internal_game_name(project_name: &str) -> String {
    // The IDE passes GMAssetCompiler's --gamename value with spaces replaced
    // by underscores, while the first GEN8 name keeps the project filename.
    project_name.replace(' ', "_")
}

fn write_optn(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    assets: &Assets,
    build: &BuildOptions,
) -> Result<(), WriteError> {
    let options = &assets.settings.options;
    writer.write_i32(i32::MIN)?;
    writer.write_i32(2)?;
    writer.write_u64(options.optn_flags())?;
    writer.write_i32(options.scale)?;
    writer.write_i32(options.window_color)?;
    writer.write_i32(options.color_depth)?;
    writer.write_i32(options.resolution)?;
    writer.write_i32(options.frequency)?;
    writer.write_i32(options.sync_vertex)?;
    writer.write_i32(options.priority)?;
    // Splash images are texture-page references and are populated by phase 5.
    writer.write_u32(0)?;
    writer.write_u32(0)?;
    writer.write_u32(0)?;
    writer.write_i32(options.load_alpha)?;

    let used = used_option_constants(&assets.settings.constants, &build.used_constants);
    let count = used.len() + 2;
    writer.write_u32(u32::try_from(count).map_err(|_| WriteError::SizeOverflow {
        field: "option constant count",
        size: count as u64,
    })?)?;
    for constant in used {
        strings.write_reference(writer, &constant.name)?;
        strings.write_reference(writer, &constant.value)?;
    }
    strings.write_reference(writer, "@@SleepMargin")?;
    strings.write_reference(writer, &options.sleep_margin.to_string())?;
    strings.write_reference(writer, "@@DrawColour")?;
    strings.write_reference(writer, &options.draw_color.to_string())?;
    Ok(())
}

fn used_option_constants<'a>(
    constants: &'a [CompileConstant],
    used_names: &[String],
) -> Vec<&'a CompileConstant> {
    constants
        .iter()
        .filter(|constant| constant.source != ConstantSource::Extension)
        .filter(|constant| used_names.iter().any(|name| name == &constant.name))
        .collect()
}

fn write_empty_lang(writer: &mut ChunkWriter<'_>) -> Result<(), WriteError> {
    writer.write_i32(1)?;
    writer.write_i32(0)?;
    writer.write_i32(0)
}

fn write_extn(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    assets: &Assets,
) -> Result<(), WriteError> {
    let config = &assets.settings.options.config;
    let target_mask = assets.settings.target_mask;
    let used: Vec<usize> = assets
        .extensions
        .iter()
        .enumerate()
        .filter(|(_, extension)| extension.used_for(config, target_mask))
        .map(|(index, _)| index)
        .collect();

    writer.write_offset_table(&used, |writer, extension_index| {
        let extension = &assets.extensions[*extension_index];
        write_extension_record(writer, strings, assets, extension)
    })?;

    let seed = [
        0, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141, 142, 143,
    ];
    let mut chain = seed;
    for extension_index in used {
        let extension = &assets.extensions[extension_index];
        if let Some(mut product) = decode_product_id(&extension.product_id) {
            for (byte, previous) in product.iter_mut().zip(chain) {
                *byte ^= previous;
            }
            writer.write_all(&product)?;
            chain = product;
        } else {
            writer.write_all(&seed)?;
        }
    }
    Ok(())
}

fn write_extension_record(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    assets: &Assets,
    extension: &Extension,
) -> Result<(), WriteError> {
    strings.write_reference(writer, "")?;
    strings.write_reference(writer, &extension.name)?;
    strings.write_reference(writer, &extension.class_name)?;
    let config = &assets.settings.options.config;
    let target_mask = assets.settings.target_mask;
    let files: Vec<usize> = extension
        .files
        .iter()
        .enumerate()
        .filter(|(_, file)| file.used_for(config, target_mask))
        .map(|(index, _)| index)
        .collect();
    writer.write_offset_table(&files, |writer, file_index| {
        write_extension_file(writer, strings, assets, &extension.files[*file_index])
    })
}

fn write_extension_file(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    assets: &Assets,
    file: &ExtensionFile,
) -> Result<(), WriteError> {
    strings.write_reference(
        writer,
        file.filename_for_target(assets.settings.target_mask),
    )?;
    strings.write_reference(
        writer,
        translate_extension_function(assets, &file.finalizer),
    )?;
    strings.write_reference(writer, translate_extension_function(assets, &file.init))?;
    writer.write_i32(file.kind)?;
    if file.kind == 2 {
        writer.write_u32(0)
    } else {
        writer.write_offset_table(&file.functions, |writer, function| {
            write_extension_function(writer, strings, function)
        })
    }
}

fn translate_extension_function<'a>(assets: &'a Assets, name: &'a str) -> &'a str {
    if name.is_empty() {
        return name;
    }
    assets.extension_function(name).map_or(name, |function| {
        if function.external_name.is_empty() {
            &function.name
        } else {
            &function.external_name
        }
    })
}

fn write_extension_function(
    writer: &mut ChunkWriter<'_>,
    strings: &StringTable,
    function: &ExtensionFunction,
) -> Result<(), WriteError> {
    strings.write_reference(writer, &function.name)?;
    writer.write_i32(function.id)?;
    writer.write_i32(function.kind)?;
    writer.write_i32(function.return_type)?;
    strings.write_reference(
        writer,
        if function.external_name.is_empty() {
            &function.name
        } else {
            &function.external_name
        },
    )?;
    writer.write_u32(u32::try_from(function.arguments.len()).map_err(|_| {
        WriteError::SizeOverflow {
            field: "extension function argument count",
            size: function.arguments.len() as u64,
        }
    })?)?;
    for argument in &function.arguments {
        writer.write_i32(*argument)?;
    }
    Ok(())
}

fn decode_product_id(value: &str) -> Option<[u8; 16]> {
    if value.len() != 32 {
        return None;
    }
    let mut output = [0; 16];
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::assets::Assets;
    use crate::cache::build_dependencies;
    use crate::config::Config;
    use crate::gml::{analyze_assets, compile_vm};
    use crate::project::ProjectManifest;
    use crate::wad::{FourCc, WadFile};

    use super::{BuildOptions, decode_product_id, write_base_wad, write_vm_wad_with_artifacts};

    #[test]
    fn writes_parseable_base_wad_with_official_chunk_order() {
        let root = temp_dir("base-wad");
        fs::create_dir_all(root.join("Configs")).unwrap();
        let project_path = root.join("Tiny.project.gmx");
        fs::write(
            &project_path,
            "<assets><Configs><Config>Configs\\Default</Config></Configs><datafiles><datafile><name>included.bin</name><exists>-1</exists><size>3</size><filename>included.bin</filename></datafile></datafiles></assets>",
        )
        .unwrap();
        fs::write(root.join("included.bin"), b"bin").unwrap();
        fs::write(
            root.join("Configs/Default.config.gmx"),
            "<Config><Options><option_gameid>42</option_gameid><option_display_name>Tiny Game</option_display_name><option_use_new_audio>true</option_use_new_audio></Options></Config>",
        )
        .unwrap();

        let project = ProjectManifest::load(&project_path).unwrap();
        let config = Config::load_from_project(&project, "Default").unwrap();
        let assets = Assets::load(&project, &config).unwrap();
        assert_eq!(
            assets.binary(&root.join("included.bin")),
            Some(b"bin".as_slice())
        );
        let mut output = Cursor::new(Vec::new());
        let written =
            write_base_wad(&project, &assets, &BuildOptions::default(), &mut output).unwrap();
        output.set_position(0);
        let parsed = WadFile::read(&mut output).unwrap();
        assert_eq!(written, parsed);
        assert_eq!(
            parsed
                .chunks
                .iter()
                .map(|chunk| chunk.name)
                .collect::<Vec<_>>(),
            [
                FourCc::new(*b"GEN8"),
                FourCc::new(*b"OPTN"),
                FourCc::new(*b"LANG"),
                FourCc::new(*b"EXTN"),
                FourCc::new(*b"STRG"),
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_background_image_writes_a_null_texture_reference() {
        let root = temp_dir("missing-background");
        fs::create_dir_all(root.join("Configs")).unwrap();
        fs::create_dir_all(root.join("backgrounds")).unwrap();
        let project_path = root.join("Missing Background.project.gmx");
        fs::write(
            &project_path,
            r#"<assets>
                <Configs><Config>Configs\Default</Config></Configs>
                <backgrounds><background>backgrounds\Missing</background></backgrounds>
            </assets>"#,
        )
        .unwrap();
        fs::write(
            root.join("Configs/Default.config.gmx"),
            "<Config><Options/></Config>",
        )
        .unwrap();
        fs::write(
            root.join("backgrounds/Missing.background.gmx"),
            "<background><data>images\\missing.png</data></background>",
        )
        .unwrap();

        let project = ProjectManifest::load(&project_path).unwrap();
        let config = Config::load_from_project(&project, "Default").unwrap();
        let assets = Assets::load(&project, &config).unwrap();
        let missing_image = root.join("backgrounds/images/missing.png");
        assert_eq!(assets.binary(&missing_image), None);
        assert!(build_dependencies(&project, &config, &assets).contains(&missing_image));

        let analysis = analyze_assets(&assets).unwrap();
        let compiled = compile_vm(&assets, &analysis).unwrap();
        let mut output = Cursor::new(Vec::new());
        let built = write_vm_wad_with_artifacts(
            &project,
            &assets,
            &BuildOptions::default(),
            &compiled,
            &mut output,
        )
        .unwrap();
        let bgnd = built
            .wad
            .chunks
            .iter()
            .find(|chunk| chunk.name == FourCc::new(*b"BGND"))
            .unwrap();
        let tpags = built
            .wad
            .chunks
            .iter()
            .find(|chunk| chunk.name == FourCc::new(*b"TPAG"))
            .unwrap();
        let bytes = output.get_ref();
        assert_eq!(u32_at(bytes, bgnd.data_offset as usize), 1);
        let record = u32_at(bytes, bgnd.data_offset as usize + 4) as usize;
        assert_eq!(u32_at(bytes, record + 16), 0);
        assert_eq!(u32_at(bytes, tpags.data_offset as usize), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn product_ids_are_exactly_sixteen_hex_bytes() {
        assert_eq!(
            decode_product_id("000102030405060708090A0B0C0D0E0F"),
            Some([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15])
        );
        assert_eq!(decode_product_id("not-a-product-id"), None);
    }

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("gmx-rs-{label}-{}-{nonce}", std::process::id()))
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }
}
