//! Function names registered by the GMS 1.4.9999 compiler but absent from the
//! mechanically extracted direct Runner registration table. This includes
//! indirect Runner registrations and target-specific compiler entries.
//! `gml_pragma` is omitted because project preprocessing handles it directly.

pub(crate) const NAMES: &[&str] = &[
    "action_splash_web",
    "background_replace_alpha",
    "buffer_set_network_safe",
    "gamepad_add_mapping_from_file",
    "gamepad_add_mapping_from_string",
    "gamepad_get_database",
    "logical_xor",
    "matchmaking_session_get_ping_info",
    "network_get_address",
    "network_receive_packet",
    "ps4_gamepad_reset_colour",
    "psn_init_leaderboard",
    "psn_np_commerce_dialog_open_on_product",
    "shader_set_uniform_i1",
    "sprite_replace_alpha",
    "sprite_save_strip",
    "switch_controller_support_set_identification_color",
    "switch_controller_support_set_identification_colour",
    "tile_delete_at",
    "tile_find",
    "uwp_was_closed_by_user",
    "xboxone_debug",
    "xboxone_product_show_details",
    "xboxone_was_closed_by_user",
];

pub(crate) fn names() -> impl Iterator<Item = &'static str> {
    NAMES.iter().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_has_unique_names() {
        assert!(NAMES.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
