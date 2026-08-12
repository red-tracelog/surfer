//! "Go to value" search logic for the focused variable.
//!
//! See [`SystemState::handle_goto_value`].

use num::BigUint;
use num::bigint::ToBigInt as _;
use num::{One as _, Zero as _};
use surfer_translation_types::VariableValue;
use tracing::{info, warn};

use crate::SystemState;
use crate::displayed_item::{DisplayedFieldRef, DisplayedItem};
use crate::translation::TranslationResultExt;

impl SystemState {
    /// Handle `Message::GoToValue`.
    ///
    /// Search forward (or backward, if `next` is false) from the cursor for the
    /// next transition where the focused variable takes the given (displayed)
    /// value, and move the cursor there.
    pub(crate) fn handle_goto_value(&mut self, value: &str, next: bool) {
        // Compute time formatting info before mutably borrowing waves.
        let wanted_timeunit = self.user.wanted_timeunit;
        let time_format = self.get_time_format();

        let Some(waves) = self.user.waves.as_mut() else {
            return;
        };
        if waves.max_timestamp().is_none() {
            warn!("Go to value: No timestamps count, even though waveforms should be loaded");
            return;
        }

        let Some(focused) = waves.focused_item else {
            warn!("Go to value: no focused item");
            return;
        };
        let Some(node) = waves.items_tree.get_visible(focused) else {
            return;
        };
        let item_ref = node.item_ref;
        let DisplayedItem::Variable(displayed_variable) = &waves.displayed_items[&item_ref] else {
            warn!("Go to value: focused item is not a variable");
            return;
        };
        let variable_ref = displayed_variable.variable_ref.clone();

        let wave_container = waves.inner.as_waves().unwrap();
        let meta = match wave_container.variable_meta(&variable_ref) {
            Ok(m) => m,
            Err(e) => {
                warn!("Go to value: failed to get variable meta: {e}");
                return;
            }
        };
        let displayed_field_ref: DisplayedFieldRef = item_ref.into();
        let translators_ref = &self.translators;
        let translator = waves.variable_translator_with_meta(
            &displayed_field_ref.without_field(),
            translators_ref,
            &meta,
        );

        // Start searching from the current cursor, or from time 0 if no
        // cursor is set.  We look for the *next* (or *previous*) transition
        // whose value matches, so the current position is the starting
        // point, not a candidate.
        let start_time: BigUint = waves
            .cursor
            .as_ref()
            .and_then(|c| c.to_biguint())
            .unwrap_or_default();

        let cursor_was_set = waves.cursor.is_some();
        let target = value.trim().to_lowercase();
        let field_path = &displayed_field_ref.field;

        // Helper: does `val` match the user-provided target string?
        let matches_value = |val: &VariableValue| -> bool {
            // 1. Compare against the translated display string (what the
            //    user sees in the value column).
            if let Ok(translated) = translator.translate(&meta, val) {
                let fields = translated.format_flat(
                    &displayed_variable.format,
                    &displayed_variable.field_formats,
                    translators_ref,
                );
                let subfield = fields.iter().find(|res| res.names == *field_path);
                if let Some(res) = subfield
                    && let Some(tv) = &res.value
                {
                    if tv.value.trim().to_lowercase() == target {
                        return true;
                    }
                }
            }
            // 2. Fallback: compare against the raw value.  For BigUint,
            //    accept decimal, hex (0x), binary (0b) and octal (0o)
            //    representations.  For String values, compare directly.
            match val {
                VariableValue::BigUint(v) => {
                    if let Some(parsed) = parse_biguint(&target) {
                        return parsed == *v;
                    }
                }
                VariableValue::String(s) => {
                    let s_lower = s.trim().to_lowercase();
                    // Exact match on the full raw value (e.g. "xxxx"
                    // for a 4-bit all-x signal).
                    if s_lower == target {
                        return true;
                    }
                    // Single-character special-value codes
                    // (x, z, h, l, u, w, -): match if the raw
                    // per-bit value contains that character, so the
                    // user can type `x` to find a multi-bit `x1x0`.
                    if target.len() == 1
                        && matches!(
                            target.chars().next(),
                            Some('x' | 'z' | 'h' | 'l' | 'u' | 'w' | '-')
                        )
                        && s_lower.contains(&target)
                    {
                        return true;
                    }
                }
            }
            false
        };

        let found_time: Option<BigUint> = search_transitions(
            wave_container,
            &variable_ref,
            &start_time,
            next,
            cursor_was_set,
            &matches_value,
        );

        if let Some(time) = found_time {
            let timescale = waves.inner.metadata().timescale.clone();
            let time_bigint = time.to_bigint().unwrap_or_default();
            let time_str =
                crate::time::time_string(&time_bigint, &timescale, &wanted_timeunit, &time_format);
            info!("Go to value: found '{value}' at {time_str}");
            waves.cursor = Some(time_bigint);
            waves.go_to_cursor_if_not_in_view();
            self.invalidate_draw_commands();
        } else {
            let direction = if next { "next" } else { "previous" };
            warn!(
                "Go to value: value '{value}' not found in {direction} direction for focused variable"
            );
        }
    }
}

/// Walk through transitions in the given direction and return the first
/// matching time.
///
/// When searching forward (`next` is true) and no cursor was set, the value
/// at the current position (time 0) is checked as a candidate before walking.
/// When a cursor was set, the search starts from the *next* transition after
/// it.  When searching backward, the search steps through transitions in
/// reverse from the current position.
fn search_transitions(
    wave_container: &crate::wave_container::WaveContainer,
    variable_ref: &crate::wave_container::VariableRef,
    start_time: &BigUint,
    next: bool,
    cursor_was_set: bool,
    matches_value: &impl Fn(&VariableValue) -> bool,
) -> Option<BigUint> {
    let mut search_time = start_time.clone();
    let mut found = None;

    // When searching forward with no cursor set, the current position
    // (time 0) is a valid candidate — check it before walking.  When a
    // cursor is set, we want the *next* match after it, so we skip the
    // current value.  Backward search never checks the current position.
    if next && !cursor_was_set {
        if let Ok(Some(res)) = wave_container.query_variable(variable_ref, &search_time) {
            if let Some((curr_time, curr_val)) = &res.current
                && matches_value(curr_val)
            {
                found = Some(curr_time.clone());
            }
        }
    }

    if found.is_none() {
        for _ in 0..u32::MAX {
            let Ok(Some(res)) = wave_container.query_variable(variable_ref, &search_time) else {
                break;
            };

            let candidate_time = if next {
                // Forward: jump to the next transition.
                let Some(next_time) = res.next else {
                    break;
                };
                next_time
            } else {
                // Backward: get the current value's start time, then step
                // back one tick to enter the previous value's interval.
                let Some((curr_time, _)) = &res.current else {
                    break;
                };
                if curr_time.is_zero() {
                    break;
                }
                curr_time - BigUint::one()
            };

            let Ok(Some(candidate_res)) =
                wave_container.query_variable(variable_ref, &candidate_time)
            else {
                break;
            };
            let Some((candidate_time, candidate_val)) = &candidate_res.current else {
                break;
            };
            if matches_value(candidate_val) {
                found = Some(candidate_time.clone());
                break;
            }
            search_time = candidate_time.clone();
        }
    }

    found
}

/// Parse a string into a [`BigUint`], supporting decimal, hex (`0x`), binary
/// (`0b`) and octal (`0o`) prefixes.  Used by the "go to value" search to match
/// raw numeric values.
fn parse_biguint(s: &str) -> Option<BigUint> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Strip a leading + sign.
    let s = s.strip_prefix('+').unwrap_or(s);
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        BigUint::parse_bytes(hex.as_bytes(), 16)
    } else if let Some(bin) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        BigUint::parse_bytes(bin.as_bytes(), 2)
    } else if let Some(oct) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
        BigUint::parse_bytes(oct.as_bytes(), 8)
    } else {
        BigUint::parse_bytes(s.as_bytes(), 10)
    }
}
