//! E6.4 — Operator-console serial framing (microVM Phase 0).
//!
//! The bare-metal kernel has no network listener yet (smoltcp runs only a
//! loopback demo; real virtio-net is future work). Until then, the operator
//! console rides the channel the kernel *already* has to the host: the COM1
//! serial line. This module frames the shared [`console_proto`] wire protocol
//! onto that line so the **same** dashboard and TUI that drive the container
//! surface also work against a microVM, via the host-side
//! `anima-console serial` bridge.
//!
//! - **Egress (agent → operator):** each [`OperatorEvent`] is written as
//!   `ANIMA_TLM <ndjson>` using the dependency-free [`OperatorEvent::to_ndjson`]
//!   writer — no `serde_json` is linked into the kernel.
//! - **Ingress (operator → agent):** lines arriving on COM1 RX as
//!   `ANIMA_IN <ndjson>` are decoded with [`parse_input_line`], the kernel's
//!   `no_std` JSON scanner.
//!
//! # Phase 1 (future)
//!
//! Once virtio-net lands, the identical [`console_proto`] messages run over the
//! existing smoltcp + TLS 1.3 stack and the operator connects straight to the
//! microVM — no host bridge. Only the transport changes; this protocol does not.
//!
//! # Exit criterion
//!
//! Writes `E6.4_CONSOLE_DONE` to COM1 via the `serial` callback. Mirrors the
//! `sleep_soak` module's structure so the boot task can drive it the same way.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use console_proto::{parse_input_line, OperatorEvent, OperatorInput, Priority, TELEMETRY_PREFIX};

/// Frame one event onto the serial line as `ANIMA_TLM <ndjson>\n`.
///
/// `serial` is the same COM1 write callback used throughout the kernel; it
/// expands `\n` to `\r\n` for us.
pub fn emit(serial: &impl Fn(&str), event: &OperatorEvent) {
    serial(TELEMETRY_PREFIX);
    serial(&event.to_ndjson());
    serial("\n");
}

/// Drive the Phase-0 operator-console demonstration.
///
/// Emits a representative telemetry burst and exercises the inbound guidance
/// parser, then writes the `E6.4_CONSOLE_DONE` marker the CI boot job asserts.
pub fn run_operator_console_demo(serial: impl Fn(&str)) -> Result<(), &'static str> {
    serial("\n[E6.4] operator_console: serial telemetry/guidance framing\n");

    // ── Egress — emit telemetry exactly as the host bridge will consume it ──
    let events = [
        OperatorEvent::State {
            lifecycle: String::from("Awake"),
            sleep_phase: None,
            agenda_depth: 1,
        },
        OperatorEvent::Vitals {
            thermal_load: 0.12,
            compute_pressure: 0.30,
            memory_pressure: 0.20,
            power_budget: 1.0,
            financial_budget: 1.0,
            attention_demand: 0.5,
            aggregate_stress: 0.18,
        },
        OperatorEvent::AgentMessage {
            task_id: 1,
            tokens: 7,
            text: String::from("microVM operator console online"),
        },
    ];
    for ev in &events {
        emit(&serial, ev);
    }

    // ── Ingress — decode a guidance line as it would arrive on COM1 RX ──────
    // CI has no live serial input, so feed a representative `ANIMA_IN` line to
    // exercise the no_std parser deterministically. The live path is
    // [`poll_guidance`], driven byte-by-byte by the boot loop when virtio/host
    // input is wired.
    let inbound = r#"ANIMA_IN {"text":"reduce batch size","priority":"High"}"#;
    match parse_input_line(inbound) {
        Some(input) => {
            serial(&format!(
                "[E6.4] parsed guidance: priority={} text={:?}\n",
                input.priority.as_str(),
                input.text
            ));
            if input.priority != Priority::High {
                return Err("inbound guidance priority mis-parsed");
            }
            if input.text != "reduce batch size" {
                return Err("inbound guidance text mis-parsed");
            }
        }
        None => return Err("failed to parse inbound guidance line"),
    }

    serial("E6.4_CONSOLE_DONE: operator-console serial framing complete\n");
    Ok(())
}

/// Live inbound path for the host bridge: feed bytes as they arrive on COM1 RX.
///
/// Returns `Some(OperatorInput)` once a complete `\n`-terminated line has been
/// accumulated and successfully parsed; `None` while a line is still in flight
/// or the completed line was not valid guidance. `\r` is ignored so `\r\n`
/// framing works. The caller owns the `line` accumulator across calls.
///
/// Not yet wired into the boot loop (CI has no live serial input); it is the
/// real ingress the host bridge / Phase-1 virtio path will drive.
#[allow(dead_code)]
pub fn poll_guidance(line: &mut Vec<u8>, byte: u8) -> Option<OperatorInput> {
    match byte {
        b'\n' => {
            let parsed = core::str::from_utf8(line).ok().and_then(parse_input_line);
            line.clear();
            parsed
        }
        b'\r' => None,
        _ => {
            line.push(byte);
            None
        }
    }
}
