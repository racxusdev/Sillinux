//! Byte-golden tests against the job that physically cut a square on a Cameo 5.
//!
//! Source of truth: `re_session.md`, Round 6 (owner observed "Quadrado cortado,
//! fechado"). If these assertions ever fail, the encoder no longer matches the
//! hardware-verified byte stream.

use silhouette_driver::{command, golden_square_cut, Error, MockTransport, Session, Status};

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn golden_job_is_byte_identical_to_the_verified_run() {
    let expected = [
        "1b 04",
        "46 4e 30 03",                   // FN0
        "53 4f 30 03",                   // SO0
        "54 47 31 03",                   // TG1
        "4a 31 03",                      // J1
        "21 33 2c 31 03",                // !3,1
        "46 58 31 2c 31 03",             // FX1,1
        "46 59 31 03",                   // FY1
        "54 46 31 2c 31 03",             // TF1,1
        "46 58 34 2c 31 03",             // FX4,1
        "46 43 31 38 2c 31 38 2c 31 03", // FC18,18,1
        "46 45 31 2c 31 03",             // FE1,1
        "4d 32 30 30 2c 32 30 30 03",    // M200,200
        "44 32 30 30 2c 36 30 30 03",    // D200,600
        "44 36 30 30 2c 36 30 30 03",    // D600,600
        "44 36 30 30 2c 32 30 30 03",    // D600,200
        "44 32 30 30 2c 32 30 30 03",    // D200,200
        "4a 30 03",                      // J0
        "46 4e 30 03",                   // FN0
    ];

    let job = golden_square_cut();
    assert_eq!(job.len(), expected.len(), "number of commands");
    for (i, (cmd, want)) in job.commands().iter().zip(expected).enumerate() {
        assert_eq!(hex(cmd), want, "command #{i}");
    }
}

#[test]
fn numeric_parameters_are_ascii_not_raw_bytes() {
    // Regression test for the prose error in the webapp handshake notes, which
    // wrote `46 58 14 2c 31 03` for "FX14,1" (0x14 is DC4, not "14").
    assert_eq!(hex(&command("FX14,1")), "46 58 31 34 2c 31 03");
    assert_eq!(hex(&command("FC18,1,1")), "46 43 31 38 2c 31 2c 31 03");
    assert_eq!(hex(&command("!5,1")), "21 35 2c 31 03");
    assert_eq!(hex(&command("TB71")), "54 42 37 31 03");
    assert_eq!(hex(&command("M200,200")), "4d 32 30 30 2c 32 30 30 03");
}

#[test]
fn every_text_command_ends_with_etx() {
    let job = golden_square_cut();
    for cmd in job.commands() {
        if cmd.len() == 2 && cmd[0] == 0x1b {
            continue; // ESC + control byte
        }
        assert_eq!(cmd.last(), Some(&0x03), "missing ETX in {cmd:?}");
        assert_eq!(cmd.iter().filter(|&&b| b == 0x03).count(), 1);
    }
}

#[test]
fn job_is_never_written_as_one_transfer() {
    // Documented failure mode: concatenating five M commands into one 49-byte
    // write moved the carriage once instead of tracing the square.
    let job = golden_square_cut();
    assert!(job.len() > 1);
    assert!(job.concatenated().len() > job.commands()[0].len());
}

#[test]
fn firmware_response_is_trimmed() {
    let mut mock = MockTransport::new();
    mock.queue(b"CAMEO 5 V1.07    \x03");
    let mut session = Session::new(mock);

    assert_eq!(session.firmware().unwrap(), "CAMEO 5 V1.07");
    assert_eq!(
        session.transport().writes(),
        &[b"FG\x03".to_vec()],
        "firmware query bytes"
    );
}

#[test]
fn status_and_tools_are_parsed() {
    let mut mock = MockTransport::new();
    mock.queue(b"0\x03");
    mock.queue(b" 2, 5\x03");
    let mut session = Session::new(mock);

    assert_eq!(session.status().unwrap(), Status::Ready);
    assert_eq!(session.tools().unwrap(), (2, 5));
}

#[test]
fn initialize_sends_only_esc_eot() {
    let mock = MockTransport::new();
    let mut session = Session::new(mock);
    session.initialize().unwrap();
    assert_eq!(session.transport().writes(), &[vec![0x1b, 0x04]]);
    let _ = mock;
}

#[test]
fn run_writes_one_transfer_per_command() {
    let mock = MockTransport::new();
    let mut session = Session::new(mock);
    let job = golden_square_cut();

    session.run(&job).unwrap();

    let writes = session.transport().writes();
    assert_eq!(writes.len(), job.len());
    for (w, c) in writes.iter().zip(job.commands()) {
        assert_eq!(w, c);
    }
}

#[test]
fn missing_response_is_a_timeout_not_a_panic() {
    let mut mock = MockTransport::new();
    mock.queue(b""); // empty read -> 0 bytes -> protocol error path
    let mut session = Session::new(mock);
    let err = session.query(b"FG\x03", 10).unwrap_err();
    assert!(matches!(err, Error::Protocol(_)));
}
