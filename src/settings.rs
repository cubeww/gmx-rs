use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use crate::config::Config;
use crate::project::ProjectManifest;
use crate::resources::{Extension, Room, Sound};

/// The target mask used by the standalone GMS 1.4 Windows compiler when no
/// narrower mask is supplied on its command line.
pub const DEFAULT_TARGET_MASK: i64 = i32::MAX as i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstantSource {
    RoomInstance,
    Project,
    Config,
    Extension,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileConstant {
    pub name: String,
    pub value: String,
    pub source: ConstantSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioGroupSettings {
    pub index: usize,
    pub name: String,
    pub target_mask: i64,
    pub implicit: bool,
    pub sounds: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureGroupSettings {
    pub index: usize,
    pub name: String,
    pub scaled: bool,
    pub border: i32,
    pub remove_space: bool,
    pub target_mask: i64,
    pub parent: Option<String>,
    pub mips_to_generate: i32,
}

/// The subset of the selected GMX configuration consumed by the GMS 1.4 WAD
/// header and options chunks. Values use the same defaults and legacy Windows
/// key mapping as `GMOptions.SetFromConfig`.
#[derive(Debug, Clone, PartialEq)]
pub struct GameOptions {
    pub config: String,
    pub game_id: i32,
    pub game_guid: String,
    pub display_name: String,
    pub active_targets: i64,
    pub full_screen: bool,
    pub borderless: bool,
    pub interpolate_pixels: bool,
    pub use_new_audio: bool,
    pub no_border: bool,
    pub show_cursor: bool,
    pub scale: i32,
    pub sizeable: bool,
    pub stay_on_top: bool,
    pub window_color: i32,
    pub change_resolution: bool,
    pub color_depth: i32,
    pub resolution: i32,
    pub frequency: i32,
    pub sleep_margin: i32,
    pub no_buttons: bool,
    pub sync_vertex: i32,
    pub screen_key: bool,
    pub help_key: bool,
    pub quit_key: bool,
    pub save_key: bool,
    pub screenshot_key: bool,
    pub close_escape: bool,
    pub priority: i32,
    pub freeze: bool,
    pub show_progress: bool,
    pub load_transparent: bool,
    pub load_alpha: i32,
    pub scale_progress: bool,
    pub display_errors: bool,
    pub write_errors: bool,
    pub abort_errors: bool,
    pub variable_errors: bool,
    pub creation_event_order: bool,
    pub use_front_touch: bool,
    pub use_rear_touch: bool,
    pub use_fast_collision: bool,
    pub fast_collision_compatibility: bool,
    pub save_location: i32,
    pub studio_edition: i32,
    pub draw_color: u32,
    pub game_speed: f64,
    pub allow_statistics: bool,
}

impl GameOptions {
    fn from_config(project: &ProjectManifest, config: &Config) -> Result<Self, SettingsError> {
        // GMS 1.4's older GMX format uses generic Windows keys. Later 1.4
        // projects use the `option_windows_*` names that originated in Zeus.
        let zeus = config.option("option_windows_start_fullscreen").is_some();
        let full_screen_key = if zeus {
            "option_windows_start_fullscreen"
        } else {
            "option_fullscreen"
        };
        let borderless_key = if zeus {
            "option_windows_borderless"
        } else {
            "option_borderless"
        };
        let interpolate_key = if zeus {
            "option_windows_interpolate_pixels"
        } else {
            "option_interpolate"
        };
        let show_cursor_key = if zeus {
            "option_windows_display_cursor"
        } else {
            "option_showcursor"
        };
        let sizeable_key = if zeus {
            "option_windows_resize_window"
        } else {
            "option_sizeable"
        };
        let sync_key = if zeus {
            "option_windows_vsync"
        } else {
            "option_sync_vertex"
        };
        let screen_key = if zeus {
            "option_windows_allow_fullscreen_switching"
        } else {
            "option_screenkey"
        };

        let mut scale = int_option(
            config,
            if zeus {
                "option_windows_scale"
            } else {
                "option_scale"
            },
            0,
        )?;
        if zeus {
            // This intentionally mirrors the official conversion: the editor
            // stores a stretch boolean while the runner stores 0/1.
            scale = i32::from(scale == 0);
        }

        let display_name = config
            .option(if zeus {
                "option_windows_display_name"
            } else {
                "option_display_name"
            })
            .or_else(|| config.option("option_display_name"))
            .filter(|value| !value.is_empty())
            .unwrap_or(&project.name)
            .to_owned();

        Ok(Self {
            config: config.name.clone(),
            game_id: int_option(config, "option_gameid", 0)?,
            game_guid: config.option("option_gameguid").unwrap_or("").to_owned(),
            display_name,
            active_targets: 0,
            full_screen: bool_option(config, full_screen_key, false)?,
            borderless: bool_option(config, borderless_key, false)?,
            interpolate_pixels: bool_option(config, interpolate_key, false)?,
            use_new_audio: bool_option(config, "option_use_new_audio", true)?,
            no_border: bool_option(config, "option_noborder", false)?,
            show_cursor: bool_option(config, show_cursor_key, false)?,
            scale,
            sizeable: bool_option(config, sizeable_key, false)?,
            stay_on_top: bool_option(config, "option_stayontop", false)?,
            window_color: 0,
            change_resolution: bool_option(config, "option_changeresolution", false)?,
            color_depth: int_option(config, "option_colordepth", 0)?,
            resolution: int_option(config, "option_resolution", 0)?,
            frequency: int_option(config, "option_frequency", 0)?,
            sleep_margin: int_option(config, "option_windows_sleep_margin", 0)?,
            no_buttons: bool_option(config, "option_nobuttons", false)?,
            sync_vertex: int_or_bool_option(config, sync_key, 0)?,
            screen_key: bool_option(config, screen_key, false)?,
            help_key: false,
            quit_key: bool_option(config, "option_quitkey", false)?,
            save_key: bool_option(config, "option_savekey", false)?,
            screenshot_key: bool_option(config, "option_screenshotkey", false)?,
            close_escape: bool_option(config, "option_closeesc", false)?,
            priority: int_option(config, "option_priority", 0)?,
            freeze: bool_option(config, "option_freeze", false)?,
            show_progress: false,
            load_transparent: bool_option(config, "option_loadtransparent", false)?,
            load_alpha: int_option(config, "option_loadalpha", 0)?,
            scale_progress: bool_option(config, "option_scaleprogress", false)?,
            display_errors: bool_option(config, "option_displayerrors", false)?,
            write_errors: bool_option(config, "option_writeerrors", false)?,
            abort_errors: bool_option(config, "option_aborterrors", false)?,
            // ApplyConfigSettings unconditionally forces this on after reading
            // the legacy option.
            variable_errors: true,
            creation_event_order: bool_option(config, "option_html5_CreationEventOrder", false)?,
            use_front_touch: false,
            use_rear_touch: false,
            use_fast_collision: bool_option(config, "option_use_fast_collision", false)?,
            fast_collision_compatibility: bool_option(
                config,
                "option_fast_collision_compatibility",
                false,
            )?,
            save_location: int_option(config, "option_windows_save_location", 0)?,
            studio_edition: 3,
            draw_color: u32::MAX,
            game_speed: float_option(config, "option_game_speed", 30.0)?,
            allow_statistics: bool_option(config, "option_allow_game_statistics", true)?,
        })
    }

    pub fn optn_flags(&self) -> u64 {
        let values = [
            self.full_screen,
            self.interpolate_pixels,
            self.use_new_audio,
            self.no_border,
            self.show_cursor,
            self.sizeable,
            self.stay_on_top,
            self.change_resolution,
            self.no_buttons,
            self.screen_key,
            self.help_key,
            self.quit_key,
            self.save_key,
            self.screenshot_key,
            self.close_escape,
            self.freeze,
            self.show_progress,
            self.load_transparent,
            self.scale_progress,
            self.display_errors,
            self.write_errors,
            self.abort_errors,
            self.variable_errors,
            self.creation_event_order,
            self.use_front_touch,
            self.use_rear_touch,
            self.use_fast_collision,
            self.fast_collision_compatibility,
        ];
        values
            .into_iter()
            .enumerate()
            .fold(0, |flags, (bit, set)| flags | (u64::from(set) << bit))
    }

    pub fn gen8_flags(&self) -> u32 {
        let mut flags = 0;
        flags |= u32::from(self.full_screen);
        flags |= u32::from((self.sync_vertex & 1) != 0) << 1;
        flags |= u32::from((self.sync_vertex as u32 & 0x8000_0000) != 0) << 2;
        flags |= u32::from(self.interpolate_pixels) << 3;
        flags |= u32::from(self.scale != 0) << 4;
        flags |= u32::from(self.show_cursor) << 5;
        flags |= u32::from(self.sizeable) << 6;
        flags |= u32::from(self.screen_key) << 7;
        flags |= u32::from((self.sync_vertex & 0x4000_0000) != 0) << 8;
        flags |= match self.studio_edition {
            1 => 0x200,
            2 => 0x400,
            3 => 0x800,
            4 => 0x600,
            _ => 0,
        };
        flags |= u32::from(self.save_location != 0) << 13;
        flags |= u32::from(self.borderless) << 14;
        flags
    }
}

#[derive(Debug, Clone)]
pub struct CompileSettings {
    pub options: GameOptions,
    pub constants: Vec<CompileConstant>,
    pub audio_groups: Vec<AudioGroupSettings>,
    pub texture_groups: Vec<TextureGroupSettings>,
    pub texture_page_size: i32,
    pub target_mask: i64,
    constant_index: HashMap<String, usize>,
}

impl CompileSettings {
    pub fn new(
        project: &ProjectManifest,
        config: &Config,
        sounds: &[Sound],
        rooms: &[Room],
        extensions: &[Extension],
    ) -> Result<Self, SettingsError> {
        let target_mask = DEFAULT_TARGET_MASK;
        let options = GameOptions::from_config(project, config)?;
        let (constants, constant_index) =
            merge_constants(project, config, rooms, extensions, target_mask);
        let audio_groups = merge_audio_groups(project, config, sounds)?;
        let texture_groups = merge_texture_groups(config)?;
        let texture_page_size = int_option(config, "option_windows_texture_page", 2048)?;
        if texture_page_size <= 0 || texture_page_size > i16::MAX.into() {
            return Err(SettingsError::InvalidOption {
                name: "option_windows_texture_page".to_owned(),
                value: texture_page_size.to_string(),
                expected: "an integer from 1 through 32767",
            });
        }
        Ok(Self {
            options,
            constants,
            audio_groups,
            texture_groups,
            texture_page_size,
            target_mask,
            constant_index,
        })
    }

    pub fn constant(&self, name: &str) -> Option<&CompileConstant> {
        self.constant_index
            .get(name)
            .map(|index| &self.constants[*index])
    }
}

fn merge_texture_groups(config: &Config) -> Result<Vec<TextureGroupSettings>, SettingsError> {
    let count =
        usize::try_from(int_option(config, "option_textureGroup_count", 0)?).map_err(|_| {
            SettingsError::InvalidOption {
                name: "option_textureGroup_count".to_owned(),
                value: config
                    .option("option_textureGroup_count")
                    .unwrap_or_default()
                    .to_owned(),
                expected: "a non-negative integer",
            }
        })?;
    if count == 0 {
        return Ok(vec![TextureGroupSettings {
            index: 0,
            name: "Default".to_owned(),
            scaled: false,
            border: 2,
            remove_space: false,
            target_mask: 0x7_ffff_ffff,
            parent: None,
            mips_to_generate: 0,
        }]);
    }

    let mut groups = Vec::with_capacity(count);
    for index in 0..count {
        let prefix = format!("option_textureGroup{index}");
        let name = config
            .option(&format!("option_textureGroups{index}"))
            .unwrap_or("Default")
            .to_owned();
        let parent = config
            .option(&format!("{prefix}_parent"))
            .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("<none>"))
            .map(str::to_owned);
        groups.push(TextureGroupSettings {
            index,
            name,
            scaled: bool_option(config, &format!("{prefix}_scaled"), false)?,
            border: int_option(config, &format!("{prefix}_border"), 2)?.max(0),
            remove_space: !bool_option(config, &format!("{prefix}_nocropping"), false)?,
            target_mask: mask_option(config, &format!("{prefix}_targets"), i64::MAX)?,
            parent,
            mips_to_generate: 0,
        });
    }
    Ok(groups)
}

fn merge_constants(
    project: &ProjectManifest,
    config: &Config,
    rooms: &[Room],
    extensions: &[Extension],
    target_mask: i64,
) -> (Vec<CompileConstant>, HashMap<String, usize>) {
    let mut constants = Vec::<CompileConstant>::new();
    let mut index = HashMap::<String, usize>::new();

    for instance in rooms.iter().flat_map(|room| &room.instances) {
        if !instance.name.is_empty() && !index.contains_key(&instance.name) {
            insert_constant(
                &mut constants,
                &mut index,
                &instance.name,
                instance.id.to_string(),
                ConstantSource::RoomInstance,
                false,
            );
        }
    }
    for constant in &project.constants {
        insert_constant(
            &mut constants,
            &mut index,
            &constant.name,
            constant.value.clone(),
            ConstantSource::Project,
            true,
        );
    }
    for constant in config.constants() {
        insert_constant(
            &mut constants,
            &mut index,
            &constant.name,
            constant.value.clone(),
            ConstantSource::Config,
            true,
        );
    }

    // Extension constants are a fallback namespace in the official compiler;
    // they never replace project/config constants and the first extension wins.
    for extension in extensions {
        if !extension.enabled_for(&config.name, target_mask) {
            continue;
        }
        for file in &extension.files {
            if !file.enabled_for(&config.name, target_mask) {
                continue;
            }
            for constant in &file.constants {
                insert_constant(
                    &mut constants,
                    &mut index,
                    &constant.name,
                    constant.value.clone(),
                    ConstantSource::Extension,
                    false,
                );
            }
        }
    }
    (constants, index)
}

fn insert_constant(
    constants: &mut Vec<CompileConstant>,
    index: &mut HashMap<String, usize>,
    name: &str,
    value: String,
    source: ConstantSource,
    replace: bool,
) {
    if let Some(position) = index.get(name).copied() {
        if replace {
            constants[position].value = value;
            constants[position].source = source;
        }
        return;
    }
    let position = constants.len();
    constants.push(CompileConstant {
        name: name.to_owned(),
        value,
        source,
    });
    index.insert(name.to_owned(), position);
}

fn merge_audio_groups(
    project: &ProjectManifest,
    config: &Config,
    sounds: &[Sound],
) -> Result<Vec<AudioGroupSettings>, SettingsError> {
    let configured_count = usize::try_from(int_option(config, "option_audioGroupCount", 0)?)
        .map_err(|_| SettingsError::InvalidOption {
            name: "option_audioGroupCount".to_owned(),
            value: config
                .option("option_audioGroupCount")
                .unwrap_or_default()
                .to_owned(),
            expected: "a non-negative integer",
        })?;
    let count = project.audio_groups.len().max(configured_count).max(1);
    let mut groups = Vec::with_capacity(count);
    for group_index in 0..count {
        let project_group = project.audio_groups.get(group_index);
        let target_mask = if group_index == 0 {
            i64::MAX
        } else {
            mask_option(
                config,
                &format!("option_audioGroup{group_index}_targets"),
                i64::MAX,
            )?
        };
        groups.push(AudioGroupSettings {
            index: group_index,
            name: project_group.map_or_else(
                || {
                    if group_index == 0 {
                        "audiogroup_default".to_owned()
                    } else {
                        format!("audiogroup{group_index}")
                    }
                },
                |group| group.name.clone(),
            ),
            target_mask,
            implicit: project_group.is_none(),
            sounds: Vec::new(),
        });
    }
    for sound in sounds {
        let group = usize::try_from(sound.group_index)
            .ok()
            .and_then(|index| groups.get_mut(index));
        let Some(group) = group else {
            return Err(SettingsError::InvalidAudioGroup {
                sound: sound.name.clone(),
                group: sound.group_index,
                count,
            });
        };
        group.sounds.push(sound.index);
    }
    Ok(groups)
}

fn bool_option(config: &Config, name: &str, default: bool) -> Result<bool, SettingsError> {
    let Some(value) = config.option(name) else {
        return Ok(default);
    };
    parse_bool(value).ok_or_else(|| SettingsError::InvalidOption {
        name: name.to_owned(),
        value: value.to_owned(),
        expected: "a boolean or integer",
    })
}

fn int_option(config: &Config, name: &str, default: i32) -> Result<i32, SettingsError> {
    let Some(value) = config.option(name) else {
        return Ok(default);
    };
    value
        .trim()
        .parse()
        .map_err(|_| SettingsError::InvalidOption {
            name: name.to_owned(),
            value: value.to_owned(),
            expected: "a 32-bit integer",
        })
}

fn int_or_bool_option(config: &Config, name: &str, default: i32) -> Result<i32, SettingsError> {
    let Some(value) = config.option(name) else {
        return Ok(default);
    };
    if let Ok(value) = value.trim().parse() {
        return Ok(value);
    }
    parse_bool(value)
        .map(i32::from)
        .ok_or_else(|| SettingsError::InvalidOption {
            name: name.to_owned(),
            value: value.to_owned(),
            expected: "a boolean or 32-bit integer",
        })
}

fn float_option(config: &Config, name: &str, default: f64) -> Result<f64, SettingsError> {
    let Some(value) = config.option(name) else {
        return Ok(default);
    };
    value
        .trim()
        .parse()
        .map_err(|_| SettingsError::InvalidOption {
            name: name.to_owned(),
            value: value.to_owned(),
            expected: "a number",
        })
}

fn mask_option(config: &Config, name: &str, default: i64) -> Result<i64, SettingsError> {
    let Some(value) = config.option(name) else {
        return Ok(default);
    };
    let value = value.trim();
    let parsed = if let Some(hex) = value.strip_prefix('$') {
        u64::from_str_radix(hex, 16).map(|value| value as i64)
    } else {
        value.parse::<i64>()
    };
    parsed.map_err(|_| SettingsError::InvalidOption {
        name: name.to_owned(),
        value: value.to_owned(),
        expected: "a decimal or $-prefixed hexadecimal target mask",
    })
}

fn parse_bool(value: &str) -> Option<bool> {
    let value = value.trim();
    if let Ok(value) = value.parse::<i64>() {
        return Some(value != 0);
    }
    if value.eq_ignore_ascii_case("true") {
        Some(true)
    } else if value.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsError {
    InvalidOption {
        name: String,
        value: String,
        expected: &'static str,
    },
    InvalidAudioGroup {
        sound: String,
        group: i32,
        count: usize,
    },
}

impl fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOption {
                name,
                value,
                expected,
            } => write!(
                formatter,
                "invalid config option {name}={value:?}; expected {expected}"
            ),
            Self::InvalidAudioGroup {
                sound,
                group,
                count,
            } => write!(
                formatter,
                "sound {sound:?} refers to audio group {group}, but only {count} groups exist"
            ),
        }
    }
}

impl Error for SettingsError {}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::config::Config;
    use crate::project::{ProjectConstant, ProjectManifest};

    use super::{CompileSettings, ConstantSource};

    #[test]
    fn merges_config_over_project_constants_and_matches_official_option_flags() {
        let path = temp_file();
        fs::write(
            &path,
            r#"<Config>
                <Options>
                  <option_gameid>42</option_gameid>
                  <option_use_new_audio>true</option_use_new_audio>
                  <option_showcursor>true</option_showcursor>
                  <option_sizeable>true</option_sizeable>
                  <option_screenkey>true</option_screenkey>
                  <option_quitkey>true</option_quitkey>
                  <option_savekey>true</option_savekey>
                  <option_screenshotkey>true</option_screenshotkey>
                  <option_closeesc>true</option_closeesc>
                  <option_scale>-1</option_scale>
                  <option_scaleprogress>true</option_scaleprogress>
                  <option_displayerrors>true</option_displayerrors>
                  <option_variableerrors>false</option_variableerrors>
                  <option_html5_CreationEventOrder>true</option_html5_CreationEventOrder>
                  <option_audioGroupCount>1</option_audioGroupCount>
                </Options>
                <ConfigConstants><constants>
                  <constant name="LIMIT">200</constant>
                </constants></ConfigConstants>
              </Config>"#,
        )
        .unwrap();
        let config = Config::load("Default", &path).unwrap();
        let project = ProjectManifest {
            name: "Tiny".to_owned(),
            project_file: PathBuf::from("Tiny.project.gmx"),
            root_dir: PathBuf::new(),
            resources: Vec::new(),
            data_files: Vec::new(),
            constants: vec![ProjectConstant {
                name: "LIMIT".to_owned(),
                value: "100".to_owned(),
            }],
            audio_groups: Vec::new(),
        };
        let settings = CompileSettings::new(&project, &config, &[], &[], &[]).unwrap();

        assert_eq!(settings.options.optn_flags(), 0x00cc_7a34);
        assert_eq!(settings.options.game_id, 42);
        assert_eq!(settings.constant("LIMIT").unwrap().value, "200");
        assert_eq!(
            settings.constant("LIMIT").unwrap().source,
            ConstantSource::Config
        );
        assert_eq!(settings.audio_groups.len(), 1);
        assert!(settings.audio_groups[0].implicit);
        fs::remove_file(path).unwrap();
    }

    fn temp_file() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "gmx-rs-settings-{}-{nonce}.config.gmx",
            std::process::id()
        ))
    }
}
