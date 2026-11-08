//! Byte-exact GPGL command encoders.
//!
//! Every function returns the exact bytes to write to bulk endpoint `0x01`.

/// End-of-text terminator used by every GPGL response and command.
pub const ETX: u8 = 0x03;
/// ESC control byte.
pub const ESC: u8 = 0x1b;
/// EOT control byte.
pub const EOT: u8 = 0x04;
/// ENQ control byte.
pub const ENQ: u8 = 0x05;
/// NAK control byte.
pub const NAK: u8 = 0x15;

/// Encode an ASCII command body and append ETX.
///
/// ```
/// assert_eq!(silhouette_driver::command("M200,200"), b"M200,200\x03");
/// ```
pub fn command(body: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 1);
    out.extend_from_slice(body.as_bytes());
    out.push(ETX);
    out
}

/// `ESC + EOT`: initialize the device. Silent, but mandatory before commands.
pub fn init() -> Vec<u8> {
    vec![ESC, EOT]
}

/// `ESC + ENQ`: query status. Response is one ASCII digit `0`..`4`.
pub fn status_query() -> Vec<u8> {
    vec![ESC, ENQ]
}

/// `ESC + NAK`: query tool codes (Cameo 4+). Response e.g. `" 2, 5"`.
pub fn tools_query() -> Vec<u8> {
    vec![ESC, NAK]
}

/// `FG`: firmware version. Response is a 17-char, space-padded string.
pub fn firmware_query() -> Vec<u8> {
    command("FG")
}

/// `U`: reported bounding area. **Semantics are not fully understood** — the
/// value is media/position dependent and has returned both plausible and
/// implausible numbers. Use only as a cross-check.
pub fn bounds_query() -> Vec<u8> {
    command("U")
}

/// `[`: upper-left origin. Observed to always return `0,0`; it is **not** a
/// live position readout.
pub fn origin_query() -> Vec<u8> {
    command("[")
}

/// `FN0`: reset / portrait initialisation.
pub fn reset() -> Vec<u8> {
    command("FN0")
}

/// `SO0`: set the current position as origin.
pub fn origin_current() -> Vec<u8> {
    command("SO0")
}

/// `TG{n}`: set cutting-mat type.
///
/// `0` none, `1` 12x12, `2` 12x24, `8` 15x15, `9` 24x24, `11` 8.5x12.
pub fn mat(kind: u8) -> Vec<u8> {
    command(&format!("TG{kind}"))
}

/// `J{n}`: select tool (0 = none).
pub fn select_tool(tool: u8) -> Vec<u8> {
    command(&format!("J{tool}"))
}

/// `!{speed},{tool}`: set speed.
pub fn speed(speed: u8, tool: u8) -> Vec<u8> {
    command(&format!("!{speed},{tool}"))
}

/// `FX{force},{tool}`: set force / pressure.
pub fn force(force: u8, tool: u8) -> Vec<u8> {
    command(&format!("FX{force},{tool}"))
}

/// `TJ{n}`: set acceleration.
pub fn acceleration(level: u8) -> Vec<u8> {
    command(&format!("TJ{level}"))
}

/// `TF{depth},{tool}`: set AutoBlade depth (0..10).
///
/// The AutoBlade must be reset with [`blade_reset`] first; the device then
/// clicks `10 - depth` times.
pub fn blade_depth(depth: u8, tool: u8) -> Vec<u8> {
    command(&format!("TF{depth},{tool}"))
}

/// `FY1`: reset the AutoBlade to its reference position (10 clicks).
pub fn blade_reset() -> Vec<u8> {
    command("FY1")
}

/// `FC{offset},{z},{tool}`: set cutter offset.
pub fn cutter_offset(offset: u16, z: u16, tool: u8) -> Vec<u8> {
    command(&format!("FC{offset},{z},{tool}"))
}

/// `FE{lift},{tool}`: lift between paths (1) or cut continuously (0).
pub fn lift_between_paths(lift: u8, tool: u8) -> Vec<u8> {
    command(&format!("FE{lift},{tool}"))
}

/// `L{n}`: line type (0 = solid).
pub fn line_type(kind: u8) -> Vec<u8> {
    command(&format!("L{kind}"))
}

/// `\{y},{x}`: lower-left corner of the cutting area.
pub fn lower_left(y: i32, x: i32) -> Vec<u8> {
    command(&format!("\\{y},{x}"))
}

/// `Z{y},{x}`: upper-right corner of the cutting area.
pub fn upper_right(y: i32, x: i32) -> Vec<u8> {
    command(&format!("Z{y},{x}"))
}

/// `FF{start},{end},{tool}`: corner sharpening, in tenths of a millimetre.
pub fn corner_sharpen(start: u8, end: u8, tool: u8) -> Vec<u8> {
    command(&format!("FF{start},{end},{tool}"))
}

/// `M{y},{x}`: move with the tool lifted.
pub fn move_to(y: i32, x: i32) -> Vec<u8> {
    command(&format!("M{y},{x}"))
}

/// `D{y},{x}`: move with the tool down (draw / cut).
pub fn draw_to(y: i32, x: i32) -> Vec<u8> {
    command(&format!("D{y},{x}"))
}

/// `H`: home. Documented as the end-of-job terminator for Cameo 1/2; on
/// Cameo 3+ the `L0 / \0,0 / M0,0 / J0 / FN0 / TB50,0` block is used instead.
pub fn home() -> Vec<u8> {
    command("H")
}

/// End-of-job block for Cameo 3+ (verified shape in the protocol reference;
/// only `J0` + `FN0` were exercised on hardware).
pub fn end_job() -> Vec<Vec<u8>> {
    vec![
        command("L0"),
        command("\\0,0"),
        command("M0,0"),
        command("J0"),
        command("FN0"),
        command("TB50,0"),
    ]
}

/// `TB50,{n}`: registration-mark orientation (`0` portrait, `1` landscape).
///
/// From the GPGL reference and the strings of `Silhouette Studio.exe`
/// (`TB50,` is emitted next to `TB99`/`TB52,2`/`TB51,`/…).
pub fn tb50(orientation: u8) -> Vec<u8> {
    command(&format!("TB50,{orientation}"))
}

/// `TB99`: use registration marks.
pub fn tb99() -> Vec<u8> {
    command("TB99")
}

/// `TB52,{n}`: registration-mark type (`0` original/SD, `2` Cameo).
pub fn tb52(kind: u8) -> Vec<u8> {
    command(&format!("TB52,{kind}"))
}

/// `TB51,{length_su}`: length of one arm of the L marks, in SU.
pub fn tb51(length_su: i32) -> Vec<u8> {
    command(&format!("TB51,{length_su}"))
}

/// `TB53,{width_su}`: mark stroke width, in SU.
pub fn tb53(width_su: i32) -> Vec<u8> {
    command(&format!("TB53,{width_su}"))
}

/// `TB55,{n}`: registration-mark enable (Studio sends `TB55,1`).
pub fn tb55(enable: u8) -> Vec<u8> {
    command(&format!("TB55,{enable}"))
}

/// `TB123,{h},{w},{top},{left}`: automatic registration-mark scan.
///
/// `h`/`w` are the distances between the marks; `top`/`left` are the scan
/// search start. All values are SU. The reply is a single ETX-terminated frame
/// whose first field is `0` on success.
pub fn tb123(height_su: i32, width_su: i32, top_su: i32, left_su: i32) -> Vec<u8> {
    command(&format!("TB123,{height_su},{width_su},{top_su},{left_su}"))
}

/// `TB124,{h},{w},{top},{left}`: automatic scan for **four-corner** marks.
///
/// Same parameters as [`tb123`]; used when the sheet has a fourth L at the
/// bottom-right. Only documented by the public reference, not exercised here.
pub fn tb124(height_su: i32, width_su: i32, top_su: i32, left_su: i32) -> Vec<u8> {
    command(&format!("TB124,{height_su},{width_su},{top_su},{left_su}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_commands_are_exact() {
        assert_eq!(tb50(0), b"TB50,0\x03");
        assert_eq!(tb99(), b"TB99\x03");
        assert_eq!(tb52(2), b"TB52,2\x03");
        assert_eq!(tb51(400), b"TB51,400\x03");
        assert_eq!(tb53(10), b"TB53,10\x03");
        assert_eq!(tb55(1), b"TB55,1\x03");
        assert_eq!(tb123(5540, 3800, 0, 0), b"TB123,5540,3800,0,0\x03");
    }
}
